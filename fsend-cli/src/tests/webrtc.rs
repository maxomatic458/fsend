use std::future::Future;
use std::pin::Pin;

use super::*;
use crate::transfer::test_support::{no_data, no_offer, no_progress, suite, Peers};

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
        let abort = |pc: Arc<dyn PeerConnection>,
                     link_tx: Arc<watch::Sender<LinkState>>|
         -> Box<dyn Fn() + Send + Sync> {
            Box::new(move || {
                mark_closed(&link_tx, false, "test: plug pulled");
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

// WebRTC specifics.

#[test]
fn fragments_carry_a_header_and_reassemble() {
    for size in [
        0,
        1,
        MAX_DC_PAYLOAD - 1,
        MAX_DC_PAYLOAD,
        MAX_DC_PAYLOAD + 1,
        3 * MAX_DC_PAYLOAD + 5,
    ] {
        let data: Vec<u8> = (0..size).map(|i| i as u8).collect();
        let frags = fragment(&data);

        let expected = if size == 0 {
            1
        } else {
            size.div_ceil(MAX_DC_PAYLOAD)
        };
        assert_eq!(frags.len(), expected, "size {size}");
        for (i, frag) in frags.iter().enumerate() {
            assert!(frag.len() <= CHUNK_SIZE);
            let last = i + 1 == frags.len();
            assert_eq!(frag[0], if last { FRAG_LAST } else { FRAG_MORE });
        }

        let mut assembler = Assembler::default();
        let mut whole = None;
        for frag in &frags {
            assert!(whole.is_none(), "message complete before the last fragment");
            whole = assembler.push(frag).unwrap();
        }
        assert_eq!(whole.unwrap(), data, "size {size}");
    }
}

#[test]
fn assembler_rejects_malformed_fragments() {
    let mut assembler = Assembler::default();
    assert!(matches!(
        assembler.push(&[]),
        Err(TransferError::Protocol(msg)) if msg.contains("empty")
    ));
    assert!(matches!(
        assembler.push(&[0x07, 1, 2]),
        Err(TransferError::Protocol(msg)) if msg.contains("header")
    ));
    // A correct message afterwards still works.
    assert_eq!(assembler.push(&[FRAG_MORE, 1]).unwrap(), None);
    assert_eq!(assembler.push(&[FRAG_LAST, 2]).unwrap(), Some(vec![1, 2]));
}

#[test]
fn default_config_uses_the_public_stun_servers() {
    let config = WebRtcConfig::default();
    assert_eq!(config.stun_servers, DEFAULT_STUN_SERVERS);
    assert_eq!(config.bind_addrs, DEFAULT_BIND_ADDRS);
    assert!(!config.loopback);
    assert!(config.ice_timeouts.is_none());
}

#[tokio::test]
async fn link_state_tracks_the_peer_connection() {
    let (mut sender, mut receiver) = connected_pair().await;
    assert_eq!(*sender.link.borrow(), LinkState::Connected);
    assert_eq!(*receiver.link.borrow(), LinkState::Connected);
    assert_eq!(sender.connection_type_name().await, "WebRTC");

    // The receiver hanging reaches the sender as connection lost
    receiver.close().await;
    assert!(
        if_closed(&receiver.link).is_err(),
        "a local close marks the link"
    );
    let (_, reason) = tokio::time::timeout(Duration::from_secs(10), closed(&mut sender.link))
        .await
        .expect("the sender must notice the close");
    assert!(reason.contains("disconnected"), "{reason}");
    assert!(if_closed(&sender.link).is_err());

    // Which makes any further send an error rather than a hang.
    let ch = sender.channels.as_mut().unwrap();
    let result = send_packet(&ch.control, &mut sender.link, &ReceiverToSender::Ok).await;
    assert!(
        matches!(result, Err(TransferError::Disconnected(_))),
        "{result:?}"
    );
    let result = recv_msg(&mut ch.control, &mut sender.link).await;
    assert!(
        matches!(result, Err(TransferError::Disconnected(_))),
        "{result:?}"
    );
    sender.close().await;
}

#[tokio::test]
async fn control_packets_round_trip_between_peers() {
    let (mut a, mut b) = connected_pair().await;
    let big = SenderToReceiver::FileInfo {
        files: (0..3000)
            .map(|i| FilesAvailable::File {
                name: format!("file-{i}.bin"),
                size: i,
            })
            .collect(),
    };
    let a_ch = a.channels.as_mut().unwrap();
    let b_ch = b.channels.as_mut().unwrap();

    send_packet(&a_ch.control, &mut a.link, &big).await.unwrap();
    let got: SenderToReceiver = recv_packet(&mut b_ch.control, &mut b.link).await.unwrap();
    assert_eq!(got, big);

    send_packet(&b_ch.control, &mut b.link, &ReceiverToSender::Ok)
        .await
        .unwrap();
    let got: ReceiverToSender = recv_packet(&mut a_ch.control, &mut a.link).await.unwrap();
    assert_eq!(got, ReceiverToSender::Ok);

    // Wrong packet type fails to decode.
    send_packet(&b_ch.control, &mut b.link, &ReceiverToSender::RejectFiles)
        .await
        .unwrap();
    let got = recv_packet::<SenderToReceiver>(&mut a_ch.control, &mut a.link).await;
    assert!(matches!(got, Err(TransferError::Json(_))), "{got:?}");

    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn receiver_rejects_another_protocol_version() {
    let (mut raw_sender, mut receiver) = connected_pair().await;

    let peer = async {
        let ch = raw_sender.channels.as_mut().unwrap();
        send_packet(
            &ch.control,
            &mut raw_sender.link,
            &SenderToReceiver::ConnRequest {
                version: "9.9.9".into(),
            },
        )
        .await
        .unwrap();
        let reply: ReceiverToSender = recv_packet(&mut ch.control, &mut raw_sender.link)
            .await
            .unwrap();
        raw_sender.close().await;
        reply
    };
    let (mut progress, mut offer, mut data) = (no_progress, no_offer, no_data);
    let receive = receiver.receive_files(
        ReceiveArgs { resume: false },
        &mut progress,
        &mut offer,
        &mut data,
    );

    let (reply, result) = tokio::join!(peer, receive);
    assert_eq!(
        reply,
        ReceiverToSender::WrongVersion {
            expected: PROTO_VERSION.into()
        }
    );
    assert!(
        matches!(&result, Err(TransferError::WrongVersion(expected, got)) if expected == PROTO_VERSION && got == "9.9.9"),
        "{result:?}"
    );
}
