//! SAIL Mesh: let a SAIL host run some of its model inference on other machines the owner
//! trusts (Windows or Linux PCs with their own llama-server).
//!
//! Trust model:
//! - The **worker** creates a self-signed TLS certificate and a 256-bit shared secret when it
//!   is initialised. It prints both as a one-line [`PairingCode`].
//! - The **owner** copies that code to the host (`sail-core mesh pair <code>`). That copy is
//!   the consent step: nothing is discovered or paired automatically.
//! - The **host** then pins the worker's certificate by SHA-256 fingerprint (the worker
//!   proves who it is) and sends the secret as a bearer token inside that TLS session (the
//!   host proves it was paired). Prompts never cross the network in clear text.
//! - The worker only forwards a fixed allowlist of llama-server routes; it is not a shell,
//!   file or generic HTTP proxy.
//!
//! The worker's PC belongs to someone who also uses it: [`rest`] decides when the worker
//! steps aside (fully while gaming or making music, with exponential backoff while the PC
//! is otherwise busy), measured by [`probe`].
pub mod compute;
pub mod discovery;
pub mod hibernate;
pub mod identity;
pub mod pairing;
pub mod probe;
pub mod registry;
pub mod rest;
pub mod tls;
pub mod worker;

pub use pairing::PairingCode;
pub use registry::{Registry, WorkerRecord};
pub use sail_protocol::mesh::*;

/// Default TCP port a worker listens on.
pub const DEFAULT_PORT: u16 = 7710;

/// llama-server routes a worker forwards, with their method. Everything else is refused.
pub const FORWARDED_ROUTES: &[(&str, &str)] = &[
    ("GET", "/health"),
    ("GET", "/props"),
    ("POST", "/apply-template"),
    ("POST", "/tokenize"),
    ("POST", "/v1/chat/completions"),
];

/// Route of the worker's own self-description.
pub const HELLO_ROUTE: &str = "/mesh/v1/hello";

/// Errors from pairing, identity and transport setup.
#[derive(Debug)]
pub struct MeshError(pub String);
impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for MeshError {}
impl MeshError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
pub type Result<T> = std::result::Result<T, MeshError>;

/// Constant-time comparison for secrets.
pub(crate) fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Write a file only the current user can read (best effort on non-Unix systems, where the
/// user's profile directory is already private by default).
pub(crate) fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}
