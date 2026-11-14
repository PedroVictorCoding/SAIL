//! LAN discovery: workers say "I am here" without giving anything away.
//!
//! A worker broadcasts an [`Announcement`] on [`DISCOVERY_PORT`] so the owner can see which
//! PCs are running the worker. Discovery never pairs and never carries the shared secret:
//! the owner still pastes the pairing code from the worker's console.
use crate::pairing::valid_name;
use sail_protocol::mesh::MESH_VERSION;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
use tokio::net::UdpSocket;

/// UDP port workers announce on and the host listens on.
pub const DISCOVERY_PORT: u16 = 7711;

/// Packet prefix; a version bump changes it so old hosts and workers never misread each other.
const PREFIX: &[u8] = b"SAILMESH1 ";

/// Largest accepted packet. Announcements are tiny; anything bigger is not one.
const MAX_PACKET: usize = 512;

/// Most workers one listen window remembers; the rest are ignored.
const MAX_WORKERS: usize = 256;

/// Longest accepted worker id.
const MAX_WORKER_ID_CHARS: usize = 64;

/// What a worker broadcasts. Never contains the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announcement {
    /// [`MESH_VERSION`].
    pub version: u16,
    /// Stable random identity created when the worker was initialised (≤ 64 chars).
    pub worker_id: String,
    /// Owner-chosen display name; [`valid_name`] rules.
    pub name: String,
    /// The worker's TLS port, nonzero.
    pub port: u16,
    /// Lower-case hex SHA-256 of the worker's certificate (64 digits).
    pub fingerprint: String,
}

/// A worker heard on the LAN.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovered {
    /// `<source ip>:<announcement.port>` (IPv6 in brackets): the address to pair with.
    pub address: String,
    pub announcement: Announcement,
}

/// Whether `a` is worth sending or believing.
fn valid(a: &Announcement) -> bool {
    a.version == MESH_VERSION
        && !a.worker_id.is_empty()
        && a.worker_id.chars().count() <= MAX_WORKER_ID_CHARS
        && !a.worker_id.chars().any(char::is_control)
        && valid_name(&a.name).is_ok_and(|name| name == a.name)
        && a.port != 0
        && a.fingerprint.len() == 64
        && a.fingerprint
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Pure. `b"SAILMESH1 "` followed by compact JSON; always ≤ 512 bytes (Err if it would not be).
pub fn encode(a: &Announcement) -> Result<Vec<u8>, String> {
    if !valid(a) {
        return Err("invalid worker announcement".to_owned());
    }
    let json = serde_json::to_vec(a).map_err(|e| format!("cannot encode announcement: {e}"))?;
    let mut packet = Vec::with_capacity(PREFIX.len() + json.len());
    packet.extend_from_slice(PREFIX);
    packet.extend_from_slice(&json);
    if packet.len() > MAX_PACKET {
        return Err(format!(
            "announcement is {} bytes; at most {MAX_PACKET} fit on the wire",
            packet.len()
        ));
    }
    Ok(packet)
}

/// Pure. None for a wrong prefix or version, oversize (> 512 bytes), malformed JSON,
/// unknown fields, an invalid name/fingerprint/port/worker_id, or any `key`/`secret` field.
pub fn decode(bytes: &[u8]) -> Option<Announcement> {
    if bytes.len() > MAX_PACKET || !bytes.starts_with(PREFIX) {
        return None;
    }
    let announcement: Announcement = serde_json::from_slice(&bytes[PREFIX.len()..]).ok()?;
    valid(&announcement).then_some(announcement)
}

/// Send `a` every `every` to `target` (normally `255.255.255.255:DISCOVERY_PORT`) until
/// `stop` resolves. Send errors are logged at debug level and never end the loop.
pub async fn announce(
    a: Announcement,
    target: SocketAddr,
    every: Duration,
    stop: impl Future<Output = ()> + Send,
) {
    let packet = match encode(&a) {
        Ok(packet) => packet,
        Err(e) => {
            tracing::warn!(error = %e, "not announcing the worker");
            return;
        }
    };
    let bind = if target.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let socket = match UdpSocket::bind(bind).await {
        Ok(socket) => socket,
        Err(e) => {
            tracing::debug!(error = %e, "cannot open the discovery socket");
            return;
        }
    };
    if target.is_ipv4()
        && let Err(e) = socket.set_broadcast(true)
    {
        tracing::debug!(error = %e, "cannot broadcast on the discovery socket");
    }
    let pause = every.max(Duration::from_millis(1));
    tokio::pin!(stop);
    loop {
        tokio::select! {
            biased;
            () = &mut stop => return,
            sent = socket.send_to(&packet, target) => {
                if let Err(e) = sent {
                    tracing::debug!(error = %e, "could not announce the worker");
                }
            }
        }
        tokio::select! {
            biased;
            () = &mut stop => return,
            () = tokio::time::sleep(pause) => {}
        }
    }
}

/// Listen on `bind` for `window`; keep the latest announcement per `worker_id`; return
/// them sorted by name, then address. At most 256 workers are kept.
pub async fn listen(bind: SocketAddr, window: Duration) -> io::Result<Vec<Discovered>> {
    let socket = UdpSocket::bind(bind).await?;
    let deadline = tokio::time::Instant::now() + window;
    let mut heard: HashMap<String, Discovered> = HashMap::new();
    let mut buf = vec![0u8; 2048];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, from))) => {
                let Some(announcement) = decode(&buf[..len]) else {
                    continue;
                };
                if heard.len() >= MAX_WORKERS && !heard.contains_key(&announcement.worker_id) {
                    continue;
                }
                heard.insert(
                    announcement.worker_id.clone(),
                    Discovered {
                        address: address_of(from.ip(), announcement.port),
                        announcement,
                    },
                );
            }
            Ok(Err(e)) => {
                // A hostile or half-connected network can make a single receive fail; the
                // window still lasts as long as it was asked to.
                tracing::debug!(error = %e, "discovery receive failed");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(_) => break,
        }
    }
    let mut found: Vec<Discovered> = heard.into_values().collect();
    found.sort_by(|a, b| {
        a.announcement
            .name
            .cmp(&b.announcement.name)
            .then_with(|| a.address.cmp(&b.address))
    });
    Ok(found)
}

