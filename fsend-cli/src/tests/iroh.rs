use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;

use super::*;
use crate::transfer::test_support::{
    decline, no_data, no_offer, no_progress, no_wait, suite, Peers,
};

async fn local_endpoint() -> Endpoint {
    IrohTransfer::bind(presets::Minimal).await.unwrap()
}

/// The endpoint's loopback address on whatever ports it bound.
fn loopback_info(endpoint: &Endpoint) -> ConnectionInfo {
    let addrs = endpoint
        .bound_sockets()
        .into_iter()
        .map(|bound| {
            let ip: IpAddr = if bound.is_ipv4() {
                Ipv4Addr::LOCALHOST.into()
            } else {
                Ipv6Addr::LOCALHOST.into()
            };
            SocketAddr::new(ip, bound.port()).to_string()
        })
        .collect();
    ConnectionInfo::Iroh {
        node_id: endpoint.id().to_string(),
        addrs,
    }
}

async fn connected_pair() -> (IrohTransfer, IrohTransfer) {
    let sender = local_endpoint().await;
    let receiver = local_endpoint().await;
    let info = loopback_info(&sender);
    let (sender, receiver) = tokio::join!(
        IrohTransfer::accept(sender),
        IrohTransfer::connect(receiver, info)
    );
    (sender.unwrap(), receiver.unwrap())
}

fn pair() -> Pin<Box<dyn Future<Output = Peers> + Send>> {
    Box::pin(async {
        let (sender, receiver) = connected_pair().await;
        let (sender_conn, receiver_conn) = (sender.conn.clone(), receiver.conn.clone());
        Peers {
            sender: Box::new(sender),
            receiver: Box::new(receiver),
            abort_sender: Box::new(move || {
                sender_conn.close(VarInt::from(CLOSE_ERR), b"test: sender gone")
            }),
            abort_receiver: Box::new(move || {
                receiver_conn.close(VarInt::from(CLOSE_ERR), b"test: receiver gone")
            }),
        }
    })
}

// The behaviour every transport must have, shared with the WebRTC tests.
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

// iroh specifics.

#[tokio::test]
async fn connection_over_loopback_is_direct() {
    let (sender, receiver) = connected_pair().await;
    assert_eq!(sender.connection_type_name().await, "Direct");
    assert_eq!(receiver.connection_type_name().await, "Direct");
}

