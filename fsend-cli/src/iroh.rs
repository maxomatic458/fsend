use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use iroh::endpoint::{Connection, ConnectionError, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr, Watcher};

use crate::relay::ConnectionInfo;
use crate::transfer::*;

/// Resolving Home relay and direct address timeout.
const ADDR_TIMEOUT: Duration = Duration::from_secs(15);

// Initial peer connection timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

const CLOSE_OK: u32 = 0;
const CLOSE_ERR: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Accepted the connection and opens the streams.
    Sender,
    /// Dialled the sender and accepts the streams.
    Receiver,
}

pub struct IrohTransfer {
    conn: Connection,
    endpoint: Endpoint,
    role: Role,
}

impl IrohTransfer {
    /// An iroh endpoint with the n0 defaults
    pub async fn create_endpoint() -> Result<Endpoint, TransferError> {
        Self::bind(presets::N0).await
    }

    pub async fn bind(preset: impl presets::Preset) -> Result<Endpoint, TransferError> {
        Ok(Endpoint::builder(preset)
            .alpns(vec![FSEND_ALPN.to_vec()])
            .bind()
            .await?)
    }

    /// The address to hand to the relay for the other peer.
    pub async fn local_connection_info(endpoint: &Endpoint) -> ConnectionInfo {
        let mut watcher = endpoint.watch_addr();
        let ready = async {
            loop {
                let addr = watcher.get();
                if addr.relay_urls().next().is_some() && addr.ip_addrs().next().is_some() {
                    return;
                }
                if watcher.updated().await.is_err() {
                    return;
                }
            }
        };

        if tokio::time::timeout(ADDR_TIMEOUT, ready).await.is_err() {
            tracing::warn!("sharing the address before the relay was reached");
        }

        Self::connection_info_from_addr(&endpoint.addr())
    }

    /// Convert the [`EndpointAddr`] into a [`ConnectionInfo`]
    pub fn connection_info_from_addr(addr: &EndpointAddr) -> ConnectionInfo {
        let addrs = addr
            .addrs
            .iter()
            .filter_map(|a| match a {
                TransportAddr::Ip(addr) => Some(addr.to_string()),
                TransportAddr::Relay(url) => Some(url.to_string()),
                _ => None,
            })
            .collect();
        ConnectionInfo::Iroh {
            node_id: addr.id.to_string(),
            addrs,
        }
    }

    pub fn addr_from_connection_info(info: &ConnectionInfo) -> Result<EndpointAddr, TransferError> {
        let ConnectionInfo::Iroh { node_id, addrs } = info else {
            return Err(TransferError::UnexpectedConnectionInfo);
        };
        let id = node_id.parse::<EndpointId>()?;
        let addrs = addrs.iter().filter_map(|a| {
            if let Ok(addr) = a.parse::<SocketAddr>() {
                Some(TransportAddr::Ip(addr))
            } else if let Ok(url) = a.parse::<RelayUrl>() {
                Some(TransportAddr::Relay(url))
            } else {
                tracing::warn!("ignoring unparsable peer address {a:?}");
                None
            }
        });
        Ok(EndpointAddr::from_parts(id, addrs))
    }

    /// Waits for the receiver to connect.
    pub async fn accept(endpoint: Endpoint) -> Result<Self, TransferError> {
        let accept = async {
            let incoming = endpoint
                .accept()
                .await
                .ok_or(TransferError::EndpointClosed)?;
            Ok::<_, TransferError>(incoming.accept()?.await?)
        };
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, accept)
            .await
            .map_err(|_| TransferError::PeerTimeout)??;
        tracing::info!("receiver connected");
        Ok(Self {
            conn,
            endpoint,
            role: Role::Sender,
        })
    }

    /// Connects to the sender.
    pub async fn connect(endpoint: Endpoint, peer: ConnectionInfo) -> Result<Self, TransferError> {
        let addr = Self::addr_from_connection_info(&peer)?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(addr, FSEND_ALPN))
            .await
            .map_err(|_| TransferError::PeerTimeout)??;
        tracing::info!("connected to sender");
        Ok(Self {
            conn,
            endpoint,
            role: Role::Receiver,
        })
    }

    /// The next bidirectional stream.
    async fn stream(&self) -> Result<Box<dyn ByteStream>, TransferError> {
        let (send, recv) = match self.role {
            Role::Sender => self.conn.open_bi().await?,
            Role::Receiver => self.conn.accept_bi().await?,
        };
        Ok(Box::new(tokio::io::join(recv, send)))
    }
}

#[async_trait]
impl Transport for IrohTransfer {
    fn connection_type_name(&self) -> String {
        let paths = self.conn.paths();
        match paths.iter().find(|p| p.is_selected()) {
            Some(p) if p.is_ip() => "Direct",
            Some(p) if p.is_relay() => "Relay",
            Some(_) => "Unknown",
            None => "None",
        }
        .into()
    }

    async fn control(&mut self) -> Result<Box<dyn ByteStream>, TransferError> {
        self.stream().await
    }

    async fn data(&mut self) -> Result<Box<dyn ByteStream>, TransferError> {
        self.stream().await
    }

    /// The close code says whether the peer was done or gave up.
    async fn closed(&mut self) -> Result<(), TransferError> {
        match self.conn.closed().await {
            ConnectionError::ApplicationClosed(close)
                if close.error_code == VarInt::from(CLOSE_OK) =>
            {
                Ok(())
            }
            reason => Err(TransferError::Disconnected(reason.to_string())),
        }
    }

    async fn close(&mut self, ok: bool) {
        let code = if ok { CLOSE_OK } else { CLOSE_ERR };
        self.conn.close(VarInt::from(code), b"done");
        self.endpoint.close().await;
    }
}

#[cfg(test)]
#[path = "tests/iroh.rs"]
mod tests;
