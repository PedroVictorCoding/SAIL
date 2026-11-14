//! The one-line code a worker prints and the owner gives to the host.
//!
//! Format: `sailmesh://<host>:<port>/?name=<name>&fp=<sha256 hex>&key=<secret>`
//! The code contains the worker's secret: treat it like a password and pair promptly.
use crate::{MeshError, Result};

const SCHEME: &str = "sailmesh";

#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode {
    /// `host:port` the host connects to (IPv6 hosts in brackets).
    pub address: String,
    pub name: String,
    /// Lower-case hex SHA-256 of the worker's DER certificate.
    pub fingerprint: String,
    /// URL-safe base64 shared secret.
    pub secret: String,
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCode")
            .field("address", &self.address)
            .field("name", &self.name)
            .field("fingerprint", &self.fingerprint)
            .field("secret", &"…")
            .finish()
    }
}

impl PairingCode {
    pub fn parse(code: &str) -> Result<Self> {
        let url = url::Url::parse(code.trim())
            .map_err(|e| MeshError::new(format!("Not a SAIL Mesh pairing code: {e}")))?;
        if url.scheme() != SCHEME {
            return Err(MeshError::new(
                "A pairing code starts with sailmesh:// (copy it from the worker)",
            ));
        }
        let host = url
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| MeshError::new("Pairing code has no worker address"))?;
        let port = url
            .port()
            .ok_or_else(|| MeshError::new("Pairing code has no worker port"))?;
        let mut name = None;
        let mut fingerprint = None;
        let mut secret = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "name" => name = Some(value.into_owned()),
                "fp" => fingerprint = Some(value.to_ascii_lowercase()),
                "key" => secret = Some(value.into_owned()),
                _ => {}
            }
        }
        let fingerprint =
            fingerprint.ok_or_else(|| MeshError::new("Pairing code has no fingerprint"))?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(MeshError::new(
                "Pairing code fingerprint must be 64 hex digits",
            ));
        }
        let secret = secret.ok_or_else(|| MeshError::new("Pairing code has no key"))?;
        if crate::identity::decode_secret(&secret).is_none() {
            return Err(MeshError::new("Pairing code key is malformed"));
        }
        let name = valid_name(&name.unwrap_or_else(|| host.to_owned()))?;
        Ok(Self {
            address: format!("{host}:{port}"),
            name,
            fingerprint,
            secret,
        })
    }

    pub fn render(&self) -> String {
        let mut url = url::Url::parse(&format!("{SCHEME}://{}/", self.address))
            .expect("worker address is a valid authority");
        url.query_pairs_mut()
            .append_pair("name", &self.name)
            .append_pair("fp", &self.fingerprint)
            .append_pair("key", &self.secret);
        url.to_string()
    }

    /// The worker's base URL for HTTPS requests.
    pub fn base_url(&self) -> String {
        format!("https://{}", self.address)
    }
}

/// Worker names are shown in the UI and used on the CLI: short, no spaces or markup.
pub fn valid_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 40
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(MeshError::new(
            "Worker names use 1-40 letters, digits, '-', '_' or '.'",
        ));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PairingCode {
        PairingCode {
            address: "192.168.1.40:7710".into(),
            name: "desktop".into(),
            fingerprint: "ab".repeat(32),
            secret: crate::identity::new_secret(),
        }
    }

    #[test]
    fn debug_hides_the_secret() {
        let code = sample();
        assert!(!format!("{code:?}").contains(&code.secret));
    }

    #[test]
    fn round_trips() {
        let code = sample();
        let text = code.render();
        assert!(text.starts_with("sailmesh://192.168.1.40:7710/?"));
        assert_eq!(PairingCode::parse(&text).unwrap(), code);
    }

    #[test]
    fn ipv6_round_trips() {
        let code = PairingCode {
            address: "[fd00::5]:7710".into(),
            ..sample()
        };
        assert_eq!(PairingCode::parse(&code.render()).unwrap(), code);
    }

    #[test]
    fn rejects_bad_codes() {
        let good = sample().render();
        for bad in [
            "https://192.168.1.40:7710/?fp=00",
            "sailmesh://192.168.1.40/?name=a",
            &good.replace(&"ab".repeat(32), "abc"),
            &good.replace("name=desktop", "name=bad%20name"),
            "sailmesh://192.168.1.40:7710/?name=x&fp=00",
        ] {
            assert!(PairingCode::parse(bad).is_err(), "{bad}");
        }
        let no_key = format!("sailmesh://h:1/?name=a&fp={}", "ab".repeat(32));
        assert!(PairingCode::parse(&no_key).is_err());
    }
}