#[tokio::test]
async fn receiver_rejects_another_protocol_version() {
    let (raw_sender, mut receiver) = connected_pair().await;

    let peer = async {
        send_packet(
            &SenderToReceiver::ConnRequest {
                version: "9.9.9".into(),
            },
            &raw_sender.conn,
        )
        .await
        .unwrap();
        receive_packet::<ReceiverToSender>(&raw_sender.conn)
            .await
            .unwrap()
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

#[tokio::test]
async fn sender_stops_when_the_receiver_wants_another_version() {
    let (mut sender, raw_receiver) = connected_pair().await;

    let peer = async {
        let request = receive_packet::<SenderToReceiver>(&raw_receiver.conn)
            .await
            .unwrap();
        assert_eq!(
            request,
            SenderToReceiver::ConnRequest {
                version: PROTO_VERSION.into()
            }
        );
        send_packet(
            &ReceiverToSender::WrongVersion {
                expected: "9.9.9".into(),
            },
            &raw_receiver.conn,
        )
        .await
        .unwrap();
    };
    let (mut progress, mut waiting, mut data) = (no_progress, no_wait, no_data);
    let send = sender.send_files(
        SendArgs {
            files: vec!["unused".into()],
        },
        &mut progress,
        &mut waiting,
        &mut data,
    );

    let ((), result) = tokio::join!(peer, send);
    assert!(
        matches!(&result, Err(TransferError::WrongVersion(expected, got)) if expected == "9.9.9" && got == PROTO_VERSION),
        "{result:?}"
    );
}

#[tokio::test]
async fn unexpected_packets_are_errors() {
    // A sender that skips the handshake.
    let (raw_sender, mut receiver) = connected_pair().await;
    let peer = async {
        send_packet(
            &SenderToReceiver::FileInfo { files: vec![] },
            &raw_sender.conn,
        )
        .await
        .unwrap();
    };
    let (mut progress, mut offer, mut data) = (no_progress, decline, no_data);
    let receive = receiver.receive_files(
        ReceiveArgs { resume: false },
        &mut progress,
        &mut offer,
        &mut data,
    );
    let ((), result) = tokio::join!(peer, receive);
    assert!(
        matches!(result, Err(TransferError::UnexpectedPacket)),
        "{result:?}"
    );

    // A receiver that answers the handshake with a decline.
    let (mut sender, raw_receiver) = connected_pair().await;
    let peer = async {
        let _: SenderToReceiver = receive_packet(&raw_receiver.conn).await.unwrap();
        send_packet(&ReceiverToSender::RejectFiles, &raw_receiver.conn)
            .await
            .unwrap();
    };
    let (mut progress, mut waiting, mut data) = (no_progress, no_wait, no_data);
    let send = sender.send_files(
        SendArgs { files: vec![] },
        &mut progress,
        &mut waiting,
        &mut data,
    );
    let ((), result) = tokio::join!(peer, send);
    assert!(
        matches!(result, Err(TransferError::UnexpectedPacket)),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_skip_list_of_the_wrong_length_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, b"hello").unwrap();

    let (mut sender, raw_receiver) = connected_pair().await;
    let peer = async {
        let _: SenderToReceiver = receive_packet(&raw_receiver.conn).await.unwrap();
        send_packet(&ReceiverToSender::Ok, &raw_receiver.conn)
            .await
            .unwrap();
        let _: SenderToReceiver = receive_packet(&raw_receiver.conn).await.unwrap();
        send_packet(
            &ReceiverToSender::AcceptFilesSkip {
                files: vec![None, None],
            },
            &raw_receiver.conn,
        )
        .await
        .unwrap();
    };
    let (mut progress, mut waiting, mut data) = (no_progress, no_wait, no_data);
    let send = sender.send_files(
        SendArgs { files: vec![file] },
        &mut progress,
        &mut waiting,
        &mut data,
    );
    let ((), result) = tokio::join!(peer, send);
    assert!(
        matches!(result, Err(TransferError::UnexpectedPacket)),
        "{result:?}"
    );
}

#[tokio::test]
async fn packets_survive_the_wire() {
    let (a, b) = connected_pair().await;
    let big = SenderToReceiver::FileInfo {
        files: (0..5000)
            .map(|i| FilesAvailable::File {
                name: format!("file-{i}.bin"),
                size: i,
            })
            .collect(),
    };
    let (sent, received) = tokio::join!(
        send_packet(&big, &a.conn),
        receive_packet::<SenderToReceiver>(&b.conn)
    );
    sent.unwrap();
    assert_eq!(received.unwrap(), big);

    // The other direction, and a packet type mismatch.
    let (sent, received) = tokio::join!(
        send_packet(&ReceiverToSender::Ok, &b.conn),
        receive_packet::<SenderToReceiver>(&a.conn)
    );
    sent.unwrap();
    assert!(matches!(
        received,
        Err(TransferError::Packet(PacketError::Decode(_)))
    ));
}

#[tokio::test]
async fn a_closed_peer_is_a_connection_error() {
    let (a, b) = connected_pair().await;
    a.close_with(CLOSE_ERR, b"gone").await;
    let result = receive_packet::<SenderToReceiver>(&b.conn).await;
    assert!(
        matches!(result, Err(TransferError::Connection(_))),
        "{result:?}"
    );
}

#[test]
fn connection_info_round_trips_addresses_and_relays() {
    let secret = iroh::SecretKey::generate();
    let id = secret.public();
    let ip: SocketAddr = "192.0.2.1:4433".parse().unwrap();
    let relay: RelayUrl = "https://relay.example.org./".parse().unwrap();
    let addr = EndpointAddr::from_parts(
        id,
        [TransportAddr::Ip(ip), TransportAddr::Relay(relay.clone())],
    );

    let info = IrohTransfer::connection_info_from_addr(&addr);
    let ConnectionInfo::Iroh { node_id, addrs } = &info else {
        panic!("expected iroh info");
    };
    assert_eq!(node_id, &id.to_string());
    assert_eq!(addrs.len(), 2);
    assert!(addrs.contains(&ip.to_string()));
    assert!(addrs.contains(&relay.to_string()));

    let back = IrohTransfer::addr_from_connection_info(&info).unwrap();
    assert_eq!(back, addr);
}

#[test]
fn connection_info_ignores_what_it_cannot_parse() {
    let id = iroh::SecretKey::generate().public();
    let info = ConnectionInfo::Iroh {
        node_id: id.to_string(),
        addrs: vec![
            "10.0.0.1:1".into(),
            "not an address".into(),
            "[::1]:2".into(),
        ],
    };
    let addr = IrohTransfer::addr_from_connection_info(&info).unwrap();
    assert_eq!(addr.id, id);
    assert_eq!(addr.ip_addrs().count(), 2);
    assert_eq!(addr.relay_urls().count(), 0);

    let bad_id = ConnectionInfo::Iroh {
        node_id: "zzz".into(),
        addrs: vec![],
    };
    assert!(matches!(
        IrohTransfer::addr_from_connection_info(&bad_id),
        Err(TransferError::NodeId(_))
    ));

    let webrtc = ConnectionInfo::WebRtc {
        sdp: String::new(),
        ice_candidates: vec![],
    };
    assert!(matches!(
        IrohTransfer::addr_from_connection_info(&webrtc),
        Err(TransferError::UnexpectedConnectionInfo)
    ));
}
