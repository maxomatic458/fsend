use std::path::Path;
use std::sync::Arc;

use async_recursion::async_recursion;
use async_trait::async_trait;
use bincode::{Decode, Encode};
use bytes::Bytes;
use tokio::sync::{mpsc, Mutex, Notify};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::transfer::{self, *};

const CHUNK_SIZE: usize = 16384; // 16KB chunks for DataChannel messages

// Fragment header: 0x00 = more fragments follow, 0x01 = last fragment.
const FRAG_MORE: u8 = 0x00;
const FRAG_LAST: u8 = 0x01;
// Max payload per DC message (leave 1 byte for the fragment header).
const MAX_DC_PAYLOAD: usize = CHUNK_SIZE - 1;

// --- Packet helpers over DataChannel (with fragmentation) ---

async fn send_fragmented(
    dc: &Arc<RTCDataChannel>,
    data: &[u8],
) -> Result<(), TransferError> {
    for (i, chunk) in data.chunks(MAX_DC_PAYLOAD).enumerate() {
        let is_last = (i + 1) * MAX_DC_PAYLOAD >= data.len();
        let mut buf = Vec::with_capacity(1 + chunk.len());
        buf.push(if is_last { FRAG_LAST } else { FRAG_MORE });
        buf.extend_from_slice(chunk);
        dc.send(&Bytes::from(buf))
            .await
            .map_err(|e| TransferError::WebRtc(format!("send: {e}")))?;
    }
    Ok(())
}

async fn recv_fragmented(
    rx: &Mutex<mpsc::Receiver<Vec<u8>>>,
) -> Result<Vec<u8>, TransferError> {
    let mut rx = rx.lock().await;
    let mut assembled = Vec::new();
    loop {
        let msg = rx
            .recv()
            .await
            .ok_or_else(|| TransferError::WebRtc("channel closed".into()))?;
        if msg.is_empty() {
            return Err(TransferError::WebRtc("empty fragment".into()));
        }
        let header = msg[0];
        assembled.extend_from_slice(&msg[1..]);
        if header == FRAG_LAST {
            break;
        }
    }
    Ok(assembled)
}

async fn send_packet<P: Encode + std::fmt::Debug>(
    dc: &Arc<RTCDataChannel>,
    packet: &P,
) -> Result<(), TransferError> {
    tracing::debug!("webrtc sending packet: {:?}", packet);
    let data = bincode::encode_to_vec(packet, bincode::config::standard())
        .map_err(|e| TransferError::WebRtc(format!("encode: {e}")))?;
    let compressed = transfer::compress_gzip(&data).await?;
    send_fragmented(dc, &compressed).await
}

async fn recv_packet<P: Decode<()> + std::fmt::Debug>(
    rx: &Mutex<mpsc::Receiver<Vec<u8>>>,
) -> Result<P, TransferError> {
    let compressed = recv_fragmented(rx).await?;
    let decompressed = transfer::decompress_gzip(&compressed).await?;
    let (packet, _) = bincode::decode_from_slice(&decompressed, bincode::config::standard())
        .map_err(|e| TransferError::WebRtc(format!("decode: {e}")))?;
    tracing::debug!("webrtc received packet: {:?}", packet);
    Ok(packet)
}

// --- WebRTC helpers ---

fn rtc_config() -> RTCConfiguration {
    RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec![
                "stun:stun.l.google.com:19302".to_string(),
                "stun:stun1.l.google.com:19302".to_string(),
            ],
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn create_peer_connection() -> Result<RTCPeerConnection, TransferError> {
    let mut m = MediaEngine::default();
    m.register_default_codecs()
        .map_err(|e| TransferError::WebRtc(format!("media engine: {e}")))?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)
        .map_err(|e| TransferError::WebRtc(format!("interceptors: {e}")))?;

    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();

    api.new_peer_connection(rtc_config())
        .await
        .map_err(|e| TransferError::WebRtc(format!("new_peer_connection: {e}")))
}

