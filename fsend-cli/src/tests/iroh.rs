use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;

use super::*;
use crate::transfer::test_support::{Peers, transport_suite};

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

// The behaviour every transport must have.
transport_suite!();

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
