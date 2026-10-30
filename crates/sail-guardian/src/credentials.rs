use chrono::{DateTime, Duration, Utc};
use ring::aead::{Aad, BoundKey, Nonce, NonceSequence, OpeningKey, SealingKey, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use sail_common::types::SubsystemId;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
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
    ScopeMismatch {
        requested: String,
        allowed: String,
    },
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRecord {
    pub owner: SubsystemId,
    pub key: String,
    pub secret: String,
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
    revoked_handles: RwLock<HashSet<Uuid>>,
    vault_path: Option<PathBuf>,
    encryption_key: [u8; 32],
}

impl CredentialStore {
    pub fn new() -> Self {
        // Ephemeral in-memory key if no vault path specified
        let rng = SystemRandom::new();
        let mut key = [0u8; 32];
        let _ = rng.fill(&mut key);

        Self {
            secrets: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            revoked_handles: RwLock::new(HashSet::new()),
            vault_path: None,
            encryption_key: key,
        }
    }

    /// Open or initialize an encrypted persistent credential vault backed by AES-256-GCM.
    pub fn open_persistent(vault_dir: &Path) -> Result<Self, CredentialError> {
        let _ = fs::create_dir_all(vault_dir);
        let key_path = vault_dir.join("vault.key");
        let enc_path = vault_dir.join("vault.enc");

        let mut key = [0u8; 32];
        if key_path.exists() {
            let key_bytes = fs::read(&key_path)
                .map_err(|e| CredentialError::IoError(format!("Failed to read vault key: {e}")))?;
            if key_bytes.len() != 32 {
                return Err(CredentialError::CryptoError("Invalid key length".into()));
            }
            key.copy_from_slice(&key_bytes);
        } else {
            let rng = SystemRandom::new();
            rng.fill(&mut key)
                .map_err(|_| CredentialError::CryptoError("Failed to generate random vault key".into()))?;
            fs::write(&key_path, key)
                .map_err(|e| CredentialError::IoError(format!("Failed to persist vault key: {e}")))?;
        }

        let store = Self {
            secrets: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            revoked_handles: RwLock::new(HashSet::new()),
            vault_path: Some(enc_path.clone()),
            encryption_key: key,
        };

        if enc_path.exists() {
            store.load_from_disk(&enc_path)?;
        }

        Ok(store)
    }

    fn encrypt_records(&self, records: &HashMap<(SubsystemId, String), StoredRecord>) -> Result<Vec<u8>, CredentialError> {
        let list: Vec<&StoredRecord> = records.values().collect();
        let serialized = serde_json::to_vec(&list)
            .map_err(|e| CredentialError::CryptoError(e.to_string()))?;

        let rng = SystemRandom::new();
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill(&mut nonce_bytes)
            .map_err(|_| CredentialError::CryptoError("RNG failed".into()))?;

        let unbound = UnboundKey::new(&AES_256_GCM, &self.encryption_key)
            .map_err(|_| CredentialError::CryptoError("Key init failed".into()))?;
        let mut sealing_key = SealingKey::new(unbound, OneNonce(Some(nonce_bytes)));

        let mut in_out = serialized;
        sealing_key
            .seal_in_place_append_tag(Aad::empty(), &mut in_out)
            .map_err(|_| CredentialError::CryptoError("Encryption failed".into()))?;

        let mut output = Vec::with_capacity(NONCE_LEN + in_out.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&in_out);

        Ok(output)
    }

    fn decrypt_records(&self, ciphertext: &[u8]) -> Result<HashMap<(SubsystemId, String), StoredRecord>, CredentialError> {
        if ciphertext.len() < NONCE_LEN + 16 {
            return Err(CredentialError::CryptoError("Ciphertext too short".into()));
        }

        let (nonce_bytes, payload) = ciphertext.split_at(NONCE_LEN);
        let mut nonce_arr = [0u8; NONCE_LEN];
        nonce_arr.copy_from_slice(nonce_bytes);

        let unbound = UnboundKey::new(&AES_256_GCM, &self.encryption_key)
            .map_err(|_| CredentialError::CryptoError("Key init failed".into()))?;
        let mut opening_key = OpeningKey::new(unbound, OneNonce(Some(nonce_arr)));

        let mut in_out = payload.to_vec();
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
            let tmp_path = path.with_extension("tmp");
            fs::write(&tmp_path, encrypted)
                .map_err(|e| CredentialError::IoError(format!("Write error: {e}")))?;
            fs::rename(&tmp_path, path)
                .map_err(|e| CredentialError::IoError(format!("Rename error: {e}")))?;
        }
        Ok(())
    }

    fn load_from_disk(&self, path: &Path) -> Result<(), CredentialError> {
        let encrypted = fs::read(path)
            .map_err(|e| CredentialError::IoError(format!("Read error: {e}")))?;
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
        let key_str = key.into();
        let scope_str = scope.into();
        let now = Utc::now();

        let record = StoredRecord {
            owner,
            key: key_str.clone(),
            secret: secret.into(),
            scope: scope_str.clone(),
            created_at: now,
        };

        {
            let mut guard = self.secrets.write().unwrap();
            guard.insert((owner, key_str.clone()), record);
        }

        self.persist_to_disk()?;

        let handle = CredentialHandle {
            handle_id: Uuid::new_v4(),
            owner,
            key: key_str,
            scope: scope_str,
            expires_at: now + ttl,
        };

        {
            let mut guard = self.handles.write().unwrap();
            guard.insert(handle.handle_id, handle.clone());
        }

        Ok(handle)
    }

    /// Legacy convenience wrapper
    pub fn set_secret(&self, owner: SubsystemId, key: impl Into<String>, secret: impl Into<String>) {
        let _ = self.store_credential(owner, key, secret, "*", Duration::days(365));
    }

    /// Retrieve legacy secret directly (restricted strictly to the registered owner)
    pub fn get_secret(&self, requester: SubsystemId, key: &str) -> Option<String> {
        let guard = self.secrets.read().unwrap();
        guard.get(&(requester, key.to_string())).map(|r| r.secret.clone())
    }

    /// Request an opaque scoped handle for an existing credential.
    pub fn get_handle(
        &self,
        requester: SubsystemId,
        key: &str,
        requested_scope: &str,
    ) -> Result<CredentialHandle, CredentialError> {
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

        if record.scope != "*" && !record.scope.contains(requested_scope) {
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
        if self.revoked_handles.read().unwrap().contains(&handle.handle_id) {
            return Err(CredentialError::Revoked(handle.handle_id));
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
        {
            let mut guard = self.secrets.write().unwrap();
            let record = guard
                .get_mut(&(owner, key.to_string()))
                .ok_or_else(|| CredentialError::NotFound(owner, key.to_string()))?;
            record.secret = new_secret.into();
        }
        self.persist_to_disk()?;
        info!(owner = ?owner, key, "Credential rotated in vault");
        Ok(())
    }

    /// Revoke a credential and all associated handles
    pub fn revoke_credential(&self, owner: SubsystemId, key: &str) -> Result<(), CredentialError> {
        {
            let mut guard = self.secrets.write().unwrap();
            guard.remove(&(owner, key.to_string()));
        }
        self.persist_to_disk()?;
        Ok(())
    }

    /// Revoke a specific active handle immediately
    pub fn revoke_handle(&self, handle_id: Uuid) {
        let mut guard = self.revoked_handles.write().unwrap();
        guard.insert(handle_id);
    }

    /// Centralized structured log and trace redaction: scans text and replaces any known secret values with [REDACTED]
    pub fn redact_text(&self, input: &str) -> String {
        let guard = self.secrets.read().unwrap();
        let mut redacted = input.to_string();

        for record in guard.values() {
            if record.secret.len() >= 4 && redacted.contains(&record.secret) {
                redacted = redacted.replace(&record.secret, "[REDACTED_SECRET]");
            }
        }

        redacted
    }

    /// Reject prompt injection attacks attempting to exfiltrate or query all secrets
    pub fn assert_no_credential_leakage(&self, prompt_text: &str) -> bool {
        let lower = prompt_text.to_lowercase();
        if lower.contains("print all credentials")
            || lower.contains("dump secrets")
            || lower.contains("export api keys")
            || lower.contains("reveal authorization token")
        {
            warn!("Rejected untrusted prompt attempting to dump credential store");
            return false;
        }
        true
    }
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}
