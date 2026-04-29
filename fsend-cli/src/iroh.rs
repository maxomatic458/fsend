use std::path::Path;

use async_compression::tokio::bufread::GzipDecoder;
use async_compression::tokio::write::GzipEncoder;
use async_trait::async_trait;
use bincode::{Decode, Encode};
use iroh::{Endpoint, NodeAddr, RelayMode, SecretKey};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::relay::ConnectionInfo;
use crate::transfer::{self, *};

// --- Packet I/O ---

async fn send_packet<P: Encode + std::fmt::Debug>(
    packet: P,
    conn: &iroh::endpoint::Connection,
) -> std::io::Result<()> {
    tracing::debug!("sending packet: {:?}", packet);
    let mut send = conn.open_uni().await?;
    let data = bincode::encode_to_vec(&packet, bincode::config::standard()).unwrap();
    let compressed = compress_gzip(&data).await?;
    send.write_all(&compressed).await?;
    send.flush().await?;
    send.finish()?;
    Ok(())
}

async fn receive_packet<P: Decode<()> + std::fmt::Debug>(
    conn: &iroh::endpoint::Connection,
) -> Result<P, PacketRecvError> {
    let mut recv = conn.accept_uni().await?;
    let mut buf = Vec::new();
    let mut tmp = vec![0u8; 4096];
    loop {
        match recv.read(&mut tmp).await? {
            Some(n) => buf.extend_from_slice(&tmp[..n]),
            None => break,
        }
    }
    let decompressed = decompress_gzip(&buf).await?;
    let (packet, _) = bincode::decode_from_slice(&decompressed, bincode::config::standard())?;
    tracing::debug!("received packet: {:?}", packet);
    Ok(packet)
}

use transfer::compress_gzip;
use transfer::decompress_gzip;

// --- File data streaming ---

async fn send_file_data<S, R>(
    send: &mut S,
    file: &mut R,
    skip: u64,
    size: u64,
    write_cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    S: AsyncWriteExt + Unpin,
    R: AsyncReadExt + AsyncSeekExt + Unpin,
{
    file.seek(std::io::SeekFrom::Start(skip)).await?;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut read = skip;
    while read < size {
        let to_read = std::cmp::min(BUF_SIZE as u64, size - read) as usize;
        let n = file.read_exact(&mut buf[..to_read]).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected eof"));
        }
        send.write_all(&buf[..n]).await?;
        read += n as u64;
        write_cb(n as u64);
    }
    Ok(())
}

fn send_directory<S>(
    send: &mut S,
    root: &Path,
    files: &[FileSendRecvTree],
    cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    S: AsyncWriteExt + Unpin + Send,
{
    for file in files {
        match file {
            FileSendRecvTree::File { name, skip, size } => {
                let path = root.join(name);
                tokio::task::block_in_place(|| {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let mut f = tokio::fs::File::open(&path).await?;
                        send_file_data(send, &mut f, *skip, *size, cb).await
                    })
                })?;
            }
            FileSendRecvTree::Dir { name, files } => {
                send_directory(send, &root.join(name), files, cb)?;
            }
        }
    }
    Ok(())
}

async fn receive_file_data<R, W>(
    recv: &mut R,
    file: &mut W,
    skip: u64,
    size: u64,
    read_cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + AsyncSeekExt + Unpin,
{
    file.seek(std::io::SeekFrom::Start(skip)).await?;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut written = skip;
    while written < size {
        let to_read = std::cmp::min(BUF_SIZE as u64, size - written) as usize;
        let n = recv.read_exact(&mut buf[..to_read]).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected eof"));
        }
        file.write_all(&buf[..n]).await?;
        written += n as u64;
        read_cb(n as u64);
    }
    Ok(())
}

fn receive_directory<R>(
    recv: &mut R,
    root: &Path,
    files: &[FileSendRecvTree],
    cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin + Send,
{
    for file in files {
        match file {
            FileSendRecvTree::File { name, skip, size } => {
                let path = root.join(name);
                tokio::task::block_in_place(|| {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let mut f = tokio::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .open(&path)
                            .await?;
                        receive_file_data(recv, &mut f, *skip, *size, cb).await?;
                        f.sync_all().await?;
                        f.shutdown().await?;
                        Ok::<(), std::io::Error>(())
                    })
                })?;
            }
            FileSendRecvTree::Dir { name, files } => {
                let dir = root.join(name);
                if !dir.exists() {
                    std::fs::create_dir(&dir)?;
                }
                receive_directory(recv, &dir, files, cb)?;
            }
        }
    }
    Ok(())
}

