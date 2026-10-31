use chrono::{DateTime, Duration, Utc};
use ring::aead::{Aad, BoundKey, Nonce, NonceSequence, OpeningKey, SealingKey, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

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
    #[error("Access denied: untrusted subsystem {0:?} cannot request raw credential material")]
    UntrustedCaller(SubsystemId),
    #[error("Credential handle {0} has expired")]
    Expired(Uuid),
    #[error("Credential handle {0} has been revoked")]
    Revoked(Uuid),
    #[error("Requested scope '{requested}' is outside allowed scope '{allowed}'")]
    ScopeMismatch {
        requested: String,
        allowed: String,
    },
    #[error("Vault is locked; explicit unlock required")]
    VaultLocked,
    #[error("Vault cryptographic error: {0}")]
    CryptoError(String),
    #[error("Vault persistence error: {0}")]
    IoError(String),
}

/// Zeroizing wrapper for sensitive secrets to ensure memory is securely wiped when dropped.
/// Formats as `[REDACTED_SECRET]` under Debug and Display to prevent accidental leakage into logs.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[REDACTED_SECRET]")
    }
}

impl std::fmt::Display for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[REDACTED_SECRET]")
    }
}

impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(SecretString)
    }
}

/// Internal zeroizing wrapper for 256-bit vault keys
#[derive(Zeroize, ZeroizeOnDrop)]
struct VaultKey([u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultStatus {
    Unlocked,
    Locked,
}

/// Structured credential scope to isolate connector access by resource, account and operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopedResource {
    pub resource: String,
    pub account: Option<String>,
    pub operation: String,
}

impl ScopedResource {
    pub fn new(resource: impl Into<String>, account: Option<String>, operation: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
            account,
            operation: operation.into(),
        }
    }

    pub fn parse(scope_str: &str) -> Self {
        let parts: Vec<&str> = scope_str.split(':').collect();
        match parts.len() {
            1 => Self {
                resource: parts[0].to_string(),
                account: None,
                operation: "*".to_string(),
            },
            2 => Self {
                resource: parts[0].to_string(),
                account: None,
                operation: parts[1].to_string(),
            },
            _ => Self {
                resource: parts[0].to_string(),
                account: Some(parts[1].to_string()),
                operation: parts[2..].join(":"),
            },
        }
    }

    pub fn permits(&self, target_resource: &str, target_account: Option<&str>, target_op: &str) -> bool {
        if self.resource != "*" && self.resource != target_resource {
            return false;
        }
        if let (Some(own_acc), Some(req_acc)) = (&self.account, target_account) {
            if own_acc != "*" && own_acc != req_acc {
                return false;
            }
        }
        if self.operation != "*" && self.operation != target_op {
            return false;
        }
        true
    }
}

pub type CredentialScope = ScopedResource;

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
    pub secret: SecretString,
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
    encryption_key: RwLock<Option<VaultKey>>,
    is_locked: RwLock<bool>,
}

/// Identifies subsystems that must NEVER be allowed to directly query or receive credentials.
fn is_untrusted_caller(subsystem: SubsystemId) -> bool {
    matches!(
        subsystem,
        SubsystemId::Dialogue | SubsystemId::Agents | SubsystemId::Mind | SubsystemId::Canvas
    )
}

impl CredentialStore {
    pub fn new() -> Self {
        let rng = SystemRandom::new();
        let mut key = [0u8; 32];
        let _ = rng.fill(&mut key);

        Self {
            secrets: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            revoked_handles: RwLock::new(HashSet::new()),
            vault_path: None,
            encryption_key: RwLock::new(Some(VaultKey(key))),
            is_locked: RwLock::new(false),
        }
    }

    /// Open or initialize an encrypted persistent credential vault backed by AES-256-GCM.
    /// Sets strict OS file permissions (0700 dir, 0600 key and enc files).
    pub fn open_persistent(vault_dir: &Path) -> Result<Self, CredentialError> {
        Self::open_persistent_internal(vault_dir, false)
    }

    /// Open persistent vault in locked state, requiring explicit unlock before access.
    pub fn open_persistent_locked(vault_dir: &Path) -> Result<Self, CredentialError> {
        Self::open_persistent_internal(vault_dir, true)
    }

