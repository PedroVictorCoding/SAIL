use chrono::{DateTime, Duration, Utc};
use ring::aead::{
    AES_256_GCM, Aad, BoundKey, NONCE_LEN, Nonce, NonceSequence, OpeningKey, SealingKey, UnboundKey,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use tracing::info;
use uuid::Uuid;
use zeroize::Zeroizing;

use sail_common::types::SubsystemId;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("Unknown or modified credential handle")]
    InvalidHandle,
    #[error("Only trusted Bridge connectors may hold credentials")]
    UntrustedConnector,
    #[error("Credential for {0:?}:{1} not found")]
    NotFound(SubsystemId, String),
    #[error("Access denied: caller {actual:?} is not owner {expected:?}")]
    OwnerMismatch {
        actual: SubsystemId,
        expected: SubsystemId,
    },
    #[error("Credential handle {0} has expired")]
    Expired(Uuid),
    #[error("Credential handle {0} has been revoked")]
    Revoked(Uuid),
    #[error("Requested scope '{requested}' is outside allowed scope '{allowed}'")]
    ScopeMismatch { requested: String, allowed: String },
    #[error("Vault cryptographic error: {0}")]
    CryptoError(String),
    #[error("Vault persistence error: {0}")]
    IoError(String),
}

/// Opaque, scoped credential handle returned to connectors. Raw secrets are NEVER exposed to models or untrusted callers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CredentialHandle {
    pub handle_id: Uuid,
    pub owner: SubsystemId,
    pub key: String,
    pub scope: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredRecord {
    pub owner: SubsystemId,
    pub key: String,
    pub secret: Zeroizing<String>,
    pub scope: String,
    pub created_at: DateTime<Utc>,
}

struct OneNonce(Option<[u8; NONCE_LEN]>);

impl NonceSequence for OneNonce {
    fn advance(&mut self) -> Result<Nonce, ring::error::Unspecified> {
        self.0
            .take()
            .ok_or(ring::error::Unspecified)
            .map(|n| Nonce::assume_unique_for_key(n))
    }
}

pub struct CredentialStore {
    secrets: RwLock<HashMap<(SubsystemId, String), StoredRecord>>,
    handles: RwLock<HashMap<Uuid, CredentialHandle>>,
    vault_path: Option<PathBuf>,
    encryption_key: Zeroizing<[u8; 32]>,
    operation: Mutex<()>,
}

impl CredentialStore {
    pub fn new() -> Self {
        // Ephemeral in-memory key if no vault path specified
        let rng = SystemRandom::new();
        let mut key = [0u8; 32];
        rng.fill(&mut key).expect("secure randomness unavailable");

        Self {
            secrets: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            vault_path: None,
            encryption_key: Zeroizing::new(key),
            operation: Mutex::new(()),
        }
    }

