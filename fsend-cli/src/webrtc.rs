use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch, Notify};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState, RTCSessionDescription,
    SettingEngine,
};
use webrtc::runtime::TokioRuntime;

use crate::transfer::{self, *};

pub const DEFAULT_STUN_SERVERS: &[&str] = &[
    "stun:stun.l.google.com:19302",
    "stun:stun1.l.google.com:19302",
];

pub const DEFAULT_BIND_ADDRS: &[&str] = &["0.0.0.0:0", "[::]:0"];
const CHUNK_SIZE: usize = 16384;
const FRAG_MORE: u8 = 0x00;
const FRAG_LAST: u8 = 0x01;
const MAX_DC_PAYLOAD: usize = CHUNK_SIZE - 1;

/// Per-channel send buffer.
const MAX_BUFFERED_BYTES: usize = 1024 * 1024;
const ICE_GATHERING_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for the peer to read what we last sent.
const GOODBYE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct WebRtcConfig {
    pub stun_servers: Vec<String>,
    /// Local UDP addresses to bind.
    pub bind_addrs: Vec<String>,
    /// If loopback candidates are offered.
    pub loopback: bool,
    /// ICE `(disconnected, failed, keep-alive)` timeouts.
    /// `None` keeps the webrtc crate defaults.
    pub ice_timeouts: Option<(Duration, Duration, Duration)>,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            stun_servers: DEFAULT_STUN_SERVERS.iter().map(|s| s.to_string()).collect(),
            bind_addrs: DEFAULT_BIND_ADDRS.iter().map(|s| s.to_string()).collect(),
            loopback: false,
            ice_timeouts: None,
        }
    }
}

/// State of the peer connection.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkState {
    Connecting,
    Connected,
    Closed { graceful: bool, reason: String },
}

type Link = watch::Receiver<LinkState>;

fn mark_closed(tx: &watch::Sender<LinkState>, graceful: bool, reason: impl Into<String>) {
    tx.send_if_modified(|state| {
        if matches!(state, LinkState::Closed { .. }) {
            return false;
        }
        *state = LinkState::Closed {
            graceful,
            reason: reason.into(),
        };
        true
    });
}

/// Resolves once the link is closed, with `(graceful, reason)`.
async fn closed(link: &mut Link) -> (bool, String) {
    match link
        .wait_for(|s| matches!(s, LinkState::Closed { .. }))
        .await
    {
        Ok(state) => match &*state {
            LinkState::Closed { graceful, reason } => (*graceful, reason.clone()),
            _ => unreachable!("wait_for only yields closed states"),
        },
        Err(_) => (false, "peer connection dropped".into()),
    }
}

fn if_closed(link: &Link) -> Result<(), TransferError> {
    match &*link.borrow() {
        LinkState::Closed { reason, .. } => Err(TransferError::Disconnected(reason.clone())),
        _ => Ok(()),
    }
}

/// Splits a control message into data channel sized fragments.
fn fragment(data: &[u8]) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(MAX_DC_PAYLOAD).collect()
    };
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut buf = Vec::with_capacity(1 + chunk.len());
            buf.push(if i == last { FRAG_LAST } else { FRAG_MORE });
            buf.extend_from_slice(chunk);
            buf
        })
        .collect()
}

/// Puts fragments back together.
#[derive(Default)]
struct Assembler {
    buf: Vec<u8>,
}

