use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;

use super::*;
use crate::transfer::test_support::{Peers, suite};

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
async fn resume_restarts_files_that_differ_locally() {
    suite::resume_restarts_files_that_differ_locally(pair).await
}
#[tokio::test]
async fn resume_resends_a_same_size_file_that_differs() {
    suite::resume_resends_a_same_size_file_that_differs(pair).await
}
#[tokio::test]
async fn sender_fails_when_the_receiver_cannot_store_the_files() {
    suite::sender_fails_when_the_receiver_cannot_store_the_files(pair).await
}
#[tokio::test]
async fn refuses_names_that_escape_the_output_dir() {
    suite::refuses_names_that_escape_the_output_dir(pair).await
}
#[tokio::test]
async fn refuses_offers_that_would_spoof_the_prompt() {
    suite::refuses_offers_that_would_spoof_the_prompt(pair).await
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

// iroh specifics.

#[tokio::test]
async fn connection_over_loopback_is_direct() {
    let (sender, receiver) = connected_pair().await;
    assert_eq!(sender.connection_type_name(), "Direct");
    assert_eq!(receiver.connection_type_name(), "Direct");
}

#[tokio::test]
async fn a_peer_that_gave_up_is_reported_as_lost() {
    let (mut sender, mut receiver) = connected_pair().await;
    receiver.close(false).await;
    let result = sender.closed().await;
    assert!(
        matches!(result, Err(TransferError::Disconnected(_))),
        "{result:?}"
    );

    let (mut sender, mut receiver) = connected_pair().await;
    receiver.close(true).await;
    assert!(sender.closed().await.is_ok());
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