    fn open_persistent_internal(vault_dir: &Path, start_locked: bool) -> Result<Self, CredentialError> {
        let _ = fs::create_dir_all(vault_dir);
        #[cfg(unix)]
        let _ = fs::set_permissions(vault_dir, fs::Permissions::from_mode(0o700));

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

            #[cfg(unix)]
            {
                let mut opts = fs::OpenOptions::new();
                opts.write(true).create(true).truncate(true).mode(0o600);
                use std::io::Write;
                let mut f = opts
                    .open(&key_path)
                    .map_err(|e| CredentialError::IoError(format!("Failed to persist vault key: {e}")))?;
                f.write_all(&key)
                    .map_err(|e| CredentialError::IoError(format!("Failed to write vault key: {e}")))?;
            }
            #[cfg(not(unix))]
            {
                fs::write(&key_path, &key)
                    .map_err(|e| CredentialError::IoError(format!("Failed to persist vault key: {e}")))?;
            }
        }

        #[cfg(unix)]
        let _ = fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600));

        let store = Self {
            secrets: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            revoked_handles: RwLock::new(HashSet::new()),
            vault_path: Some(enc_path.clone()),
            encryption_key: RwLock::new(if start_locked { None } else { Some(VaultKey(key)) }),
            is_locked: RwLock::new(start_locked),
        };

        if !start_locked && enc_path.exists() {
            store.load_from_disk(&enc_path)?;
        }

        Ok(store)
    }

    pub fn status(&self) -> VaultStatus {
        if *self.is_locked.read().unwrap() {
            VaultStatus::Locked
        } else {
            VaultStatus::Unlocked
        }
    }

    pub fn is_locked(&self) -> bool {
        *self.is_locked.read().unwrap()
    }

    /// Lock vault: securely wipes memory containing plaintext secrets and encryption keys.
    pub fn lock(&self) {
        let mut locked = self.is_locked.write().unwrap();
        *locked = true;

        let mut secrets = self.secrets.write().unwrap();
        secrets.clear();

        let mut key_guard = self.encryption_key.write().unwrap();
        *key_guard = None;

        info!("Credential vault has been locked; memory wiped");
    }

    /// Explicitly unlock vault with the 32-byte master key.
    pub fn unlock(&self, master_key: &[u8]) -> Result<(), CredentialError> {
        if master_key.len() != 32 {
            return Err(CredentialError::CryptoError("Master key must be exactly 32 bytes".into()));
        }

        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(master_key);

        if let Some(path) = &self.vault_path {
            if path.exists() {
                let encrypted = fs::read(path)
                    .map_err(|e| CredentialError::IoError(format!("Read error: {e}")))?;
                let records = Self::decrypt_records_with_key(&key_arr, &encrypted)?;
                let mut guard = self.secrets.write().unwrap();
                *guard = records;
            }
        }

        let mut key_guard = self.encryption_key.write().unwrap();
        *key_guard = Some(VaultKey(key_arr));

        let mut locked = self.is_locked.write().unwrap();
        *locked = false;

        info!("Credential vault successfully unlocked");
        Ok(())
    }

    fn encrypt_records_with_key(
        key: &[u8; 32],
        records: &HashMap<(SubsystemId, String), StoredRecord>,
    ) -> Result<Vec<u8>, CredentialError> {
        let list: Vec<&StoredRecord> = records.values().collect();
        let serialized = serde_json::to_vec(&list)
            .map_err(|e| CredentialError::CryptoError(e.to_string()))?;

        let rng = SystemRandom::new();
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill(&mut nonce_bytes)
            .map_err(|_| CredentialError::CryptoError("RNG failed".into()))?;

        let unbound = UnboundKey::new(&AES_256_GCM, key)
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

    fn decrypt_records_with_key(
        key: &[u8; 32],
        ciphertext: &[u8],
    ) -> Result<HashMap<(SubsystemId, String), StoredRecord>, CredentialError> {
        if ciphertext.len() < NONCE_LEN + 16 {
            return Err(CredentialError::CryptoError("Ciphertext too short".into()));
        }

        let (nonce_bytes, payload) = ciphertext.split_at(NONCE_LEN);
        let mut nonce_arr = [0u8; NONCE_LEN];
        nonce_arr.copy_from_slice(nonce_bytes);

        let unbound = UnboundKey::new(&AES_256_GCM, key)
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
        if self.is_locked() {
            return Err(CredentialError::VaultLocked);
        }

        if let Some(path) = &self.vault_path {
            let key_guard = self.encryption_key.read().unwrap();
            let key = key_guard
                .as_ref()
                .ok_or(CredentialError::VaultLocked)?;

            let guard = self.secrets.read().unwrap();
            let encrypted = Self::encrypt_records_with_key(&key.0, &guard)?;
            let tmp_path = path.with_extension("tmp");

            #[cfg(unix)]
            {
                let mut opts = fs::OpenOptions::new();
                opts.write(true).create(true).truncate(true).mode(0o600);
                use std::io::Write;
                let mut f = opts
                    .open(&tmp_path)
                    .map_err(|e| CredentialError::IoError(format!("Write error: {e}")))?;
                f.write_all(&encrypted)
                    .map_err(|e| CredentialError::IoError(format!("Write error: {e}")))?;
            }
            #[cfg(not(unix))]
            {
                fs::write(&tmp_path, encrypted)
                    .map_err(|e| CredentialError::IoError(format!("Write error: {e}")))?;
            }

            fs::rename(&tmp_path, path)
                .map_err(|e| CredentialError::IoError(format!("Rename error: {e}")))?;

            #[cfg(unix)]
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn load_from_disk(&self, path: &Path) -> Result<(), CredentialError> {
        let key_guard = self.encryption_key.read().unwrap();
        let key = key_guard
            .as_ref()
            .ok_or(CredentialError::VaultLocked)?;

        let encrypted = fs::read(path)
            .map_err(|e| CredentialError::IoError(format!("Read error: {e}")))?;
        let records = Self::decrypt_records_with_key(&key.0, &encrypted)?;
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
        if self.is_locked() {
            return Err(CredentialError::VaultLocked);
        }

        let key_str = key.into();
        let scope_str = scope.into();
        let now = Utc::now();

        let record = StoredRecord {
            owner,
            key: key_str.clone(),
            secret: SecretString::new(secret),
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

    /// Retrieve legacy secret directly. Untrusted subsystems (Dialogue, Agents, Mind, Canvas)
    /// are strictly forbidden from receiving raw secrets.
    pub fn get_secret(&self, requester: SubsystemId, key: &str) -> Option<String> {
        if is_untrusted_caller(requester) {
            warn!(requester = ?requester, key, "Untrusted subsystem denied raw credential access");
            return None;
        }
        if self.is_locked() {
            return None;
        }
        let guard = self.secrets.read().unwrap();
        guard
            .get(&(requester, key.to_string()))
            .map(|r| r.secret.expose_secret().to_string())
    }

    /// Request an opaque scoped handle for an existing credential.
    /// Rejects requests from untrusted model/agent subsystems.
    pub fn get_handle(
        &self,
        requester: SubsystemId,
        key: &str,
        requested_scope: &str,
    ) -> Result<CredentialHandle, CredentialError> {
        if is_untrusted_caller(requester) {
            warn!(requester = ?requester, key, "Untrusted subsystem denied credential handle");
            return Err(CredentialError::UntrustedCaller(requester));
        }

        if self.is_locked() {
            return Err(CredentialError::VaultLocked);
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

        let parsed_allowed = ScopedResource::parse(&record.scope);
        let parsed_requested = ScopedResource::parse(requested_scope);
        if !parsed_allowed.permits(
            &parsed_requested.resource,
            parsed_requested.account.as_deref(),
            &parsed_requested.operation,
        ) {
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
        if is_untrusted_caller(caller) {
            return Err(CredentialError::UntrustedCaller(caller));
        }

        if self.is_locked() {
            return Err(CredentialError::VaultLocked);
        }

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

        Ok(action(record.secret.expose_secret()))
    }

    /// Execute closure ensuring handle's scope explicitly covers target resource, account and operation.
    pub fn execute_with_scoped_action<F, R>(
        &self,
        handle: &CredentialHandle,
        caller: SubsystemId,
        target_resource: &str,
        target_account: Option<&str>,
        target_op: &str,
        action: F,
    ) -> Result<R, CredentialError>
    where
        F: FnOnce(&str) -> R,
    {
        let parsed_scope = ScopedResource::parse(&handle.scope);
        if !parsed_scope.permits(target_resource, target_account, target_op) {
            return Err(CredentialError::ScopeMismatch {
                requested: format!(
                    "{target_resource}:{}:{}",
                    target_account.unwrap_or("*"),
                    target_op
                ),
                allowed: handle.scope.clone(),
            });
        }
        self.execute_with_secret(handle, caller, action)
    }

    /// Rotate credential value, securely invalidating previous secret
    pub fn rotate_credential(
        &self,
        owner: SubsystemId,
        key: &str,
        new_secret: impl Into<String>,
    ) -> Result<(), CredentialError> {
        if self.is_locked() {
            return Err(CredentialError::VaultLocked);
        }

        {
            let mut guard = self.secrets.write().unwrap();
            let record = guard
                .get_mut(&(owner, key.to_string()))
                .ok_or_else(|| CredentialError::NotFound(owner, key.to_string()))?;
            record.secret = SecretString::new(new_secret);
        }
        self.persist_to_disk()?;
        info!(owner = ?owner, key, "Credential rotated in vault");
        Ok(())
    }

    /// Revoke a credential and all associated handles
    pub fn revoke_credential(&self, owner: SubsystemId, key: &str) -> Result<(), CredentialError> {
        if self.is_locked() {
            return Err(CredentialError::VaultLocked);
        }

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

    /// Centralized structured log and trace redaction: scans text and replaces any known secret values
    /// or credential patterns (Bearer, tokens, keys) with [REDACTED_SECRET].
    pub fn redact_text(&self, input: &str) -> String {
        let guard = self.secrets.read().unwrap();
        let mut redacted = input.to_string();

        for record in guard.values() {
            let sec = record.secret.expose_secret();
            if sec.len() >= 4 && redacted.contains(sec) {
                redacted = redacted.replace(sec, "[REDACTED_SECRET]");
            }
        }

        redacted = redact_credential_patterns(&redacted);
        redacted
    }

    /// Redact secret values inside JSON objects or arrays
    pub fn redact_value(&self, value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => serde_json::Value::String(self.redact_text(s)),
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(|v| self.redact_value(v)).collect())
            }
            serde_json::Value::Object(map) => {
                let mut new_map = serde_json::Map::new();
                for (k, v) in map {
                    let sanitized_val = if k.to_lowercase().contains("secret")
                        || k.to_lowercase().contains("token")
                        || k.to_lowercase().contains("password")
                        || k.to_lowercase().contains("key")
                    {
                        serde_json::Value::String("[REDACTED_SECRET]".into())
                    } else {
                        self.redact_value(v)
                    };
                    new_map.insert(k.clone(), sanitized_val);
                }
                serde_json::Value::Object(new_map)
            }
            other => other.clone(),
        }
    }

    /// Legacy heuristic helper retained for backward compatibility
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

/// Generic pattern-based redaction for bearer tokens and common credential tokens
pub fn redact_credential_patterns(input: &str) -> String {
    let mut sanitized = input.to_string();

    let prefixes = [
        "ghp_",
        "github_pat_",
        "sk_live_",
        "sk_test_",
        "xoxb-",
        "xoxp-",
    ];

    for prefix in prefixes {
        while let Some(start) = sanitized.find(prefix) {
            let end_candidate = (start + 64).min(sanitized.len());
            let token_end = sanitized[start..end_candidate]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == ';')
                .map(|off| start + off)
                .unwrap_or(end_candidate);

            if token_end > start + prefix.len() {
                sanitized.replace_range(start..token_end, "[REDACTED_SECRET]");
            } else {
                break;
            }
        }
    }

    if let Some(idx) = sanitized.find("Bearer ") {
        let rest = &sanitized[idx + 7..];
        let token_len = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        if token_len > 6 {
            sanitized.replace_range(idx + 7..idx + 7 + token_len, "[REDACTED_SECRET]");
        }
    }

    sanitized
}