impl Assembler {
    /// Adds one fragment. Returns the whole message once the last one is in.
    fn push(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>, TransferError> {
        let Some((&header, payload)) = msg.split_first() else {
            self.buf.clear();
            return Err(TransferError::Protocol("empty fragment".into()));
        };
        match header {
            FRAG_LAST => {
                self.buf.extend_from_slice(payload);
                Ok(Some(std::mem::take(&mut self.buf)))
            }
            FRAG_MORE => {
                self.buf.extend_from_slice(payload);
                Ok(None)
            }
            other => {
                self.buf.clear();
                Err(TransferError::Protocol(format!(
                    "bad fragment header {other:#04x}"
                )))
            }
        }
    }
}

/// One data channel and the queue its messages land in.
struct Channel {
    dc: Arc<dyn DataChannel>,
    rx: mpsc::Receiver<Vec<u8>>,
    opened: Arc<Notify>,
}

struct Channels {
    control: Channel,
    data: Channel,
}

/// Hooks a data channel up to a message queue and to the link state.
fn attach(dc: Arc<dyn DataChannel>, link_tx: Arc<watch::Sender<LinkState>>) -> Channel {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(256);
    let opened = Arc::new(Notify::new());

    let poller = dc.clone();
    let opened_tx = opened.clone();
    tokio::spawn(async move {
        let label = poller.label().await.unwrap_or_default();
        while let Some(event) = poller.poll().await {
            match event {
                DataChannelEvent::OnOpen => opened_tx.notify_one(),
                DataChannelEvent::OnMessage(msg) => {
                    if tx.send(msg.data.to_vec()).await.is_err() {
                        break;
                    }
                }
                DataChannelEvent::OnClose => {
                    mark_closed(
                        &link_tx,
                        true,
                        format!("{label} channel closed by the peer"),
                    );
                    break;
                }
                DataChannelEvent::OnError => {
                    tracing::warn!("webrtc {label} channel error");
                }
                _ => {}
            }
        }
        mark_closed(&link_tx, false, format!("{label} channel went away"));
    });

    Channel { dc, rx, opened }
}

async fn recv_msg(ch: &mut Channel, link: &mut Link) -> Result<Vec<u8>, TransferError> {
    tokio::select! {
        biased;
        msg = ch.rx.recv() => msg.ok_or(TransferError::Internal("message queue closed")),
        (_, reason) = closed(link) => Err(TransferError::Disconnected(reason)),
    }
}

async fn recv_packet<P: for<'de> Deserialize<'de> + std::fmt::Debug>(
    ch: &mut Channel,
    link: &mut Link,
) -> Result<P, TransferError> {
    let mut assembler = Assembler::default();
    let compressed = loop {
        let msg = recv_msg(ch, link).await?;
        if let Some(whole) = assembler.push(&msg)? {
            break whole;
        }
    };
    let data = transfer::decompress_gzip(&compressed).await?;
    let packet: P = serde_json::from_slice(&data)?;
    tracing::debug!("webrtc received packet: {:?}", packet);
    Ok(packet)
}

/// Hands one message to the data channel.
async fn send_msg(ch: &Channel, link: &mut Link, data: &[u8]) -> Result<(), TransferError> {
    if_closed(link)?;
    tokio::select! {
        biased;
        (_, reason) = closed(link) => Err(TransferError::Disconnected(reason)),
        sent = ch.dc.send(BytesMut::from(data)) => {
            sent.map_err(TransferError::WebRtc)
        }
    }
}

async fn send_packet<P: Serialize + std::fmt::Debug>(
    ch: &Channel,
    link: &mut Link,
    packet: &P,
) -> Result<(), TransferError> {
    tracing::debug!("webrtc sending packet: {:?}", packet);
    let data = serde_json::to_vec(packet)?;
    let compressed = transfer::compress_gzip(&data).await?;
    for frag in fragment(&compressed) {
        send_msg(ch, link, &frag).await?;
    }
    Ok(())
}

async fn send_file(
    ch: &Channel,
    link: &mut Link,
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
        send_msg(ch, link, &buf[..n]).await?;
        read += n as u64;
        cb(n as u64);
    }
    Ok(())
}

async fn recv_file(
    ch: &mut Channel,
    link: &mut Link,
    path: &Path,
    skip: u64,
    size: u64,
    cb: &mut (dyn FnMut(u64) + Send),
) -> Result<(), TransferError> {
    use tokio::io::AsyncWriteExt;

    let mut file = open_for_receive(path, skip).await?;
    let mut written = skip;
    while written < size {
        let chunk = recv_msg(ch, link).await?;
        let n = chunk.len() as u64;
        if written + n > size {
            return Err(TransferError::Protocol(
                "the peer sent more data than it announced".into(),
            ));
        }
        file.write_all(&chunk).await?;
        written += n;
        cb(n);
    }
    finish_received_file(&mut file, size).await?;
    Ok(())
}

