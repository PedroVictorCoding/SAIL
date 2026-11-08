//! The owner's Moodle sign-in on disk: site URL and token in one `0600` JSON file inside
//! the private Moodle data directory.
//!
//! This follows the Spotify connector's pattern rather than the Guardian vault. The vault
//! is locked until the owner unlocks it, and the poll loop has to run unattended. The file
//! is written atomically (temp file, then rename), read only through
//! [`load`], and its token is a [`MoodleToken`], so it never prints.
use crate::contracts::{MoodleToken, SiteConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File name inside the Moodle data directory.
pub const CREDENTIALS_FILE: &str = "credentials.json";

/// Largest credentials file read. Real ones are about 200 bytes.
pub const MAX_CREDENTIALS_BYTES: u64 = 16 * 1024;

/// What is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub site: SiteConfig,
    pub token: MoodleToken,
    /// When the owner connected, for the status page.
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

/// Why the credentials couldn't be read or written. Messages never contain the token.
#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error("Moodle credentials: {0}")]
    Io(#[from] std::io::Error),
    #[error("Moodle credentials file is damaged")]
    Damaged,
    #[error("Moodle credentials file is too large")]
    TooLarge,
}

/// Path of the credentials file under `dir`.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(CREDENTIALS_FILE)
}

/// Read the stored credentials. `Ok(None)` when the owner hasn't connected. A damaged file
/// is an error rather than "not connected", so it can't silently hide a broken setup.
pub fn load(dir: &Path) -> Result<Option<Credentials>, CredentialsError> {
    let path = path_in(dir);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() {
        return Err(CredentialsError::Damaged);
    }
    if meta.len() > MAX_CREDENTIALS_BYTES {
        return Err(CredentialsError::TooLarge);
    }
    let bytes = std::fs::read(&path)?;
    let creds: Credentials =
        serde_json::from_slice(&bytes).map_err(|_| CredentialsError::Damaged)?;
    if !creds.token.is_plausible() {
        return Err(CredentialsError::Damaged);
    }
    Ok(Some(creds))
}

/// Store credentials, replacing any previous ones atomically. The file is `0600`.
pub fn save(dir: &Path, creds: &Credentials) -> Result<(), CredentialsError> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let path = path_in(dir);
    let tmp = dir.join(format!("{CREDENTIALS_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    let body = serde_json::to_vec_pretty(creds).map_err(std::io::Error::other)?;
    file.write_all(&body)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Forget the credentials. Not an error when there are none.
pub fn remove(dir: &Path) -> Result<(), CredentialsError> {
    match std::fs::remove_file(path_in(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sail-moodle-creds-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample() -> Credentials {
        Credentials {
            site: SiteConfig {
                base_url: url::Url::parse("https://moodle.example.test").unwrap(),
            },
            token: MoodleToken::new("0123456789abcdef0123456789abcdef"),
            connected_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn round_trip_is_private_and_removable() {
        let dir = temp_dir("roundtrip");
        assert!(load(&dir).unwrap().is_none());
        save(&dir, &sample()).unwrap();
        let loaded = load(&dir).unwrap().unwrap();
        assert_eq!(loaded.token, sample().token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path_in(&dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        save(&dir, &sample()).unwrap();
        remove(&dir).unwrap();
        remove(&dir).unwrap();
        assert!(load(&dir).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_file_is_an_error_not_a_sign_out() {
        let dir = temp_dir("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path_in(&dir), b"{not json").unwrap();
        assert!(matches!(load(&dir), Err(CredentialsError::Damaged)));
        let mut bad = sample();
        bad.token = MoodleToken::new("short");
        std::fs::write(path_in(&dir), serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(matches!(load(&dir), Err(CredentialsError::Damaged)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_never_show_the_token() {
        let dir = temp_dir("redact");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path_in(&dir), b"0123456789abcdef0123456789abcdef").unwrap();
        let err = load(&dir).unwrap_err().to_string();
        assert!(!err.contains("0123456789abcdef"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