/// `<ip>:<port>` with IPv6 kept in brackets, as a URL authority needs.
fn address_of(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(ip) => format!("{ip}:{port}"),
        IpAddr::V6(ip) => format!("[{ip}]:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Announcement {
        Announcement {
            version: MESH_VERSION,
            worker_id: "6f1c2f4e-0d1a-4f1f-9a4e-1b2c3d4e5f60".into(),
            name: name.into(),
            port: 7710,
            fingerprint: "ab".repeat(32),
        }
    }

    #[test]
    fn round_trips_and_is_pure() {
        let announcement = sample("desktop");
        let first = encode(&announcement).unwrap();
        let second = encode(&announcement).unwrap();
        assert_eq!(first, second);
        assert_eq!(decode(&first), Some(announcement.clone()));
        assert_eq!(decode(&first), decode(&first));
        assert!(first.starts_with(PREFIX));
    }

    fn with_fields(extra: &str) -> Vec<u8> {
        let good = encode(&sample("desktop")).unwrap();
        let mut json: serde_json::Value = serde_json::from_slice(&good[PREFIX.len()..]).unwrap();
        for (key, value) in serde_json::from_str::<serde_json::Value>(extra)
            .expect("test JSON")
            .as_object()
            .expect("test object")
        {
            json[key] = value.clone();
        }
        let mut packet = PREFIX.to_vec();
        packet.extend_from_slice(&serde_json::to_vec(&json).expect("test JSON"));
        packet
    }

    fn prefixed(body: &[u8]) -> Vec<u8> {
        let mut packet = PREFIX.to_vec();
        packet.extend_from_slice(body);
        packet
    }

    #[test]
    fn rejects_hostile_packets() {
        let good = encode(&sample("desktop")).unwrap();
        let mut oversize = good.clone();
        oversize.resize(MAX_PACKET + 1, b' ');
        let mut truncated = good.clone();
        truncated.truncate(PREFIX.len() + 3);
        let wrong_prefix = prefixed(&good[PREFIX.len()..]);
        let mut wrong_prefix = wrong_prefix;
        wrong_prefix[8] = b'2';
        let cases = vec![
            Vec::new(),
            PREFIX.to_vec(),
            truncated,
            oversize,
            with_fields(r#"{"key":"secret"}"#),
            with_fields(r#"{"secret":"secret"}"#),
            with_fields(r#"{"extra":1}"#),
            with_fields(r#"{"port":0}"#),
            with_fields(r#"{"version":2}"#),
            with_fields(r#"{"fingerprint":"ab"}"#),
            with_fields(r#"{"fingerprint":"AB"}"#),
            with_fields(r#"{"name":"bad name"}"#),
            with_fields(r#"{"worker_id":""}"#),
            prefixed(br#"{"version":1,"worker_id":"x","name":"a","port":7710,"fingerprint":""}"#),
            wrong_prefix,
        ];
        for (index, case) in cases.iter().enumerate() {
            assert_eq!(decode(case), None, "case {index}");
        }
        assert_eq!(decode(&good), Some(sample("desktop")));
    }

    #[test]
    fn rejects_non_utf8() {
        let mut packet = encode(&sample("desktop")).unwrap();
        let len = packet.len();
        packet[len - 2] = 0xff;
        assert_eq!(decode(&packet), None);
    }

    #[test]
    fn encode_refuses_oversize_and_invalid() {
        let mut long = sample("desktop");
        long.worker_id = "x".repeat(MAX_WORKER_ID_CHARS + 1);
        assert!(encode(&long).is_err());
        let mut long = sample("desktop");
        long.worker_id = "x".repeat(MAX_WORKER_ID_CHARS);
        assert!(encode(&long).is_ok());
        let mut name = sample("desktop");
        name.name = "bad name".into();
        assert!(encode(&name).is_err());
        let mut version = sample("desktop");
        version.version = 2;
        assert!(encode(&version).is_err());
        let mut port = sample("desktop");
        port.port = 0;
        assert!(encode(&port).is_err());
    }
}
