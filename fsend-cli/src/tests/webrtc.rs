use std::future::Future;
use std::pin::Pin;

use super::*;
use crate::transfer::test_support::{Peers, suite};

fn local_config() -> WebRtcConfig {
    WebRtcConfig {
        stun_servers: vec![],
        bind_addrs: vec!["127.0.0.1:0".into()],
        loopback: true,
        ice_timeouts: Some((
            Duration::from_secs(1),
            Duration::from_secs(3),
            Duration::from_millis(500),
        )),
    }
}

async fn connected_pair() -> (WebRtcTransfer, WebRtcTransfer) {
    let config = local_config();
    let (mut offerer, offer) = WebRtcTransfer::create_offerer_with(&config).await.unwrap();
    let (mut answerer, answer) = WebRtcTransfer::create_answerer_with(&offer, &config)
        .await
        .unwrap();
    offerer.set_answer(&answer).await.unwrap();
    let (a, b) = tokio::join!(offerer.wait_connected(), answerer.wait_connected());
    a.unwrap();
    b.unwrap();
    (offerer, answerer)
}

fn pair() -> Pin<Box<dyn Future<Output = Peers> + Send>> {
    Box::pin(async {
        let (sender, receiver) = connected_pair().await;
        // Pulling the plug: the peer connection goes away and, since
        // a local close produces no events, the link is marked as gone.
        let abort = |pc: Arc<dyn PeerConnection>,
                     link_tx: Arc<watch::Sender<LinkState>>|
         -> Box<dyn Fn() + Send + Sync> {
            Box::new(move || {
                mark_closed(&link_tx, "test: plug pulled");
                let pc = pc.clone();
                tokio::spawn(async move {
                    let _ = pc.close().await;
                });
            })
        };
        Peers {
            abort_sender: abort(sender.pc.clone(), sender.link_tx.clone()),
            abort_receiver: abort(receiver.pc.clone(), receiver.link_tx.clone()),
            sender: Box::new(sender),
            receiver: Box::new(receiver),
        }
    })
}

// The behaviour every transport must have, shared with the iroh tests.
#[tokio::test]
async fn sends_a_single_file() {
    suite::sends_a_single_file(pair).await
}
#[tokio::test]
async fn sends_an_empty_file() {
    suite::sends_an_empty_file(pair).await
}
#[tokio::test]
async fn sends_a_file_larger_than_the_buffers() {
    suite::sends_a_file_larger_than_the_buffers(pair).await
}
#[tokio::test]
async fn sends_a_directory_tree() {
    suite::sends_a_directory_tree(pair).await
}
#[tokio::test]
async fn sends_several_entries_at_once() {
    suite::sends_several_entries_at_once(pair).await
}
#[tokio::test]
async fn sends_the_current_directory() {
    suite::sends_the_current_directory(pair).await
}
#[tokio::test]
async fn receiver_can_decline() {
    suite::receiver_can_decline(pair).await
}
#[tokio::test]
async fn resumes_a_partial_file() {
    suite::resumes_a_partial_file(pair).await
}
#[tokio::test]
async fn resumes_a_partial_directory() {
    suite::resumes_a_partial_directory(pair).await
}
#[tokio::test]
async fn resume_skips_complete_files() {
    suite::resume_skips_complete_files(pair).await
}
#[tokio::test]
async fn resume_starts_over_when_the_local_file_is_larger() {
    suite::resume_starts_over_when_the_local_file_is_larger(pair).await
}
#[tokio::test]
async fn overwrite_ignores_partial_files() {
    suite::overwrite_ignores_partial_files(pair).await
}
#[tokio::test]
async fn overwrite_truncates_a_larger_stale_file() {
    suite::overwrite_truncates_a_larger_stale_file(pair).await
}
#[tokio::test]
async fn interrupted_by_the_receiver_then_resumed() {
    suite::interrupted_by_the_receiver_then_resumed(pair).await
}
#[tokio::test]
async fn interrupted_by_the_sender_then_resumed() {
    suite::interrupted_by_the_sender_then_resumed(pair).await
}
#[tokio::test]
async fn a_missing_file_fails_the_transfer() {
    suite::a_missing_file_fails_the_transfer(pair).await
}
#[tokio::test]
async fn receiver_rejects_another_protocol_version() {
    suite::receiver_rejects_another_protocol_version(pair).await
}
#[tokio::test]
async fn sender_stops_when_the_receiver_wants_another_version() {
    suite::sender_stops_when_the_receiver_wants_another_version(pair).await
}
#[tokio::test]
async fn unexpected_packets_are_errors() {
    suite::unexpected_packets_are_errors(pair).await
}
#[tokio::test]
async fn a_skip_list_of_the_wrong_length_is_rejected() {
    suite::a_skip_list_of_the_wrong_length_is_rejected(pair).await
}
#[tokio::test]
async fn packets_survive_the_wire() {
    suite::packets_survive_the_wire(pair).await
}
#[tokio::test]
async fn a_closed_peer_fails_the_control_stream() {
    suite::a_closed_peer_fails_the_control_stream(pair).await
}

// WebRTC specifics.

#[test]
fn default_config_uses_the_public_stun_servers() {
    let config = WebRtcConfig::default();
    assert_eq!(config.stun_servers, DEFAULT_STUN_SERVERS);
    assert_eq!(config.bind_addrs, DEFAULT_BIND_ADDRS);
    assert!(!config.loopback);
    assert!(config.ice_timeouts.is_none());
}

#[tokio::test]
async fn a_lost_link_fails_the_streams_instead_of_hanging() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut sender, mut receiver) = connected_pair().await;
    assert_eq!(*sender.link.borrow(), LinkState::Connected);
    assert_eq!(sender.connection_type_name(), "WebRTC");
    let mut data = sender.data().await.unwrap();

    // The receiver hanging up reaches the sender as a lost connection.
    receiver.close(false).await;
    assert!(matches!(&*receiver.link.borrow(), LinkState::Closed(_)));
    tokio::time::timeout(Duration::from_secs(10), sender.closed())
        .await
        .expect("the sender must notice the close")
        .unwrap();

    // Which makes any further write or read an error rather than a hang.
    let result = async {
        data.write_all(b"more").await?;
        data.flush().await
    }
    .await;
    assert!(
        matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::ConnectionAborted),
        "{result:?}"
    );
    let mut buf = [0u8; 8];
    let result = data.read(&mut buf).await;
    assert!(
        matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::ConnectionAborted),
        "{result:?}"
    );
    sender.close(false).await;
}

#[tokio::test]
async fn writes_are_split_into_channel_sized_messages() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut sender, mut receiver) = connected_pair().await;
    let mut out = sender.data().await.unwrap();
    let mut inp = receiver.data().await.unwrap();

    // More than one message
    let payload: Vec<u8> = (0..5 * MAX_MESSAGE_BYTES + 17).map(|i| i as u8).collect();
    let write = async {
        out.write_all(&payload).await.unwrap();
        out.flush().await.unwrap();
    };
    let read = async {
        let mut got = vec![0u8; payload.len()];
        inp.read_exact(&mut got).await.unwrap();
        got
    };
    let ((), got) = tokio::join!(write, read);
    assert_eq!(got, payload);

    sender.close(true).await;
    receiver.close(true).await;
}
