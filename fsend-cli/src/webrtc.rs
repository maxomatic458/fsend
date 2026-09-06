use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Notify, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::StreamReader;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState, RTCSessionDescription,
    SettingEngine,
};
use webrtc::runtime::TokioRuntime;

use crate::transfer::*;

pub const DEFAULT_STUN_SERVERS: &[&str] = &[
    "stun:stun.l.google.com:19302",
    "stun:stun1.l.google.com:19302",
];

pub const DEFAULT_BIND_ADDRS: &[&str] = &["0.0.0.0:0", "[::]:0"];
pub const MAX_MESSAGE_BYTES: usize = 16384;

/// Per-channel send buffer.
const MAX_BUFFERED_BYTES: usize = 1024 * 1024;
const ICE_GATHERING_TIMEOUT: Duration = Duration::from_secs(5);

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
    Closed(String),
}

type Link = watch::Receiver<LinkState>;

fn mark_closed(tx: &watch::Sender<LinkState>, reason: impl Into<String>) {
    tx.send_if_modified(|state| {
        if matches!(state, LinkState::Closed(_)) {
            return false;
        }
        *state = LinkState::Closed(reason.into());
        true
    });
}

/// Resolves with the reason once the link is closed.
async fn closed(link: &mut Link) -> String {
    match link.wait_for(|s| matches!(s, LinkState::Closed(_))).await {
        Ok(state) => match &*state {
            LinkState::Closed(reason) => reason.clone(),
            _ => unreachable!("wait_for only yields closed states"),
        },
        Err(_) => "peer connection dropped".into(),
    }
}

fn lost(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, reason)
}

/// A data channel as a byte stream.
struct DcStream {
    dc: Arc<dyn DataChannel>,
    link: Link,
    opened: Arc<Notify>,
    reader: StreamReader<ReceiverStream<io::Result<Bytes>>, Bytes>,
    /// The send in progress.
    inflight: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,
}

impl DcStream {
    /// Drains the channel's events into the read queue.
    fn attach(
        dc: Arc<dyn DataChannel>,
        link_tx: Arc<watch::Sender<LinkState>>,
        link: Link,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(256);
        let opened = Arc::new(Notify::new());

        let poller = dc.clone();
        let opened_tx = opened.clone();
        let mut link_rx = link.clone();
        tokio::spawn(async move {
            let label = poller.label().await.unwrap_or_default();
            loop {
                let event = tokio::select! {
                    biased;
                    event = poller.poll() => event,
                    // The link can fail without the channel ever closing.
                    _ = closed(&mut link_rx) => break,
                };
                match event {
                    Some(DataChannelEvent::OnOpen) => opened_tx.notify_one(),
                    Some(DataChannelEvent::OnMessage(msg)) => {
                        if tx.send(Ok(msg.data.freeze())).await.is_err() {
                            return;
                        }
                    }
                    Some(DataChannelEvent::OnClose) => {
                        mark_closed(&link_tx, format!("{label} channel closed by the peer"));
                        break;
                    }
                    Some(DataChannelEvent::OnError) => {
                        tracing::warn!("webrtc {label} channel error")
                    }
                    Some(_) => {}
                    None => {
                        mark_closed(&link_tx, format!("{label} channel went away"));
                        break;
                    }
                }
            }
            let _ = tx.send(Err(lost(closed(&mut link_rx).await))).await;
        });

        Self {
            dc,
            link,
            opened,
            reader: StreamReader::new(ReceiverStream::new(rx)),
            inflight: None,
        }
    }

    async fn wait_open(&mut self) -> Result<(), TransferError> {
        if self.dc.ready_state().await == Ok(RTCDataChannelState::Open) {
            return Ok(());
        }
        tokio::select! {
            biased;
            _ = self.opened.notified() => Ok(()),
            reason = closed(&mut self.link) => Err(TransferError::Disconnected(reason)),
        }
    }
}

