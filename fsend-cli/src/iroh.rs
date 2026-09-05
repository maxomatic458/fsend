use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use iroh::endpoint::{presets, Connection, ConnectionError, StoppedError, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr, Watcher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::relay::ConnectionInfo;
use crate::transfer::*;

const MAX_PACKET_SIZE: usize = 512 * 1024 * 1024; // 512 MiB is probably enough

/// Resolving Home relay and direct address timeout.
const ADDR_TIMEOUT: Duration = Duration::from_secs(15);

// Initial peer connection timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

const CLOSE_OK: u32 = 0;
const CLOSE_ERR: u32 = 1;

/// Sends one control packet on its own unidirectional stream (they are very cheap to use).
/// The packet is gzip compressed.
async fn send_packet<P: WirePacket>(packet: &P, conn: &Connection) -> Result<(), TransferError> {
    tracing::debug!("sending packet: {:?}", packet);
    let data = packet.to_wire().map_err(PacketError::Encode)?;
    let compressed = compress_gzip(&data).await?;
    let mut send = conn.open_uni().await?;
    send.write_all(&compressed)
        .await
        .map_err(PacketError::Write)?;
    send.finish().map_err(|_| PacketError::Stopped)?;
    match send.stopped().await {
        Ok(_) => Ok(()),
        Err(StoppedError::ConnectionLost(e)) => Err(TransferError::Connection(e)),
        Err(_) => Err(PacketError::Stopped.into()),
    }
}

/// Receives one control packet on its own unidirectional stream (they are very cheap to use).
/// The packet is gzip compressed.
async fn receive_packet<P: WirePacket>(conn: &Connection) -> Result<P, TransferError> {
    let mut recv = conn.accept_uni().await?;
    let compressed = recv
        .read_to_end(MAX_PACKET_SIZE)
        .await
        .map_err(PacketError::Read)?;
    let data = decompress_gzip(&compressed)
        .await
        .map_err(PacketError::Io)?;
    let packet = P::from_wire(&data).map_err(PacketError::Decode)?;
    tracing::debug!("received packet: {:?}", packet);
    Ok(packet)
}

/// Copies `skip..size` of `file` into `send`.
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
        let n = file.read(&mut buf[..to_read]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected eof",
            ));
        }
        send.write_all(&buf[..n]).await?;
        read += n as u64;
        write_cb(n as u64);
    }
    Ok(())
}

/// Reads `skip..size` from `recv` into `file`.
async fn receive_file_data<R, W>(
    recv: &mut R,
    file: &mut W,
    skip: u64,
    size: u64,
    read_cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = vec![0u8; BUF_SIZE];
    let mut written = skip;
    while written < size {
        let to_read = std::cmp::min(BUF_SIZE as u64, size - written) as usize;
        let n = recv.read(&mut buf[..to_read]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the stream ended before the file was complete",
            ));
        }
        file.write_all(&buf[..n]).await?;
        written += n as u64;
        read_cb(n as u64);
    }
    Ok(())
}

use tokio::io::AsyncSeekExt;

