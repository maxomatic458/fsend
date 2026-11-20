use std::time::Duration;

use async_trait::async_trait;
use fsend_proto::pairing::ConnectionInfo;
use fsend_proto::peer::FSEND_ALPN;
use iroh::endpoint::{BindError, ConnectError, Connection, ConnectionError, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr, Watcher};

use crate::Error;
use crate::transfer::{ByteStream, ConnectionKind, Transport};

/// For a home relay and a direct address.
const ADDR_TIMEOUT: Duration = Duration::from_secs(15);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

const PEER_TIMEOUT: &str = "the peer did not connect in time";

const CLOSE_OK: u32 = 0;
const CLOSE_ERR: u32 = 1;

impl From<ConnectionError> for Error {
    fn from(e: ConnectionError) -> Self {
        Self::Disconnected(e.to_string())
    }
}

impl From<BindError> for Error {
    fn from(e: BindError) -> Self {
        Self::Connect(e.to_string())
    }
}

impl From<ConnectError> for Error {
    fn from(e: ConnectError) -> Self {
        Self::Connect(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Sender,
    Receiver,
}

pub struct IrohTransfer {
    conn: Connection,
    endpoint: Endpoint,
    role: Role,
}

impl IrohTransfer {
    /// An endpoint with the n0 defaults.
    pub async fn create_endpoint() -> Result<Endpoint, Error> {
        Self::bind(presets::N0).await
    }

    pub async fn bind(preset: impl presets::Preset) -> Result<Endpoint, Error> {
        Ok(Endpoint::builder(preset)
            .alpns(vec![FSEND_ALPN.to_vec()])
            .bind()
            .await?)
    }

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

    fn connection_info_from_addr(addr: &EndpointAddr) -> ConnectionInfo {
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

    fn addr_from_connection_info(info: &ConnectionInfo) -> Result<EndpointAddr, Error> {
        let ConnectionInfo::Iroh {
            node_id,
            addrs,
            relays,
        } = info
        else {
            return Err(Error::Protocol(
                "connection info for another transport".into(),
            ));
        };
        let id = (node_id.parse::<EndpointId>())
            .map_err(|e| Error::Protocol(format!("node id: {e}")))?;
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

    /// Anyone but `peer` is turned away.
    pub async fn accept(endpoint: Endpoint, peer: &ConnectionInfo) -> Result<Self, Error> {
        let expected = Self::addr_from_connection_info(peer)?.id;
        let accept = async {
            loop {
                let incoming = endpoint
                    .accept()
                    .await
                    .ok_or(Error::Internal("the endpoint closed"))?;
                let conn = match incoming.accept() {
                    Ok(connecting) => connecting.await,
                    Err(err) => Err(err.into()),
                };
                match conn {
                    Ok(conn) if conn.remote_id() == expected => {
                        return Ok::<_, Error>(conn);
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
            .map_err(|_| Error::Timeout(PEER_TIMEOUT))??;
        tracing::info!("receiver connected");
        Ok(Self {
            conn,
            endpoint,
            role: Role::Sender,
        })
    }

    pub async fn connect(endpoint: Endpoint, peer: &ConnectionInfo) -> Result<Self, Error> {
        let addr = Self::addr_from_connection_info(peer)?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(addr, FSEND_ALPN))
            .await
            .map_err(|_| Error::Timeout(PEER_TIMEOUT))??;
        tracing::info!("connected to sender");
        Ok(Self {
            conn,
            endpoint,
            role: Role::Receiver,
        })
    }

    /// The peer only sees a stream once `writer` writes to it.
    async fn stream(&self, writer: Role) -> Result<Box<dyn ByteStream>, Error> {
        let (send, recv) = if self.role == writer {
            self.conn.open_bi().await?
        } else {
            self.conn.accept_bi().await?
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

    async fn control(&mut self) -> Result<Box<dyn ByteStream>, Error> {
        self.stream(Role::Receiver).await
    }

    async fn data(&mut self) -> Result<Box<dyn ByteStream>, Error> {
        self.stream(Role::Sender).await
    }

    /// The close code says whether the peer was done or gave up.
    async fn closed(&mut self) -> Result<(), Error> {
        match self.conn.closed().await {
            ConnectionError::ApplicationClosed(close)
                if close.error_code == VarInt::from(CLOSE_OK) =>
            {
                Ok(())
            }
            reason => Err(Error::Disconnected(reason.to_string())),
        }
    }

    async fn close(&mut self, ok: bool) {
        let code = if ok { CLOSE_OK } else { CLOSE_ERR };
        let reason: &[u8] = if ok { b"done" } else { b"failed" };
        self.conn.close(VarInt::from(code), reason);
        self.endpoint.close().await;
    }
}

#[cfg(test)]
#[path = "tests/iroh.rs"]
mod tests;