struct Events {
    link_tx: Arc<watch::Sender<LinkState>>,
    gathered: Arc<Notify>,
    incoming: Option<mpsc::Sender<Arc<dyn DataChannel>>>,
}

#[async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        tracing::debug!("webrtc ice gathering state: {state:?}");
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::info!("webrtc peer connection state: {state}");
        match state {
            RTCPeerConnectionState::Connected => {
                self.link_tx.send_if_modified(|s| {
                    if *s == LinkState::Connecting {
                        *s = LinkState::Connected;
                        true
                    } else {
                        false
                    }
                });
            }
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Disconnected => {
                mark_closed(&self.link_tx, false, format!("peer connection {state}"));
            }
            RTCPeerConnectionState::Closed => {
                mark_closed(&self.link_tx, true, "peer connection closed");
            }
            _ => {}
        }
    }

    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        if let Some(incoming) = &self.incoming {
            let _ = incoming.send(dc).await;
        }
    }
}

struct Built {
    pc: Arc<dyn PeerConnection>,
    link_tx: Arc<watch::Sender<LinkState>>,
    link: Link,
    gathered: Arc<Notify>,
}

async fn create_peer_connection(
    config: &WebRtcConfig,
    incoming: Option<mpsc::Sender<Arc<dyn DataChannel>>>,
) -> Result<Built, TransferError> {
    let (link_tx, link) = watch::channel(LinkState::Connecting);
    let link_tx = Arc::new(link_tx);
    let gathered = Arc::new(Notify::new());

    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(config.loopback);
    if let Some((disconnected, failed, keep_alive)) = config.ice_timeouts {
        settings.set_ice_timeouts(Some(disconnected), Some(failed), Some(keep_alive));
    }

    let ice_servers = if config.stun_servers.is_empty() {
        vec![]
    } else {
        vec![RTCIceServer {
            urls: config.stun_servers.clone(),
            ..Default::default()
        }]
    };
    let rtc_config = RTCConfigurationBuilder::new()
        .with_ice_servers(ice_servers)
        .build();

    let pc = PeerConnectionBuilder::new()
        .with_configuration(rtc_config)
        .with_setting_engine(settings)
        .with_handler(Arc::new(Events {
            link_tx: link_tx.clone(),
            gathered: gathered.clone(),
            incoming,
        }))
        .with_runtime(Arc::new(TokioRuntime))
        .with_udp_addrs(config.bind_addrs.clone())
        .with_data_channel_send_buffer_limit(MAX_BUFFERED_BYTES)
        .build()
        .await?;

    Ok(Built {
        pc: Arc::new(pc),
        link_tx,
        link,
        gathered,
    })
}

async fn wait_ice_gathering_complete(gathered: &Notify) {
    if tokio::time::timeout(ICE_GATHERING_TIMEOUT, gathered.notified())
        .await
        .is_err()
    {
        tracing::warn!("ice gathering did not finish in time; going with what there is");
    }
}

async fn get_local_sdp(pc: &Arc<dyn PeerConnection>) -> Result<String, TransferError> {
    pc.local_description()
        .await
        .map(|d| d.sdp)
        .ok_or(TransferError::Internal("no local description"))
}

pub struct WebRtcTransfer {
    pc: Arc<dyn PeerConnection>,
    link_tx: Arc<watch::Sender<LinkState>>,
    link: Link,
    channels: Option<Channels>,
    /// Answerer only: channels the offerer opens arrive here.
    pending: Option<mpsc::Receiver<Arc<dyn DataChannel>>>,
}

impl WebRtcTransfer {
    pub async fn create_offerer() -> Result<(Self, String), TransferError> {
        Self::create_offerer_with(&WebRtcConfig::default()).await
    }

    pub async fn create_offerer_with(
        config: &WebRtcConfig,
    ) -> Result<(Self, String), TransferError> {
        let Built {
            pc,
            link_tx,
            link,
            gathered,
        } = create_peer_connection(config, None).await?;

        let control = pc.create_data_channel("control", None).await?;
        let data = pc.create_data_channel("data", None).await?;
        let channels = Channels {
            control: attach(control, link_tx.clone()),
            data: attach(data, link_tx.clone()),
        };

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;

        wait_ice_gathering_complete(&gathered).await;
        let sdp = get_local_sdp(&pc).await?;

        Ok((
            Self {
                pc,
                link_tx,
                link,
                channels: Some(channels),
                pending: None,
            },
            sdp,
        ))
    }

