use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use fsend_proto::peer::resume::{Conflict, ConflictPolicy};
use fsend_proto::peer::{Entry, progress::TopEntry};
use fsend_relay::Config;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::*;
use crate::transfer::test_support::{bytes, write};
use crate::transfer::{Callbacks, ConnectionKind, build_offer};

async fn start_relay() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("ws://{}", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(fsend_relay::serve(listener, Config::default()));
    url
}

/// A client that stays on this machine: a local relay, then WebRTC over loopback.
fn local_client(relay_url: Url) -> Client {
    Client {
        transports: vec![Transport::Webrtc],
        webrtc: WebRtcConfig {
            stun_servers: vec![],
            bind_addrs: vec!["127.0.0.1:0".parse().unwrap()],
            loopback: true,
            ice_timeouts: None,
        },
        ..Client::new(relay_url, "fsend-core tests")
    }
}

/// A file of `size` bytes in `dir`, offered.
fn offer(dir: &Path, size: usize) -> (Outgoing, Vec<u8>) {
    let data = bytes(size, 7);
    write(&dir.join("a.bin"), &data);
    (build_offer(&[dir.join("a.bin")]).unwrap(), data)
}

fn accept_into(out: PathBuf) -> impl FnMut(&[TopEntry]) -> Option<PathBuf> + Send {
    move |_| Some(out.clone())
}

fn skip(_: &[Entry], _: &[Conflict]) -> ConflictPolicy {
    ConflictPolicy::Skip
}

#[tokio::test]
async fn a_session_pairs_connects_and_sends() {
    let client = local_client(start_relay().await);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let (outgoing, data) = offer(tmp.path(), 300_000);
    let cancel = CancellationToken::new();
    let (code_tx, code_rx) = oneshot::channel();

    let (mut sender_kind, mut receiver_kind) = (None, None);
    let mut on_event = |event: Event| {
        if let Event::Connected(kind) = event {
            sender_kind = Some(kind);
        }
    };
    let on_code = move |code: &Code| code_tx.send(code.clone()).unwrap();
    let send = client.send(&outgoing, true, on_code, &mut on_event, &cancel);
    let receive = async {
        let code = code_rx.await.unwrap();
        let mut handler = Callbacks {
            accept: accept_into(out.clone()),
            resolve: skip,
            event: |event: Event| {
                if let Event::Connected(kind) = event {
                    receiver_kind = Some(kind);
                }
            },
        };
        client.receive(&code, &mut handler, &cancel).await
    };
    let (sent, received) = tokio::join!(send, receive);

    sent.unwrap();
    received.unwrap();
    assert_eq!(sender_kind, Some(ConnectionKind::WebRtc));
    assert_eq!(receiver_kind, Some(ConnectionKind::WebRtc));
    assert_eq!(std::fs::read(out.join("a.bin")).unwrap(), data);
}

#[tokio::test]
async fn cancelling_while_waiting_for_the_receiver_ends_at_once() {
    let client = local_client(start_relay().await);
    let tmp = tempfile::tempdir().unwrap();
    let (outgoing, _) = offer(tmp.path(), 10);
    let cancel = CancellationToken::new();
    let cancelled = cancel.clone();

    let mut on_event = |_: Event| {};
    let send = client.send(
        &outgoing,
        true,
        move |_| cancelled.cancel(),
        &mut on_event,
        &cancel,
    );
    let err = timeout(Duration::from_secs(5), send)
        .await
        .expect("cancelling ends the session")
        .unwrap_err();

    assert!(
        matches!(err, Error::Transfer(TransferError::Cancelled)),
        "{err:?}"
    );
    assert!(err.is_deliberate());
}

/// Cancels while its offer is open, as a user who closes it.
struct ClosesTheOffer(CancellationToken);

#[async_trait]
impl ReceiveHandler for ClosesTheOffer {
    async fn accept(&mut self, _: &[TopEntry]) -> Option<PathBuf> {
        self.0.cancel();
        std::future::pending().await
    }

    async fn resolve(&mut self, _: &[Entry], _: &[Conflict]) -> ConflictPolicy {
        unreachable!("the offer is never accepted")
    }

    fn event(&mut self, _: Event) {}
}

#[tokio::test]
async fn a_cancel_while_deciding_reaches_the_sender() {
    let client = local_client(start_relay().await);
    let tmp = tempfile::tempdir().unwrap();
    let (outgoing, _) = offer(tmp.path(), 10);
    let (code_tx, code_rx) = oneshot::channel();
    let (sender_cancel, receiver_cancel) = (CancellationToken::new(), CancellationToken::new());

    let mut on_event = |_: Event| {};
    let on_code = move |code: &Code| code_tx.send(code.clone()).unwrap();
    let send = client.send(&outgoing, true, on_code, &mut on_event, &sender_cancel);
    let receive = async {
        let code = code_rx.await.unwrap();
        let mut handler = ClosesTheOffer(receiver_cancel.clone());
        client.receive(&code, &mut handler, &receiver_cancel).await
    };
    let (sent, received) = tokio::join!(send, receive);

    assert!(
        matches!(received, Err(Error::Transfer(TransferError::Cancelled))),
        "{received:?}"
    );
    assert!(
        matches!(
            sent,
            Err(Error::Transfer(TransferError::PeerCancelled(None)))
        ),
        "{sent:?}"
    );
}

/// One transfer of `size` bytes, each side on a task of its own.
async fn transfer_on_tasks(client: Arc<Client>, dir: PathBuf, size: usize) {
    let (outgoing, data) = offer(&dir, size);
    let out = dir.join("out");
    let (code_tx, code_rx) = oneshot::channel();

    let sending = {
        let client = client.clone();
        tokio::spawn(async move {
            let on_code = move |code: &Code| code_tx.send(code.clone()).unwrap();
            let (mut on_event, cancel) = (|_: Event| {}, CancellationToken::new());
            client
                .send(&outgoing, true, on_code, &mut on_event, &cancel)
                .await
        })
    };
    let receiving = {
        let out = out.clone();
        tokio::spawn(async move {
            let code = code_rx.await.unwrap();
            let mut handler = Callbacks {
                accept: accept_into(out),
                resolve: skip,
                event: |_: Event| {},
            };
            let cancel = CancellationToken::new();
            client.receive(&code, &mut handler, &cancel).await
        })
    };

    sending.await.unwrap().unwrap();
    receiving.await.unwrap().unwrap();
    assert_eq!(std::fs::read(out.join("a.bin")).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_run_side_by_side_on_spawned_tasks() {
    let client = Arc::new(local_client(start_relay().await));
    let tmp = tempfile::tempdir().unwrap();
    let (first, second) = (tmp.path().join("first"), tmp.path().join("second"));

    tokio::join!(
        transfer_on_tasks(client.clone(), first, 2_000_000),
        transfer_on_tasks(client, second, 3_000_000),
    );
}
