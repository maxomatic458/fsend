use std::{
    collections::HashMap,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
};

use axum::http::{HeaderMap, HeaderName};
use tokio::time::Instant;

/// Addresses tracked before those without a connection are forgotten.
const MAX_TRACKED: usize = 100_000;

/// What each address may use of the server at once and per minute.
pub(crate) struct Limits {
    clients: Mutex<HashMap<IpAddr, Client>>,
    connections_per_ip: usize,
    sessions_per_minute: u32,
}

/// What one address uses.
struct Client {
    connections: usize,
    /// Sessions it may still create or join; refills over a minute.
    allowance: f64,
    refilled: Instant,
}

impl Client {
    fn refill(&mut self, per_minute: u32) {
        let now = Instant::now();
        let refilled =
            now.duration_since(self.refilled).as_secs_f64() * f64::from(per_minute) / 60.0;
        self.allowance = (self.allowance + refilled).min(f64::from(per_minute));
        self.refilled = now;
    }
}

/// Counts a connection of its address while it lives.
pub(crate) struct Connected {
    limits: Arc<Limits>,
    ip: IpAddr,
}

impl Drop for Connected {
    fn drop(&mut self) {
        let per_minute = self.limits.sessions_per_minute;
        let mut clients = self.limits.clients();
        let Some(client) = clients.get_mut(&self.ip) else {
            return;
        };
        client.connections -= 1;
        client.refill(per_minute);
        if client.connections == 0 && client.allowance >= f64::from(per_minute) {
            clients.remove(&self.ip);
        }
    }
}

impl Limits {
    pub(crate) fn new(connections_per_ip: usize, sessions_per_minute: u32) -> Self {
        Self {
            clients: Mutex::default(),
            connections_per_ip,
            sessions_per_minute,
        }
    }

    fn clients(&self) -> MutexGuard<'_, HashMap<IpAddr, Client>> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A connection from `ip`, unless it has too many open.
    pub(crate) fn connect(self: &Arc<Self>, ip: IpAddr) -> Option<Connected> {
        let mut clients = self.clients();
        if clients.len() >= MAX_TRACKED {
            clients.retain(|_, client| client.connections > 0);
        }
        let client = clients.entry(ip).or_insert_with(|| Client {
            connections: 0,
            allowance: f64::from(self.sessions_per_minute),
            refilled: Instant::now(),
        });
        if client.connections >= self.connections_per_ip {
            return None;
        }
        client.connections += 1;
        Some(Connected {
            limits: self.clone(),
            ip,
        })
    }

    /// Whether the address of `connected` may create or join another session now.
    pub(crate) fn attempt(&self, connected: &Connected) -> bool {
        let mut clients = self.clients();
        let Some(client) = clients.get_mut(&connected.ip) else {
            return false;
        };
        client.refill(self.sessions_per_minute);
        if client.allowance < 1.0 {
            return false;
        }
        client.allowance -= 1.0;
        true
    }
}

/// The client's address.
pub(crate) fn client_ip(
    header: Option<&HeaderName>,
    headers: &HeaderMap,
    addr: SocketAddr,
) -> IpAddr {
    let ip = header
        .and_then(|name| headers.get(name)?.to_str().ok()?.trim().parse().ok())
        .unwrap_or(addr.ip());
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !u128::from(u64::MAX))),
        },
        v4 => v4,
    }
}