    pub async fn set_answer(&self, answer_sdp: &str) -> Result<(), TransferError> {
        let answer = RTCSessionDescription::answer(answer_sdp.to_string())?;
        self.pc.set_remote_description(answer).await?;
        Ok(())
    }

    pub async fn create_answerer(offer_sdp: &str) -> Result<(Self, String), TransferError> {
        Self::create_answerer_with(offer_sdp, &WebRtcConfig::default()).await
    }

    pub async fn create_answerer_with(
        offer_sdp: &str,
        config: &WebRtcConfig,
    ) -> Result<(Self, String), TransferError> {
        let (dc_tx, dc_rx) = mpsc::channel::<Arc<dyn DataChannel>>(4);
        let Built {
            pc,
            link_tx,
            link,
            gathered,
        } = create_peer_connection(config, Some(dc_tx)).await?;

        let offer = RTCSessionDescription::offer(offer_sdp.to_string())?;
        pc.set_remote_description(offer).await?;

        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer).await?;

        wait_ice_gathering_complete(&gathered).await;
        let sdp = get_local_sdp(&pc).await?;

        Ok((
            Self {
                pc,
                link_tx,
                link,
                channels: None,
                pending: Some(dc_rx),
            },
            sdp,
        ))
    }

    /// Waits for the peer connection and data channels to be ready.
    pub async fn wait_connected(&mut self) -> Result<(), TransferError> {
        let state = self
            .link
            .wait_for(|s| *s != LinkState::Connecting)
            .await
            .map_err(|_| TransferError::Internal("state watcher dropped"))?
            .clone();
        if let LinkState::Closed { reason, .. } = state {
            return Err(TransferError::Disconnected(reason));
        }

        if let Some(mut pending) = self.pending.take() {
            let mut dcs = Vec::new();
            while dcs.len() < 2 {
                let dc = tokio::select! {
                    biased;
                    dc = pending.recv() => dc.ok_or(TransferError::Internal("data channels never arrived"))?,
                    (_, reason) = closed(&mut self.link) => {
                        return Err(TransferError::Disconnected(reason));
                    }
                };
                dcs.push(dc);
            }
            let second = dcs.pop().unwrap();
            let first = dcs.pop().unwrap();
            let (control, data) = if first.label().await.as_deref() == Ok("control") {
                (first, second)
            } else {
                (second, first)
            };
            self.channels = Some(Channels {
                control: attach(control, self.link_tx.clone()),
                data: attach(data, self.link_tx.clone()),
            });
        }

        let channels = self.channels.as_ref().expect("channels resolved above");
        for ch in [&channels.control, &channels.data] {
            if ch.dc.ready_state().await == Ok(RTCDataChannelState::Open) {
                continue;
            }
            tokio::select! {
                biased;
                _ = ch.opened.notified() => {}
                (_, reason) = closed(&mut self.link) => {
                    return Err(TransferError::Disconnected(reason));
                }
            }
        }
        Ok(())
    }

    /// Give the peer time to read what we last sent, then close.
    async fn goodbye(&mut self) {
        let _ = tokio::time::timeout(GOODBYE_TIMEOUT, closed(&mut self.link)).await;
    }

    async fn run_send(
        &mut self,
        args: SendArgs,
        initial_progress_cb: ProgressCb<'_>,
        waiting_cb: WaitingCb<'_>,
        write_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        let Self { channels, link, .. } = self;
        let ch = channels
            .as_mut()
            .expect("data channels not resolved; call wait_connected first");

        send_packet(
            &ch.control,
            link,
            &SenderToReceiver::ConnRequest {
                version: PROTO_VERSION.to_string(),
            },
        )
        .await?;

        match recv_packet::<ReceiverToSender>(&mut ch.control, link).await? {
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
            &ch.control,
            link,
            &SenderToReceiver::FileInfo {
                files: files_available.clone(),
            },
        )
        .await?;

        waiting_cb();

        let to_skip = match recv_packet::<ReceiverToSender>(&mut ch.control, link).await? {
            ReceiverToSender::AcceptFilesSkip { files } => files,
            ReceiverToSender::RejectFiles => return Err(TransferError::PeerDeclined),
            _ => return Err(TransferError::UnexpectedPacket),
        };
        if to_skip.len() != files_available.len() {
            return Err(TransferError::UnexpectedPacket);
        }

        let plan = plan_transfer(&files_available, &to_skip);
        initial_progress_cb(&plan.progress);

        for (path, tree) in args.files.iter().zip(plan.to_transfer) {
            let Some(tree) = tree else { continue };
            for entry in flatten_trees(std::slice::from_ref(&tree), path) {
                if let TransferEntry::File { path, skip, size } = entry {
                    send_file(&ch.data, link, &path, skip, size, write_cb).await?;
                }
            }
        }

        // The receiver hangs up once everything is on disk.
        let (_, reason) = closed(link).await;
        let outstanding = ch.data.dc.outstanding_bytes().await.unwrap_or(0);
        tracing::debug!("receiver hung up ({reason}); {outstanding} bytes unacknowledged");
        Ok(())
    }

    async fn run_receive(
        &mut self,
        args: ReceiveArgs,
        initial_progress_cb: ProgressCb<'_>,
        accept_files_cb: AcceptFilesCb<'_>,
        read_cb: DataCb<'_>,
    ) -> Result<(), TransferError> {
        let Self { channels, link, .. } = self;
        let ch = channels
            .as_mut()
            .expect("data channels not resolved; call wait_connected first");

        match recv_packet::<SenderToReceiver>(&mut ch.control, link).await? {
            SenderToReceiver::ConnRequest { version } => {
                if version != PROTO_VERSION {
                    send_packet(
                        &ch.control,
                        link,
                        &ReceiverToSender::WrongVersion {
                            expected: PROTO_VERSION.into(),
                        },
                    )
                    .await?;
                    self.goodbye().await;
                    return Err(TransferError::WrongVersion(PROTO_VERSION.into(), version));
                }
                send_packet(&ch.control, link, &ReceiverToSender::Ok).await?;
            }
            _ => return Err(TransferError::UnexpectedPacket),
        }

        let files_offered = match recv_packet::<SenderToReceiver>(&mut ch.control, link).await? {
            SenderToReceiver::FileInfo { files } => files,
            _ => return Err(TransferError::UnexpectedPacket),
        };

        let output_path = match accept_files_cb(&files_offered) {
            Some(p) => p,
            None => {
                send_packet(&ch.control, link, &ReceiverToSender::RejectFiles).await?;
                self.goodbye().await;
                return Err(TransferError::Declined);
            }
        };

        let files_to_skip = skip_list(&files_offered, &output_path, args.resume);
        let plan = plan_transfer(&files_offered, &files_to_skip);
        initial_progress_cb(&plan.progress);

        send_packet(
            &ch.control,
            link,
            &ReceiverToSender::AcceptFilesSkip {
                files: files_to_skip,
            },
        )
        .await?;

        tokio::fs::create_dir_all(&output_path).await?;
        for tree in plan.to_transfer.into_iter().flatten() {
            let entries =
                flatten_trees(std::slice::from_ref(&tree), &output_path.join(tree.name()));
            for entry in entries {
                match entry {
                    TransferEntry::Dir { path } => tokio::fs::create_dir_all(path).await?,
                    TransferEntry::File { path, skip, size } => {
                        recv_file(&mut ch.data, link, &path, skip, size, read_cb).await?;
                    }
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
        let result = self
            .run_send(args, initial_progress_cb, waiting_cb, write_cb)
            .await;
        self.close().await;
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
        self.close().await;
        result
    }

    async fn close(&mut self) {
        mark_closed(&self.link_tx, true, "closed locally");
        if let Err(e) = self.pc.close().await {
            tracing::debug!("webrtc close: {e}");
        }
    }
}

#[cfg(test)]
#[path = "tests/webrtc.rs"]
mod tests;