async fn send_entries<S>(
    send: &mut S,
    entries: &[TransferEntry],
    write_cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    S: AsyncWriteExt + Unpin,
{
    for entry in entries {
        if let TransferEntry::File { path, skip, size } = entry {
            let mut file = tokio::fs::File::open(path).await?;
            send_file_data(send, &mut file, *skip, *size, write_cb).await?;
        }
    }
    Ok(())
}

async fn receive_entries<R>(
    recv: &mut R,
    entries: &[TransferEntry],
    read_cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin,
{
    for entry in entries {
        match entry {
            TransferEntry::Dir { path } => tokio::fs::create_dir_all(path).await?,
            TransferEntry::File { path, skip, size } => {
                let mut file = open_for_receive(path, *skip).await?;
                receive_file_data(recv, &mut file, *skip, *size, read_cb).await?;
                finish_received_file(&mut file, *size).await?;
            }
        }
    }
    Ok(())
}

pub struct IrohTransfer {
    conn: Connection,
    endpoint: Endpoint,
}

impl IrohTransfer {
    /// An iroh endpoint with the n0 defaults
    pub async fn create_endpoint() -> Result<Endpoint, TransferError> {
        Self::bind(presets::N0).await
    }

    pub async fn bind(preset: impl presets::Preset) -> Result<Endpoint, TransferError> {
        Ok(Endpoint::builder(preset)
            .alpns(vec![FSEND_ALPN.to_vec()])
            .bind()
            .await?)
    }

    /// The address to hand to the relay for the other peer.
    pub async fn local_connection_info(endpoint: &Endpoint) -> ConnectionInfo {
        let mut watcher = endpoint.watch_addr();
        let ready = async {
            loop {
                let addr = watcher.get();
                if addr.relay_urls().next().is_some() && addr.ip_addrs().next().is_some() {
                    return;
                }
                if watcher.updated().await.is_err() {
                    return;
                }
            }
        };

        if tokio::time::timeout(ADDR_TIMEOUT, ready).await.is_err() {
            tracing::warn!("sharing the address before the relay was reached");
        }

        Self::connection_info_from_addr(&endpoint.addr())
    }

    /// Convert the [`EndpointAddr`] into a [`ConnectionInfo`]
    pub fn connection_info_from_addr(addr: &EndpointAddr) -> ConnectionInfo {
        let addrs = addr
            .addrs
            .iter()
            .filter_map(|a| match a {
                TransportAddr::Ip(addr) => Some(addr.to_string()),
                TransportAddr::Relay(url) => Some(url.to_string()),
                _ => None,
            })
            .collect();
        ConnectionInfo::Iroh {
            node_id: addr.id.to_string(),
            addrs,
        }
    }

    pub fn addr_from_connection_info(info: &ConnectionInfo) -> Result<EndpointAddr, TransferError> {
        let ConnectionInfo::Iroh { node_id, addrs } = info else {
            return Err(TransferError::UnexpectedConnectionInfo);
        };
        let id = node_id.parse::<EndpointId>()?;
        let addrs = addrs.iter().filter_map(|a| {
            if let Ok(addr) = a.parse::<SocketAddr>() {
                Some(TransportAddr::Ip(addr))
            } else if let Ok(url) = a.parse::<RelayUrl>() {
                Some(TransportAddr::Relay(url))
            } else {
                tracing::warn!("ignoring unparsable peer address {a:?}");
                None
            }
        });
        Ok(EndpointAddr::from_parts(id, addrs))
    }

    /// Waits for the receiver to connect.
    pub async fn accept(endpoint: Endpoint) -> Result<Self, TransferError> {
        let accept = async {
            let incoming = endpoint
                .accept()
                .await
                .ok_or(TransferError::EndpointClosed)?;
            Ok::<_, TransferError>(incoming.accept()?.await?)
        };
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, accept)
            .await
            .map_err(|_| TransferError::PeerTimeout)??;
        tracing::info!("receiver connected");
        Ok(Self { conn, endpoint })
    }

    /// Connects to the sender.
    pub async fn connect(endpoint: Endpoint, peer: ConnectionInfo) -> Result<Self, TransferError> {
        let addr = Self::addr_from_connection_info(&peer)?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(addr, FSEND_ALPN))
            .await
            .map_err(|_| TransferError::PeerTimeout)??;
        tracing::info!("connected to sender");
        Ok(Self { conn, endpoint })
    }

    async fn close_with(&self, code: u32, reason: &[u8]) {
        self.conn.close(VarInt::from(code), reason);
        self.endpoint.close().await;
    }

    async fn run_send(
        &mut self,
        args: SendArgs,
        initial_progress_cb: ProgressCb<'_>,
        waiting_cb: WaitingCb<'_>,
        write_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        send_packet(
            &SenderToReceiver::ConnRequest {
                version: PROTO_VERSION.to_string(),
            },
            &self.conn,
        )
        .await?;

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
            &SenderToReceiver::FileInfo {
                files: files_available.clone(),
            },
            &self.conn,
        )
        .await?;

        waiting_cb();

        let to_skip = match receive_packet::<ReceiverToSender>(&self.conn).await? {
            ReceiverToSender::AcceptFilesSkip { files } => files,
            ReceiverToSender::RejectFiles => return Err(TransferError::PeerDeclined),
            _ => return Err(TransferError::UnexpectedPacket),
        };
        if to_skip.len() != files_available.len() {
            return Err(TransferError::UnexpectedPacket);
        }

        let plan = plan_transfer(&files_available, &to_skip);
        initial_progress_cb(&plan.progress);

        let mut send = self.conn.open_uni().await?;

        for (path, tree) in args.files.iter().zip(plan.to_transfer) {
            let Some(tree) = tree else { continue };
            let entries = flatten_trees(std::slice::from_ref(&tree), path);
            send_entries(&mut send, &entries, write_cb).await?;
        }

        send.shutdown().await?;

        // The receiver closes once everything is on disk. CLOSE_OK indicates
        // success.
        match self.conn.closed().await {
            ConnectionError::ApplicationClosed(close)
                if close.error_code == VarInt::from(CLOSE_OK) =>
            {
                Ok(())
            }
            reason => Err(TransferError::Disconnected(reason.to_string())),
        }
    }

    async fn run_receive(
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
                        &ReceiverToSender::WrongVersion {
                            expected: PROTO_VERSION.into(),
                        },
                        &self.conn,
                    )
                    .await?;
                    return Err(TransferError::WrongVersion(PROTO_VERSION.into(), version));
                }
                send_packet(&ReceiverToSender::Ok, &self.conn).await?;
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
                send_packet(&ReceiverToSender::RejectFiles, &self.conn).await?;
                return Err(TransferError::Declined);
            }
        };

        let files_to_skip = skip_list(&files_offered, &output_path, args.resume);
        let plan = plan_transfer(&files_offered, &files_to_skip);
        initial_progress_cb(&plan.progress);

        send_packet(
            &ReceiverToSender::AcceptFilesSkip {
                files: files_to_skip,
            },
            &self.conn,
        )
        .await?;

        let mut recv = self.conn.accept_uni().await?;

        tokio::fs::create_dir_all(&output_path).await?;
        for tree in plan.to_transfer.into_iter().flatten() {
            let entries =
                flatten_trees(std::slice::from_ref(&tree), &output_path.join(tree.name()));
            receive_entries(&mut recv, &entries, read_cb).await?;
        }

        Ok(())
    }
}

