//! A worker's persistent identity: name, id, TLS certificate and shared secret.
use crate::{MeshError, PairingCode, Result, write_private};
use base64::Engine;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A fresh 256-bit secret, URL-safe base64.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("operating system random source");
    B64.encode(bytes)
}

/// The secret's bytes, when it is a well-formed 256-bit secret.
pub fn decode_secret(secret: &str) -> Option<Vec<u8>> {
    B64.decode(secret).ok().filter(|bytes| bytes.len() == 32)
}

/// Lower-case hex SHA-256 of a DER certificate.
pub fn fingerprint(cert_der: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, cert_der)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where a worker keeps its identity: `SAIL_MESH_WORKER_DIR`, else the user's config
/// directory (`%APPDATA%\sail-mesh` on Windows, `~/.config/sail-mesh` elsewhere).
pub fn default_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SAIL_MESH_WORKER_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("sail-mesh");
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config).join("sail-mesh");
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("sail-mesh")
}

#[derive(Serialize, Deserialize)]
struct Meta {
    worker_id: String,
    name: String,
}

/// Everything a worker needs to prove who it is and to recognise its paired host.
pub struct WorkerIdentity {
    pub dir: PathBuf,
    pub worker_id: String,
    pub name: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub secret: String,
}

impl WorkerIdentity {
    /// Create a new identity in `dir`. Refuses to overwrite an existing one, because that
    /// would silently unpair the host.
    pub fn create(dir: &Path, name: &str) -> Result<Self> {
        if dir.join("worker.json").exists() {
            return Err(MeshError::new(format!(
                "A worker identity already exists in {}; use it, or delete the folder to start over",
                dir.display()
            )));
        }
        let name = crate::pairing::valid_name(name)?;
        let certified = rcgen::generate_simple_self_signed(vec!["sail-mesh-worker".to_owned()])
            .map_err(|e| MeshError::new(format!("Could not create a TLS certificate: {e}")))?;
        let identity = Self {
            dir: dir.to_owned(),
            worker_id: uuid::Uuid::new_v4().to_string(),
            name,
            cert_der: certified.cert.der().to_vec(),
            key_der: certified.signing_key.serialize_der(),
            secret: new_secret(),
        };
        identity.save()?;
        Ok(identity)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let read = |file: &str| {
            std::fs::read(dir.join(file)).map_err(|e| {
                MeshError::new(format!(
                    "Cannot read {} ({e}); run `sail-mesh-worker init` first",
                    dir.join(file).display()
                ))
            })
        };
        let meta: Meta = serde_json::from_slice(&read("worker.json")?)
            .map_err(|e| MeshError::new(format!("worker.json is corrupt: {e}")))?;
        let secret = String::from_utf8(read("secret")?)
            .map_err(|_| MeshError::new("secret file is corrupt"))?
            .trim()
            .to_owned();
        if decode_secret(&secret).is_none() {
            return Err(MeshError::new("secret file is corrupt"));
        }
        Ok(Self {
            dir: dir.to_owned(),
            worker_id: meta.worker_id,
            name: meta.name,
            cert_der: read("cert.der")?,
            key_der: read("key.der")?,
            secret,
        })
    }

    /// Replace the shared secret. The host must pair again with the new code.
    pub fn rotate_secret(&mut self) -> Result<()> {
        self.secret = new_secret();
        self.save()
    }

    fn save(&self) -> Result<()> {
        let io = |e: std::io::Error| MeshError::new(format!("Cannot save worker identity: {e}"));
        let meta = serde_json::to_vec_pretty(&Meta {
            worker_id: self.worker_id.clone(),
            name: self.name.clone(),
        })
        .expect("serializable");
        write_private(&self.dir.join("worker.json"), &meta).map_err(io)?;
        write_private(&self.dir.join("cert.der"), &self.cert_der).map_err(io)?;
        write_private(&self.dir.join("key.der"), &self.key_der).map_err(io)?;
        write_private(&self.dir.join("secret"), self.secret.as_bytes()).map_err(io)?;
        Ok(())
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert_der)
    }

    /// The code the owner gives to the host, for a worker reachable at `address`.
    pub fn pairing_code(&self, address: &str) -> PairingCode {
        PairingCode {
            address: address.to_owned(),
            name: self.name.clone(),
            fingerprint: self.fingerprint(),
            secret: self.secret.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_load_and_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let created = WorkerIdentity::create(dir.path(), "desk").unwrap();
        assert!(WorkerIdentity::create(dir.path(), "desk").is_err());
        let mut loaded = WorkerIdentity::load(dir.path()).unwrap();
        assert_eq!(loaded.worker_id, created.worker_id);
        assert_eq!(loaded.fingerprint(), created.fingerprint());
        assert_eq!(loaded.secret, created.secret);
        loaded.rotate_secret().unwrap();
        assert_ne!(
            WorkerIdentity::load(dir.path()).unwrap().secret,
            created.secret
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("key.der"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "private key must not be group/world readable"
            );
        }
    }
}