// --- IrohTransfer ---

pub struct IrohTransfer {
    conn: iroh::endpoint::Connection,
    endpoint: Endpoint,
}

impl IrohTransfer {
    pub fn connection_info_from_node_addr(addr: &NodeAddr) -> ConnectionInfo {
        let addrs: Vec<String> = addr
            .direct_addresses
            .iter()
            .map(|a| a.to_string())
            .collect();
        ConnectionInfo::Iroh {
            node_id: addr.node_id.to_string(),
            addrs,
        }
    }

    pub fn node_addr_from_connection_info(info: &ConnectionInfo) -> Result<NodeAddr, TransferError> {
        match info {
            ConnectionInfo::Iroh { node_id, addrs } => {
                let node_id = node_id
                    .parse::<iroh::NodeId>()
                    .map_err(|e| TransferError::Iroh(format!("invalid node_id: {e}")))?;
                let direct_addresses: std::collections::BTreeSet<std::net::SocketAddr> = addrs
                    .iter()
                    .filter_map(|a| a.parse().ok())
                    .collect();
                Ok(NodeAddr { node_id, relay_url: None, direct_addresses })
            }
            _ => Err(TransferError::Iroh("expected iroh connection info".into())),
        }
    }

    pub async fn create_endpoint() -> Result<Endpoint, TransferError> {
        let secret_key = SecretKey::generate(rand::rngs::OsRng);
        Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![FSEND_ALPN.to_vec()])
            .relay_mode(RelayMode::Default)
            .bind()
            .await
            .map_err(|e| TransferError::Iroh(e.to_string()))
    }

    pub async fn accept(endpoint: Endpoint) -> Result<Self, TransferError> {
        let incoming = endpoint.accept().await.ok_or_else(|| {
            TransferError::Iroh("endpoint closed while waiting for connection".into())
        })?;
        let connecting = incoming.accept()?;
        let conn = connecting.await?;
        tracing::info!("receiver connected to sender");
        Ok(Self { conn, endpoint })
    }

    pub async fn connect(endpoint: Endpoint, peer_info: ConnectionInfo) -> Result<Self, TransferError> {
        let node_addr = Self::node_addr_from_connection_info(&peer_info)?;
        let conn = endpoint
            .connect(node_addr, FSEND_ALPN)
            .await
            .map_err(|e| TransferError::Iroh(e.to_string()))?;
        tracing::info!("connected to sender");
        Ok(Self { conn, endpoint })
    }
}

#[async_trait]
impl Transfer for IrohTransfer {
    async fn connection_type_name(&self) -> String {
        let conn_type = match self.conn.remote_node_id() {
            Ok(node_id) => self.endpoint.conn_type(node_id).ok().and_then(|c| c.get().ok()),
            Err(_) => None,
        };
        match conn_type {
            Some(iroh::endpoint::ConnectionType::Direct(_)) => "Direct".into(),
            Some(iroh::endpoint::ConnectionType::Relay(_)) => "Relay".into(),
            Some(iroh::endpoint::ConnectionType::Mixed(_, _)) => "Mixed".into(),
            Some(iroh::endpoint::ConnectionType::None) => "None".into(),
            None => "Unknown".into(),
        }
    }

    async fn send_files(
        &mut self,
        args: SendArgs,
        initial_progress_cb: ProgressCb<'_>,
        waiting_cb: WaitingCb<'_>,
        write_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        send_packet(
            SenderToReceiver::ConnRequest { version: PROTO_VERSION.to_string() },
            &self.conn,
        ).await?;

        match receive_packet::<ReceiverToSender>(&self.conn).await? {
            ReceiverToSender::Ok => {}
            ReceiverToSender::WrongVersion { expected } => {
                return Err(TransferError::WrongVersion(expected, PROTO_VERSION.into()));
            }
            _ => return Err(TransferError::UnexpectedPacket),
        }

        let mut files_available = Vec::new();
        for path in &args.files {
            if !path.exists() {
                return Err(TransferError::FileNotFound(path.clone()));
            }
            files_available.push(get_files_available(path)?);
        }

        send_packet(
            SenderToReceiver::FileInfo { files: files_available.clone() },
            &self.conn,
        ).await?;

        waiting_cb();

        let to_skip = match receive_packet::<ReceiverToSender>(&self.conn).await? {
            ReceiverToSender::AcceptFilesSkip { files } => files,
            ReceiverToSender::RejectFiles => return Err(TransferError::FilesRejected),
            _ => return Err(TransferError::UnexpectedPacket),
        };

        let to_send: Vec<Option<FileSendRecvTree>> = files_available.iter()
            .zip(&to_skip)
            .map(|(file, skip)| match skip {
                Some(s) => file.remove_skipped(s),
                None => Some(file.to_send_recv_tree()),
            })
            .collect();

        let progress: Vec<(String, u64, u64)> = files_available.iter()
            .zip(&to_skip)
            .map(|(file, skip)| {
                (file.name().to_string(), skip.as_ref().map(|s| s.skip()).unwrap_or(0), file.size())
            })
            .collect();

        initial_progress_cb(&progress);

        let uni = self.conn.open_uni().await?;
        let mut send = GzipEncoder::new(uni);

        for (path, file) in args.files.iter().zip(to_send) {
            if let Some(file) = file {
                match file {
                    FileSendRecvTree::File { skip, size, .. } => {
                        let mut f = tokio::fs::File::open(path).await?;
                        send_file_data(&mut send, &mut f, skip, size, write_cb).await?;
                    }
                    FileSendRecvTree::Dir { files, .. } => {
                        send_directory(&mut send, path, &files, write_cb)?;
                    }
                }
            }
        }

        send.shutdown().await?;
        self.conn.closed().await;
        Ok(())
    }