    /// Open or initialize an encrypted persistent credential vault backed by AES-256-GCM.
    pub fn open_persistent(vault_dir: &Path) -> Result<Self, CredentialError> {
        if !vault_dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(vault_dir)
                .map_err(|e| CredentialError::IoError(e.to_string()))?;
        }
        let directory =
            fs::symlink_metadata(vault_dir).map_err(|e| CredentialError::IoError(e.to_string()))?;
        if !directory.is_dir()
            || directory.uid() != unsafe { libc::geteuid() }
            || directory.mode() & 0o077 != 0
        {
            return Err(CredentialError::IoError(
                "Vault directory must be owner-only (0700), owned by daemon user and not a symlink"
                    .into(),
            ));
        }
        let key_path = vault_dir.join("vault.key");
        let enc_path = vault_dir.join("vault.enc");
        let mut key = [0u8; 32];
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&key_path)
        {
            Ok(mut file) => {
                use std::io::Write;
                SystemRandom::new()
                    .fill(&mut key)
                    .map_err(|_| CredentialError::CryptoError("RNG failed".into()))?;
                file.write_all(&key)
                    .and_then(|()| file.sync_all())
                    .map_err(|e| CredentialError::IoError(e.to_string()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                use std::io::Read;
                let mut file = secure_open(&key_path)?;
                if file
                    .metadata()
                    .map_err(|e| CredentialError::IoError(e.to_string()))?
                    .len()
                    != 32
                {
                    return Err(CredentialError::CryptoError("Invalid key length".into()));
                }
                file.read_exact(&mut key)
                    .map_err(|e| CredentialError::IoError(e.to_string()))?;
            }
            Err(error) => return Err(CredentialError::IoError(error.to_string())),
        }
        let store = Self {
            secrets: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            vault_path: Some(enc_path.clone()),
            encryption_key: Zeroizing::new(key),
            operation: Mutex::new(()),
        };

        if fs::symlink_metadata(&enc_path).is_ok() {
            store.load_from_disk(&enc_path)?;
        }

        Ok(store)
    }

    fn encrypt_records(
        &self,
        records: &HashMap<(SubsystemId, String), StoredRecord>,
    ) -> Result<Vec<u8>, CredentialError> {
        let list: Vec<&StoredRecord> = records.values().collect();
        let serialized = Zeroizing::new(
            serde_json::to_vec(&list).map_err(|e| CredentialError::CryptoError(e.to_string()))?,
        );

        let rng = SystemRandom::new();
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill(&mut nonce_bytes)
            .map_err(|_| CredentialError::CryptoError("RNG failed".into()))?;

        let unbound = UnboundKey::new(&AES_256_GCM, self.encryption_key.as_ref())
            .map_err(|_| CredentialError::CryptoError("Key init failed".into()))?;
        let mut sealing_key = SealingKey::new(unbound, OneNonce(Some(nonce_bytes)));

        let mut in_out = serialized;
        sealing_key
            .seal_in_place_append_tag(Aad::empty(), &mut *in_out)
            .map_err(|_| CredentialError::CryptoError("Encryption failed".into()))?;

        let mut output = Vec::with_capacity(NONCE_LEN + in_out.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&in_out);

        Ok(output)
    }

    fn decrypt_records(
        &self,
        ciphertext: &[u8],
    ) -> Result<HashMap<(SubsystemId, String), StoredRecord>, CredentialError> {
        if ciphertext.len() < NONCE_LEN + 16 {
            return Err(CredentialError::CryptoError("Ciphertext too short".into()));
        }

        let (nonce_bytes, payload) = ciphertext.split_at(NONCE_LEN);
        let mut nonce_arr = [0u8; NONCE_LEN];
        nonce_arr.copy_from_slice(nonce_bytes);

        let unbound = UnboundKey::new(&AES_256_GCM, self.encryption_key.as_ref())
            .map_err(|_| CredentialError::CryptoError("Key init failed".into()))?;
        let mut opening_key = OpeningKey::new(unbound, OneNonce(Some(nonce_arr)));

        let mut in_out = Zeroizing::new(payload.to_vec());
        let decrypted = opening_key
            .open_in_place(Aad::empty(), &mut in_out)
            .map_err(|_| CredentialError::CryptoError("Decryption failed or invalid tag".into()))?;

        let list: Vec<StoredRecord> = serde_json::from_slice(decrypted)
            .map_err(|e| CredentialError::CryptoError(format!("Corrupt vault record: {e}")))?;

        let mut records = HashMap::new();
        for r in list {
            records.insert((r.owner, r.key.clone()), r);
        }

        Ok(records)
    }

    fn persist_to_disk(&self) -> Result<(), CredentialError> {
        if let Some(path) = &self.vault_path {
            let guard = self.secrets.read().unwrap();
            let encrypted = self.encrypt_records(&guard)?;
            use std::io::Write;
            let tmp_path = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
            let result = (|| {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&tmp_path)?;
                file.write_all(&encrypted)?;
                file.sync_all()?;
                fs::rename(&tmp_path, path)?;
                std::fs::File::open(path.parent().unwrap())?.sync_all()
            })();
            if let Err(error) = result {
                let _ = fs::remove_file(&tmp_path);
                return Err(CredentialError::IoError(error.to_string()));
            }
        }
        Ok(())
    }

    fn load_from_disk(&self, path: &Path) -> Result<(), CredentialError> {
        let mut encrypted = Vec::new();
        use std::io::Read;
        secure_open(path)?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut encrypted)
            .map_err(|e| CredentialError::IoError(e.to_string()))?;
        if encrypted.len() > 16 * 1024 * 1024 {
            return Err(CredentialError::IoError("Vault exceeds size limit".into()));
        }
        let records = self.decrypt_records(&encrypted)?;
        let mut guard = self.secrets.write().unwrap();
        *guard = records;
        info!("CredentialStore successfully loaded and verified encrypted vault");
        Ok(())
    }

    /// Store a credential scoped strictly to an authorized subsystem and issue an opaque handle
    pub fn store_credential(
        &self,
        owner: SubsystemId,
        key: impl Into<String>,
        secret: impl Into<String>,
        scope: impl Into<String>,
        ttl: Duration,
    ) -> Result<CredentialHandle, CredentialError> {
        let _operation = self.operation.lock().unwrap();
        if owner != SubsystemId::Bridge {
            return Err(CredentialError::UntrustedConnector);
        }
        let key_str = key.into();
        let scope_str = scope.into();
        if scope_str.is_empty() || ttl <= Duration::zero() {
            return Err(CredentialError::InvalidHandle);
        }
        let now = Utc::now();

        self.invalidate_handles(owner, &key_str);
        let record = StoredRecord {
            owner,
            key: key_str.clone(),
            secret: Zeroizing::new(secret.into()),
            scope: scope_str.clone(),
            created_at: now,
        };

        let previous = self
            .secrets
            .write()
            .unwrap()
            .insert((owner, key_str.clone()), record);
        if let Err(error) = self.persist_to_disk() {
            let mut guard = self.secrets.write().unwrap();
            if let Some(previous) = previous {
                guard.insert((owner, key_str.clone()), previous);
            } else {
                guard.remove(&(owner, key_str.clone()));
            }
            return Err(error);
        }

        let handle = CredentialHandle {
            handle_id: Uuid::new_v4(),
            owner,
            key: key_str,
            scope: scope_str,
            expires_at: now + ttl,
        };

        {
            let mut guard = self.handles.write().unwrap();
            guard.retain(|_, handle| handle.expires_at > Utc::now());
            if guard.len() >= 4096 {
                return Err(CredentialError::IoError("credential handle limit".into()));
            }
            guard.insert(handle.handle_id, handle.clone());
        }

        Ok(handle)
    }

    /// Request an opaque scoped handle for an existing credential.
    pub fn get_handle(
        &self,
        requester: SubsystemId,
        key: &str,
        requested_scope: &str,
    ) -> Result<CredentialHandle, CredentialError> {
        let _operation = self.operation.lock().unwrap();
        if requester != SubsystemId::Bridge {
            return Err(CredentialError::UntrustedConnector);
        }
        let guard = self.secrets.read().unwrap();
        let record = guard
            .get(&(requester, key.to_string()))
            .ok_or_else(|| CredentialError::NotFound(requester, key.to_string()))?;

        if record.owner != requester {
            return Err(CredentialError::OwnerMismatch {
                actual: requester,
                expected: record.owner,
            });
        }

        if requested_scope.is_empty() || (record.scope != "*" && record.scope != requested_scope) {
            return Err(CredentialError::ScopeMismatch {
                requested: requested_scope.into(),
                allowed: record.scope.clone(),
            });
        }

        let handle = CredentialHandle {
            handle_id: Uuid::new_v4(),
            owner: requester,
            key: key.to_string(),
            scope: requested_scope.to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        };

        let mut handles_guard = self.handles.write().unwrap();
        handles_guard.retain(|_, handle| handle.expires_at > Utc::now());
        if handles_guard.len() >= 4096 {
            return Err(CredentialError::IoError("credential handle limit".into()));
        }
        handles_guard.insert(handle.handle_id, handle.clone());

        Ok(handle)
    }

    /// Execute a trusted connector closure with the raw secret, validating handle ownership and TTL.
    /// Secret memory is kept scoped and never exposed to model prompts or dialogue returns.
    pub fn execute_with_secret<F, R>(
        &self,
        handle: &CredentialHandle,
        caller: SubsystemId,
        action: F,
    ) -> Result<R, CredentialError>
    where
        F: FnOnce(&str) -> R,
    {
        let _operation = self.operation.lock().unwrap();
        if caller != SubsystemId::Bridge {
            return Err(CredentialError::UntrustedConnector);
        }
        let handles = self.handles.read().unwrap();
        if handles.get(&handle.handle_id) != Some(handle) {
            return Err(CredentialError::InvalidHandle);
        }
        if Utc::now() > handle.expires_at {
            return Err(CredentialError::Expired(handle.handle_id));
        }

        if handle.owner != caller {
            return Err(CredentialError::OwnerMismatch {
                actual: caller,
                expected: handle.owner,
            });
        }

        let guard = self.secrets.read().unwrap();
        let record = guard
            .get(&(handle.owner, handle.key.clone()))
            .ok_or_else(|| CredentialError::NotFound(handle.owner, handle.key.clone()))?;

        Ok(action(&record.secret))
    }

    /// Rotate credential value, invalidating previous secret
    pub fn rotate_credential(
        &self,
        owner: SubsystemId,
        key: &str,
        new_secret: impl Into<String>,
    ) -> Result<(), CredentialError> {
        let _operation = self.operation.lock().unwrap();
        self.invalidate_handles(owner, key);
        let previous = {
            let mut guard = self.secrets.write().unwrap();
            let record = guard
                .get_mut(&(owner, key.to_string()))
                .ok_or_else(|| CredentialError::NotFound(owner, key.into()))?;
            std::mem::replace(&mut record.secret, Zeroizing::new(new_secret.into()))
        };
        if let Err(error) = self.persist_to_disk() {
            self.secrets
                .write()
                .unwrap()
                .get_mut(&(owner, key.to_string()))
                .unwrap()
                .secret = previous;
            return Err(error);
        }
        info!(owner = ?owner, key, "Credential rotated in vault");
        Ok(())
    }

    /// Revoke a credential and all associated handles
    pub fn revoke_credential(&self, owner: SubsystemId, key: &str) -> Result<(), CredentialError> {
        let _operation = self.operation.lock().unwrap();
        self.invalidate_handles(owner, key);
        let previous = self
            .secrets
            .write()
            .unwrap()
            .remove(&(owner, key.to_string()));
        if let Err(error) = self.persist_to_disk() {
            if let Some(previous) = previous {
                self.secrets
                    .write()
                    .unwrap()
                    .insert((owner, key.to_string()), previous);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Revoke a specific active handle immediately
    fn invalidate_handles(&self, owner: SubsystemId, key: &str) {
        self.handles
            .write()
            .unwrap()
            .retain(|_, handle| handle.owner != owner || handle.key != key);
    }

    pub fn revoke_handle(&self, handle_id: Uuid) {
        let _operation = self.operation.lock().unwrap();
        self.handles.write().unwrap().remove(&handle_id);
    }

    /// Centralized structured log and trace redaction: scans text and replaces any known secret values with [REDACTED]
    pub fn redact_text(&self, input: &str) -> String {
        let guard = self.secrets.read().unwrap();
        let mut redacted = input.to_string();

        for record in guard.values() {
            if !record.secret.is_empty() && redacted.contains(record.secret.as_str()) {
                redacted = redacted.replace(record.secret.as_str(), "[REDACTED_SECRET]");
            }
        }

        redacted
    }
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

fn secure_open(path: &Path) -> Result<std::fs::File, CredentialError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| CredentialError::IoError(e.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|e| CredentialError::IoError(e.to_string()))?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(CredentialError::IoError(
            "Vault files must be private, regular, unlinked elsewhere and owner-owned".into(),
        ));
    }
    Ok(file)
}
