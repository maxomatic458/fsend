use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;

use super::*;
use crate::transfer::test_support::{Peers, transport_suite};

async fn local_endpoint() -> Endpoint {
    IrohTransfer::bind(presets::Minimal).await.unwrap()
}

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
            SocketAddr::new(ip, bound.port())
        })
        .collect();
    ConnectionInfo::Iroh {
        node_id: endpoint.id().to_string(),
        addrs,
        relays: vec![],
    }
}

async fn connected_pair() -> (IrohTransfer, IrohTransfer) {
    let sender = local_endpoint().await;
    let receiver = local_endpoint().await;
    let (info, receiver_info) = (loopback_info(&sender), loopback_info(&receiver));
    let (sender, receiver) = tokio::join!(
        IrohTransfer::accept(sender, &receiver_info),
        IrohTransfer::connect(receiver, &info)
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

// The transfer suite runs in full over memory; these depend on the transport.
transport_suite!(
    sends_a_file_larger_than_the_buffers,
    sends_a_directory_tree,
    interrupted_by_the_receiver_then_resumed,
    interrupted_by_the_sender_then_resumed,
    cancelled_by_the_receiver_then_resumed,
    cancelled_by_the_sender_then_resumed,
    a_file_changed_after_hashing_fails_verification,
    a_file_that_vanishes_fails_both_sides,
    messages_survive_the_wire,
    a_closed_peer_fails_the_control_stream,
);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing: cargo test --release -p fsend-core -- --ignored --nocapture"]
async fn throughput() {
    crate::transfer::test_support::suite::throughput(pair).await
}

#[tokio::test]
async fn connection_over_loopback_is_direct() {
    let (mut sender, mut receiver) = connected_pair().await;
    assert_eq!(sender.connection_kind().await, ConnectionKind::Direct);
    assert_eq!(receiver.connection_kind().await, ConnectionKind::Direct);
}

#[tokio::test]
async fn only_the_paired_receiver_gets_the_connection() {
    let sender = local_endpoint().await;
    let (receiver, stranger) = (local_endpoint().await, local_endpoint().await);
    let (info, receiver_info) = (loopback_info(&sender), loopback_info(&receiver));
    let receiver_id = receiver.id();
    let accepting = tokio::spawn(async move { IrohTransfer::accept(sender, &receiver_info).await });

    // Someone who found the sender's address gets in first, and is turned away.
    let stranger = IrohTransfer::connect(stranger, &info).await.unwrap();
    assert!(matches!(
        stranger.conn.closed().await,
        ConnectionError::ApplicationClosed(close) if close.error_code == VarInt::from(CLOSE_ERR)
    ));

    let receiver = IrohTransfer::connect(receiver, &info).await.unwrap();
    let sender = accepting.await.unwrap().unwrap();
    assert_eq!(sender.conn.remote_id(), receiver_id);
    assert_eq!(receiver.conn.remote_id(), sender.endpoint.id());
}

#[tokio::test]
async fn a_peer_that_gave_up_is_reported_as_lost() {
    let (mut sender, mut receiver) = connected_pair().await;
    receiver.close(false).await;
    let result = sender.closed().await;
    assert!(matches!(result, Err(Error::Disconnected(_))), "{result:?}");

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
    let ConnectionInfo::Iroh {
        node_id,
        addrs,
        relays,
    } = &info
    else {
        panic!("expected iroh info");
    };
    assert_eq!(node_id, &id.to_string());
    assert_eq!(addrs, &[ip]);
    assert_eq!(relays, &[relay.to_string()]);

    let back = IrohTransfer::addr_from_connection_info(&info).unwrap();
    assert_eq!(back, addr);
}

#[test]
fn connection_info_ignores_what_it_cannot_parse() {
    let id = iroh::SecretKey::generate().public();
    let info = ConnectionInfo::Iroh {
        node_id: id.to_string(),
        addrs: vec!["10.0.0.1:1".parse().unwrap(), "[::1]:2".parse().unwrap()],
        relays: vec!["not a url".into(), "https://relay.example.org./".into()],
    };
    let addr = IrohTransfer::addr_from_connection_info(&info).unwrap();
    assert_eq!(addr.id, id);
    assert_eq!(addr.ip_addrs().count(), 2);
    assert_eq!(addr.relay_urls().count(), 1);

    let bad_id = ConnectionInfo::Iroh {
        node_id: "zzz".into(),
        addrs: vec![],
        relays: vec![],
    };
    assert!(matches!(
        IrohTransfer::addr_from_connection_info(&bad_id),
        Err(Error::Protocol(_))
    ));

    let webrtc = ConnectionInfo::WebRtc { sdp: String::new() };
    assert!(matches!(
        IrohTransfer::addr_from_connection_info(&webrtc),
        Err(Error::Protocol(_))
    ));
}