fn setup_message_channel(dc: &Arc<RTCDataChannel>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(256);
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        let tx = tx.clone();
        Box::pin(async move {
            let _ = tx.send(msg.data.to_vec()).await;
        })
    }));
    rx
}

async fn wait_ice_gathering_complete(pc: &Arc<RTCPeerConnection>) {
    let mut gather_complete = pc.gathering_complete_promise().await;
    let _ = gather_complete.recv().await;
}

async fn get_local_sdp(pc: &Arc<RTCPeerConnection>) -> Result<String, TransferError> {
    pc.local_description()
        .await
        .map(|d| d.sdp)
        .ok_or_else(|| TransferError::WebRtc("no local description".into()))
}

// --- WebRtcTransfer ---

pub struct WebRtcTransfer {
    pc: Arc<RTCPeerConnection>,
    control_dc: Option<Arc<RTCDataChannel>>,
    data_dc: Option<Arc<RTCDataChannel>>,
    control_rx: Mutex<mpsc::Receiver<Vec<u8>>>,
    data_rx: Mutex<mpsc::Receiver<Vec<u8>>>,
    connected: Arc<Notify>,
    /// For the answerer: receives data channels from the offerer after connection.
    pending_dc_rx: Option<Mutex<mpsc::Receiver<Arc<RTCDataChannel>>>>,
}