    async fn receive_files(
        &mut self,
        args: ReceiveArgs,
        initial_progress_cb: ProgressCb<'_>,
        accept_files_cb: AcceptFilesCb<'_>,
        read_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        match receive_packet::<SenderToReceiver>(&self.conn).await? {
            SenderToReceiver::ConnRequest { version } => {
                if version != PROTO_VERSION {
                    send_packet(
                        ReceiverToSender::WrongVersion { expected: PROTO_VERSION.into() },
                        &self.conn,
                    ).await?;
                    return Err(TransferError::WrongVersion(PROTO_VERSION.into(), version));
                }
                send_packet(ReceiverToSender::Ok, &self.conn).await?;
            }
            _ => return Err(TransferError::UnexpectedPacket),
        }

        let files_offered = match receive_packet::<SenderToReceiver>(&self.conn).await? {
            SenderToReceiver::FileInfo { files } => files,
            _ => return Err(TransferError::UnexpectedPacket),
        };

        let output_path = match accept_files_cb(&files_offered) {
            Some(p) => p,
            None => {
                send_packet(ReceiverToSender::RejectFiles, &self.conn).await?;
                self.conn.closed().await;
                return Err(TransferError::FilesRejected);
            }
        };

        let files_to_skip: Vec<Option<FilesToSkip>> = if args.resume {
            files_offered.iter().map(|offered| {
                let local_path = output_path.join(offered.name());
                get_files_available(&local_path).ok().and_then(|local| offered.get_skippable(&local))
            }).collect()
        } else {
            vec![None; files_offered.len()]
        };

        let to_receive: Vec<Option<FileSendRecvTree>> = files_offered.iter()
            .zip(&files_to_skip)
            .map(|(offered, skip)| match skip {
                Some(s) => offered.remove_skipped(s),
                None => Some(offered.to_send_recv_tree()),
            })
            .collect();

        let progress: Vec<(String, u64, u64)> = files_offered.iter()
            .zip(&files_to_skip)
            .map(|(offered, skip)| {
                (offered.name().to_string(), skip.as_ref().map(|s| s.skip()).unwrap_or(0), offered.size())
            })
            .collect();

        initial_progress_cb(&progress);

        send_packet(
            ReceiverToSender::AcceptFilesSkip { files: files_to_skip },
            &self.conn,
        ).await?;

        let recv_stream = self.conn.accept_uni().await?;
        let mut recv = GzipDecoder::new(tokio::io::BufReader::with_capacity(BUF_SIZE, recv_stream));

        for file in to_receive.into_iter().flatten() {
            match file {
                FileSendRecvTree::File { name, skip, size } => {
                    let path = output_path.join(&name);
                    let mut f = tokio::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .open(&path)
                        .await?;
                    receive_file_data(&mut recv, &mut f, skip, size, read_cb).await?;
                    f.sync_all().await?;
                    f.shutdown().await?;
                }
                FileSendRecvTree::Dir { name, files } => {
                    let dir = output_path.join(&name);
                    if !dir.exists() {
                        std::fs::create_dir(&dir)?;
                    }
                    receive_directory(&mut recv, &dir, &files, read_cb)?;
                }
            }
        }

        self.close().await;
        Ok(())
    }

    async fn close(&mut self) {
        self.conn.close(0u32.into(), &[0]);
        self.endpoint.close().await;
    }
}