#[async_trait]
impl Transfer for IrohTransfer {
    async fn connection_type_name(&self) -> String {
        let paths = self.conn.paths();
        match paths.iter().find(|p| p.is_selected()) {
            Some(p) if p.is_ip() => "Direct",
            Some(p) if p.is_relay() => "Relay",
            Some(_) => "Unknown",
            None => "None",
        }
        .into()
    }

    async fn send_files(
        &mut self,
        args: SendArgs,
        initial_progress_cb: ProgressCb<'_>,
        waiting_cb: WaitingCb<'_>,
        write_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        let result = self
            .run_send(args, initial_progress_cb, waiting_cb, write_cb)
            .await;
        // On success the receiver already closed the connection.
        let code = if result.is_ok() { CLOSE_OK } else { CLOSE_ERR };
        self.close_with(code, b"sender done").await;
        result
    }

    async fn receive_files(
        &mut self,
        args: ReceiveArgs,
        initial_progress_cb: ProgressCb<'_>,
        accept_files_cb: AcceptFilesCb<'_>,
        read_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        let result = self
            .run_receive(args, initial_progress_cb, accept_files_cb, read_cb)
            .await;
        let code = if result.is_ok() { CLOSE_OK } else { CLOSE_ERR };
        self.close_with(code, b"receiver done").await;
        result
    }

    async fn close(&mut self) {
        self.close_with(CLOSE_OK, b"closed").await;
    }
}

#[cfg(test)]
#[path = "tests/iroh.rs"]
mod tests;