impl WebRtcTransfer {
    /// Create as the offerer (sender side). Returns (self, SDP offer).
    pub async fn create_offerer() -> Result<(Self, String), TransferError> {
        let pc = Arc::new(create_peer_connection().await?);

        let control_dc = pc
            .create_data_channel("control", None)
            .await
            .map_err(|e| TransferError::WebRtc(format!("create control dc: {e}")))?;
        let data_dc = pc
            .create_data_channel("data", None)
            .await
            .map_err(|e| TransferError::WebRtc(format!("create data dc: {e}")))?;

        let control_rx = setup_message_channel(&control_dc);
        let data_rx = setup_message_channel(&data_dc);

        let connected = Arc::new(Notify::new());
        let connected_clone = connected.clone();
        pc.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            tracing::info!("webrtc peer connection state: {state}");
            if state == RTCPeerConnectionState::Connected {
                connected_clone.notify_waiters();
            }
            Box::pin(async {})
        }));

        let offer = pc
            .create_offer(None)
            .await
            .map_err(|e| TransferError::WebRtc(format!("create offer: {e}")))?;
        pc.set_local_description(offer)
            .await
            .map_err(|e| TransferError::WebRtc(format!("set local desc: {e}")))?;

        wait_ice_gathering_complete(&pc).await;
        let sdp = get_local_sdp(&pc).await?;

        Ok((
            Self {
                pc,
                control_dc: Some(control_dc),
                data_dc: Some(data_dc),
                control_rx: Mutex::new(control_rx),
                data_rx: Mutex::new(data_rx),
                connected,
                pending_dc_rx: None,
            },
            sdp,
        ))
    }

    /// Set the remote SDP answer (sender side, after receiving from relay).
    pub async fn set_answer(&self, answer_sdp: &str) -> Result<(), TransferError> {
        let answer = RTCSessionDescription::answer(answer_sdp.to_string())
            .map_err(|e| TransferError::WebRtc(format!("parse answer: {e}")))?;
        self.pc
            .set_remote_description(answer)
            .await
            .map_err(|e| TransferError::WebRtc(format!("set remote desc: {e}")))?;
        Ok(())
    }

    /// Create as the answerer (receiver side). Returns (self, SDP answer).
    /// Data channels are resolved lazily in `wait_connected` after the
    /// answer SDP has been sent back to the offerer via the relay.
    pub async fn create_answerer(offer_sdp: &str) -> Result<(Self, String), TransferError> {
        let pc = Arc::new(create_peer_connection().await?);

        let connected = Arc::new(Notify::new());
        let connected_clone = connected.clone();
        pc.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            tracing::info!("webrtc peer connection state: {state}");
            if state == RTCPeerConnectionState::Connected {
                connected_clone.notify_waiters();
            }
            Box::pin(async {})
        }));

        // Collect data channels from the offerer (arrive after connection is up).
        let (dc_tx, dc_rx) = mpsc::channel::<Arc<RTCDataChannel>>(4);
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let tx = dc_tx.clone();
            Box::pin(async move {
                let _ = tx.send(dc).await;
            })
        }));

        let offer = RTCSessionDescription::offer(offer_sdp.to_string())
            .map_err(|e| TransferError::WebRtc(format!("parse offer: {e}")))?;
        pc.set_remote_description(offer)
            .await
            .map_err(|e| TransferError::WebRtc(format!("set remote desc: {e}")))?;

        let answer = pc
            .create_answer(None)
            .await
            .map_err(|e| TransferError::WebRtc(format!("create answer: {e}")))?;
        pc.set_local_description(answer)
            .await
            .map_err(|e| TransferError::WebRtc(format!("set local desc: {e}")))?;

        wait_ice_gathering_complete(&pc).await;
        let sdp = get_local_sdp(&pc).await?;

        // Placeholder channels — real ones arrive in wait_connected().
        let (_ctrl_tx, control_rx) = mpsc::channel(1);
        let (_data_tx, data_rx) = mpsc::channel(1);

        Ok((
            Self {
                pc,
                control_dc: None,
                data_dc: None,
                control_rx: Mutex::new(control_rx),
                data_rx: Mutex::new(data_rx),
                connected,
                pending_dc_rx: Some(Mutex::new(dc_rx)),
            },
            sdp,
        ))
    }

    /// Wait for the peer connection to reach the Connected state.
    /// For the answerer, this also resolves the pending data channels.
    pub async fn wait_connected(&mut self) -> Result<(), TransferError> {
        if self.pc.connection_state() != RTCPeerConnectionState::Connected {
            self.connected.notified().await;
        }

        // Resolve pending data channels (answerer side).
        if let Some(pending) = self.pending_dc_rx.take() {
            let mut dc_rx = pending.into_inner();
            let dc1 = dc_rx
                .recv()
                .await
                .ok_or_else(|| TransferError::WebRtc("expected data channel".into()))?;
            let dc2 = dc_rx
                .recv()
                .await
                .ok_or_else(|| TransferError::WebRtc("expected data channel".into()))?;

            let (control_dc, data_dc) = if dc1.label() == "control" {
                (dc1, dc2)
            } else {
                (dc2, dc1)
            };

            let control_rx = setup_message_channel(&control_dc);
            let data_rx = setup_message_channel(&data_dc);

            self.control_dc = Some(control_dc);
            self.data_dc = Some(data_dc);
            self.control_rx = Mutex::new(control_rx);
            self.data_rx = Mutex::new(data_rx);
        }

        Ok(())
    }

    fn control_dc(&self) -> &Arc<RTCDataChannel> {
        self.control_dc.as_ref().expect("data channels not resolved; call wait_connected first")
    }

    fn data_dc(&self) -> &Arc<RTCDataChannel> {
        self.data_dc.as_ref().expect("data channels not resolved; call wait_connected first")
    }

    async fn send_file_data(
        dc: &Arc<RTCDataChannel>,
        path: &Path,
        skip: u64,
        size: u64,
        cb: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), TransferError> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let mut file = tokio::fs::File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(skip)).await?;

        let mut buf = vec![0u8; MAX_DC_PAYLOAD];
        let mut read = skip;
        while read < size {
            let to_read = std::cmp::min(MAX_DC_PAYLOAD as u64, size - read) as usize;
            let n = file.read(&mut buf[..to_read]).await?;
            if n == 0 {
                return Err(TransferError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected eof",
                )));
            }
            dc.send(&Bytes::copy_from_slice(&buf[..n]))
                .await
                .map_err(|e| TransferError::WebRtc(format!("send data: {e}")))?;
            read += n as u64;
            cb(n as u64);
        }
        Ok(())
    }

    #[async_recursion]
    async fn send_directory(
        dc: &Arc<RTCDataChannel>,
        root: &Path,
        files: &[FileSendRecvTree],
        cb: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), TransferError> {
        for file in files {
            match file {
                FileSendRecvTree::File { name, skip, size } => {
                    let path = root.join(name);
                    Self::send_file_data(dc, &path, *skip, *size, cb).await?;
                }
                FileSendRecvTree::Dir { name, files } => {
                    Self::send_directory(dc, &root.join(name), files, cb).await?;
                }
            }
        }
        Ok(())
    }

    async fn recv_file_data(
        data_rx: &Mutex<mpsc::Receiver<Vec<u8>>>,
        path: &Path,
        skip: u64,
        size: u64,
        cb: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), TransferError> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)
            .await?;
        file.seek(std::io::SeekFrom::Start(skip)).await?;

        let mut written = skip;
        let mut rx = data_rx.lock().await;
        while written < size {
            let chunk = rx
                .recv()
                .await
                .ok_or_else(|| TransferError::WebRtc("data channel closed".into()))?;
            let n = chunk.len() as u64;
            file.write_all(&chunk).await?;
            written += n;
            cb(n);
        }

        file.sync_all().await?;
        file.shutdown().await?;
        Ok(())
    }

    #[async_recursion]
    async fn recv_directory(
        data_rx: &Mutex<mpsc::Receiver<Vec<u8>>>,
        root: &Path,
        files: &[FileSendRecvTree],
        cb: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), TransferError> {
        for file in files {
            match file {
                FileSendRecvTree::File { name, skip, size } => {
                    let path = root.join(name);
                    Self::recv_file_data(data_rx, &path, *skip, *size, cb).await?;
                }
                FileSendRecvTree::Dir { name, files } => {
                    let dir = root.join(name);
                    if !dir.exists() {
                        std::fs::create_dir(&dir)?;
                    }
                    Self::recv_directory(data_rx, &dir, files, cb).await?;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Transfer for WebRtcTransfer {
    async fn connection_type_name(&self) -> String {
        "WebRTC".into()
    }

    async fn send_files(
        &mut self,
        args: SendArgs,
        initial_progress_cb: ProgressCb<'_>,
        waiting_cb: WaitingCb<'_>,
        write_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        // Version handshake via control channel
        send_packet(
            self.control_dc(),
            &SenderToReceiver::ConnRequest {
                version: PROTO_VERSION.to_string(),
            },
        )
        .await?;

        match recv_packet::<ReceiverToSender>(&self.control_rx).await? {
            ReceiverToSender::Ok => {}
            ReceiverToSender::WrongVersion { expected } => {
                return Err(TransferError::WrongVersion(expected, PROTO_VERSION.into()));
            }
            _ => return Err(TransferError::UnexpectedPacket),
        }

        // Build file info
        let mut files_available = Vec::new();
        for path in &args.files {
            if !path.exists() {
                return Err(TransferError::FileNotFound(path.clone()));
            }
            files_available.push(get_files_available(path)?);
        }

        send_packet(
            self.control_dc(),
            &SenderToReceiver::FileInfo {
                files: files_available.clone(),
            },
        )
        .await?;

        waiting_cb();

        let to_skip = match recv_packet::<ReceiverToSender>(&self.control_rx).await? {
            ReceiverToSender::AcceptFilesSkip { files } => files,
            ReceiverToSender::RejectFiles => return Err(TransferError::FilesRejected),
            _ => return Err(TransferError::UnexpectedPacket),
        };

        let to_send: Vec<Option<FileSendRecvTree>> = files_available
            .iter()
            .zip(&to_skip)
            .map(|(file, skip)| match skip {
                Some(s) => file.remove_skipped(s),
                None => Some(file.to_send_recv_tree()),
            })
            .collect();

        let progress: Vec<(String, u64, u64)> = files_available
            .iter()
            .zip(&to_skip)
            .map(|(file, skip)| {
                (
                    file.name().to_string(),
                    skip.as_ref().map(|s| s.skip()).unwrap_or(0),
                    file.size(),
                )
            })
            .collect();

        initial_progress_cb(&progress);

        // Stream file data via the data channel
        for (path, file) in args.files.iter().zip(to_send) {
            if let Some(file) = file {
                match file {
                    FileSendRecvTree::File { skip, size, .. } => {
                        Self::send_file_data(self.data_dc(), path, skip, size, write_cb).await?;
                    }
                    FileSendRecvTree::Dir { files, .. } => {
                        Self::send_directory(self.data_dc(), path, &files, write_cb).await?;
                    }
                }
            }
        }

        // Signal end of transfer with an empty message
        self.data_dc()
            .send(&Bytes::new())
            .await
            .map_err(|e| TransferError::WebRtc(format!("send eof: {e}")))?;

        // Wait for the receiver to close
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        Ok(())
    }

    async fn receive_files(
        &mut self,
        args: ReceiveArgs,
        initial_progress_cb: ProgressCb<'_>,
        accept_files_cb: AcceptFilesCb<'_>,
        read_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        // Version handshake
        match recv_packet::<SenderToReceiver>(&self.control_rx).await? {
            SenderToReceiver::ConnRequest { version } => {
                if version != PROTO_VERSION {
                    send_packet(
                        self.control_dc(),
                        &ReceiverToSender::WrongVersion {
                            expected: PROTO_VERSION.into(),
                        },
                    )
                    .await?;
                    return Err(TransferError::WrongVersion(PROTO_VERSION.into(), version));
                }
                send_packet(self.control_dc(), &ReceiverToSender::Ok).await?;
            }
            _ => return Err(TransferError::UnexpectedPacket),
        }

        let files_offered = match recv_packet::<SenderToReceiver>(&self.control_rx).await? {
            SenderToReceiver::FileInfo { files } => files,
            _ => return Err(TransferError::UnexpectedPacket),
        };

        let output_path = match accept_files_cb(&files_offered) {
            Some(p) => p,
            None => {
                send_packet(self.control_dc(), &ReceiverToSender::RejectFiles).await?;
                return Err(TransferError::FilesRejected);
            }
        };

        let files_to_skip: Vec<Option<FilesToSkip>> = if args.resume {
            files_offered
                .iter()
                .map(|offered| {
                    let local_path = output_path.join(offered.name());
                    get_files_available(&local_path)
                        .ok()
                        .and_then(|local| offered.get_skippable(&local))
                })
                .collect()
        } else {
            vec![None; files_offered.len()]
        };

        let to_receive: Vec<Option<FileSendRecvTree>> = files_offered
            .iter()
            .zip(&files_to_skip)
            .map(|(offered, skip)| match skip {
                Some(s) => offered.remove_skipped(s),
                None => Some(offered.to_send_recv_tree()),
            })
            .collect();

        let progress: Vec<(String, u64, u64)> = files_offered
            .iter()
            .zip(&files_to_skip)
            .map(|(offered, skip)| {
                (
                    offered.name().to_string(),
                    skip.as_ref().map(|s| s.skip()).unwrap_or(0),
                    offered.size(),
                )
            })
            .collect();

        initial_progress_cb(&progress);

        send_packet(
            self.control_dc(),
            &ReceiverToSender::AcceptFilesSkip {
                files: files_to_skip,
            },
        )
        .await?;

        // Receive file data from the data channel
        for file in to_receive.into_iter().flatten() {
            match file {
                FileSendRecvTree::File { name, skip, size } => {
                    let path = output_path.join(&name);
                    Self::recv_file_data(&self.data_rx, &path, skip, size, read_cb).await?;
                }
                FileSendRecvTree::Dir { name, files } => {
                    let dir = output_path.join(&name);
                    if !dir.exists() {
                        std::fs::create_dir(&dir)?;
                    }
                    Self::recv_directory(&self.data_rx, &dir, &files, read_cb).await?;
                }
            }
        }

        self.close().await;
        Ok(())
    }

    async fn close(&mut self) {
        let _ = self.pc.close().await;
    }
}