impl AsyncRead for DcStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl AsyncWrite for DcStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        std::task::ready!(self.as_mut().poll_flush(cx))?;

        let n = buf.len().min(MAX_MESSAGE_BYTES);
        let data = BytesMut::from(&buf[..n]);
        let dc = self.dc.clone();
        let mut link = self.link.clone();
        self.inflight = Some(Box::pin(async move {
            tokio::select! {
                biased;
                reason = closed(&mut link) => Err(lost(reason)),
                sent = dc.send(data) => sent.map_err(io::Error::other),
            }
        }));
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(inflight) = self.inflight.as_mut() {
            let result = std::task::ready!(inflight.as_mut().poll(cx));
            self.inflight = None;
            result?;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
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
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => {
                mark_closed(&self.link_tx, format!("peer connection {state}"));
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

pub struct WebRtcTransfer {
    pc: Arc<dyn PeerConnection>,
    link_tx: Arc<watch::Sender<LinkState>>,
    link: Link,
    gathered: Arc<Notify>,
    control: Option<DcStream>,
    data: Option<DcStream>,
    /// Answerer only: channels the offerer opens arrive here.
    pending: Option<mpsc::Receiver<Arc<dyn DataChannel>>>,
}

impl WebRtcTransfer {
    async fn new(
        config: &WebRtcConfig,
        pending: Option<mpsc::Receiver<Arc<dyn DataChannel>>>,
        incoming: Option<mpsc::Sender<Arc<dyn DataChannel>>>,
    ) -> Result<Self, TransferError> {
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

        Ok(Self {
            pc: Arc::new(pc),
            link_tx,
            link,
            gathered,
            control: None,
            data: None,
            pending,
        })
    }

    fn attach(&self, dc: Arc<dyn DataChannel>) -> DcStream {
        DcStream::attach(dc, self.link_tx.clone(), self.link.clone())
    }

    /// The local SDP once ICE gathering is done.
    async fn local_sdp(
        pc: &Arc<dyn PeerConnection>,
        gathered: &Notify,
    ) -> Result<String, TransferError> {
        if tokio::time::timeout(ICE_GATHERING_TIMEOUT, gathered.notified())
            .await
            .is_err()
        {
            tracing::warn!("ice gathering did not finish in time; going with what there is");
        }
        pc.local_description()
            .await
            .map(|d| d.sdp)
            .ok_or(TransferError::Internal("no local description"))
    }

    pub async fn create_offerer() -> Result<(Self, String), TransferError> {
        Self::create_offerer_with(&WebRtcConfig::default()).await
    }

    pub async fn create_offerer_with(
        config: &WebRtcConfig,
    ) -> Result<(Self, String), TransferError> {
        let mut this = Self::new(config, None, None).await?;

        let control = this.pc.create_data_channel("control", None).await?;
        let data = this.pc.create_data_channel("data", None).await?;
        this.control = Some(this.attach(control));
        this.data = Some(this.attach(data));

        let offer = this.pc.create_offer(None).await?;
        this.pc.set_local_description(offer).await?;
        let sdp = Self::local_sdp(&this.pc, &this.gathered).await?;
        Ok((this, sdp))
    }

    pub async fn set_answer(&mut self, answer_sdp: &str) -> Result<(), TransferError> {
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
        let this = Self::new(config, Some(dc_rx), Some(dc_tx)).await?;

        let offer = RTCSessionDescription::offer(offer_sdp.to_string())?;
        this.pc.set_remote_description(offer).await?;
        let answer = this.pc.create_answer(None).await?;
        this.pc.set_local_description(answer).await?;
        let sdp = Self::local_sdp(&this.pc, &this.gathered).await?;
        Ok((this, sdp))
    }

    /// Waits for the peer connection and both data channels to be ready.
    pub async fn wait_connected(&mut self) -> Result<(), TransferError> {
        let state = self
            .link
            .wait_for(|s| *s != LinkState::Connecting)
            .await
            .map_err(|_| TransferError::Internal("state watcher dropped"))?
            .clone();
        if let LinkState::Closed(reason) = state {
            return Err(TransferError::Disconnected(reason));
        }

        if let Some(mut pending) = self.pending.take() {
            let mut dcs = Vec::new();
            while dcs.len() < 2 {
                let dc = tokio::select! {
                    biased;
                    dc = pending.recv() => dc.ok_or(TransferError::Internal("data channels never arrived"))?,
                    reason = closed(&mut self.link) => return Err(TransferError::Disconnected(reason)),
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
            self.control = Some(self.attach(control));
            self.data = Some(self.attach(data));
        }

        for stream in [&mut self.control, &mut self.data] {
            stream
                .as_mut()
                .expect("channels resolved above")
                .wait_open()
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Transport for WebRtcTransfer {
    fn connection_type_name(&self) -> String {
        "WebRTC".into()
    }

    async fn control(&mut self) -> Result<Box<dyn ByteStream>, TransferError> {
        let stream = self
            .control
            .take()
            .ok_or(TransferError::Internal("control stream already taken"))?;
        Ok(Box::new(stream))
    }

    async fn data(&mut self) -> Result<Box<dyn ByteStream>, TransferError> {
        let stream = self
            .data
            .take()
            .ok_or(TransferError::Internal("data stream already taken"))?;
        Ok(Box::new(stream))
    }

    /// A remote close arrives as a disconnect.
    async fn closed(&mut self) -> Result<(), TransferError> {
        let reason = closed(&mut self.link).await;
        tracing::debug!("peer hung up: {reason}");
        Ok(())
    }

    async fn close(&mut self, _ok: bool) {
        mark_closed(&self.link_tx, "closed locally");
        if let Err(e) = self.pc.close().await {
            tracing::debug!("webrtc close: {e}");
        }
    }
}

#[cfg(test)]
#[path = "tests/webrtc.rs"]
mod tests;
