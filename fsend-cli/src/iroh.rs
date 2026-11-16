use std::time::Duration;

use async_trait::async_trait;
use iroh::endpoint::{Connection, ConnectionError, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr, Watcher};

use crate::transfer::*;
use fsend_proto::pairing::ConnectionInfo;
use fsend_proto::peer::FSEND_ALPN;

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
        let (mut addrs, mut relays) = (Vec::new(), Vec::new());
        for a in &addr.addrs {
            match a {
                TransportAddr::Ip(ip) => addrs.push(*ip),
                TransportAddr::Relay(url) => relays.push(url.to_string()),
                _ => {}
            }
        }
        ConnectionInfo::Iroh {
            node_id: addr.id.to_string(),
            addrs,
            relays,
        }
    }

    pub fn addr_from_connection_info(info: &ConnectionInfo) -> Result<EndpointAddr, TransferError> {
        let ConnectionInfo::Iroh {
            node_id,
            addrs,
            relays,
        } = info
        else {
            return Err(TransferError::UnexpectedConnectionInfo);
        };
        let id = node_id.parse::<EndpointId>()?;
        let relays = relays
            .iter()
            .filter_map(|url| match url.parse::<RelayUrl>() {
                Ok(url) => Some(TransportAddr::Relay(url)),
                Err(_) => {
                    tracing::warn!("ignoring an unparsable relay URL {url:?}");
                    None
                }
            });
        let addrs = addrs.iter().copied().map(TransportAddr::Ip).chain(relays);
        Ok(EndpointAddr::from_parts(id, addrs))
    }

    /// Waits for the receiver `peer` told about over the paired relay; anyone else is turned away.
    pub async fn accept(endpoint: Endpoint, peer: &ConnectionInfo) -> Result<Self, TransferError> {
        let expected = Self::addr_from_connection_info(peer)?.id;
        let accept = async {
            loop {
                let incoming = endpoint
                    .accept()
                    .await
                    .ok_or(TransferError::EndpointClosed)?;
                let conn = match incoming.accept() {
                    Ok(connecting) => connecting.await,
                    Err(err) => Err(err.into()),
                };
                match conn {
                    Ok(conn) if conn.remote_id() == expected => {
                        return Ok::<_, TransferError>(conn);
                    }
                    Ok(conn) => {
                        tracing::warn!("turned away {}, not the paired receiver", conn.remote_id());
                        conn.close(VarInt::from(CLOSE_ERR), b"not the paired receiver");
                    }
                    Err(err) => tracing::warn!("a connection attempt failed: {err}"),
                }
            }
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
    fn connection_kind(&self) -> ConnectionKind {
        let paths = self.conn.paths();
        match paths.iter().find(|p| p.is_selected()) {
            Some(p) if p.is_ip() => ConnectionKind::Direct,
            Some(p) if p.is_relay() => ConnectionKind::Relay,
            _ => ConnectionKind::Unknown,
        }
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
