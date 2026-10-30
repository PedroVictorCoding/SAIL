use std::collections::HashMap;
use std::sync::RwLock;
use tracing::warn;

use sail_common::types::SubsystemId;

pub struct CredentialStore {
    secrets: RwLock<HashMap<(SubsystemId, String), String>>,
}

impl CredentialStore {
    pub fn new() -> Self {
        Self {
            secrets: RwLock::new(HashMap::new()),
        }
    }

    /// Store a credential scoped strictly to an authorized subsystem
    pub fn set_secret(&self, owner: SubsystemId, key: impl Into<String>, secret: impl Into<String>) {
        let mut guard = self.secrets.write().unwrap();
        guard.insert((owner, key.into()), secret.into());
    }

    /// Retrieve a credential, ensuring requester matches the owner
    pub fn get_secret(&self, requester: SubsystemId, key: &str) -> Option<String> {
        let guard = self.secrets.read().unwrap();
        guard.get(&(requester, key.to_string())).cloned()
    }

    /// Reject untrusted attempts to exfiltrate or query all secrets
    pub fn assert_no_credential_leakage(&self, prompt_text: &str) -> bool {
        let lower = prompt_text.to_lowercase();
        if lower.contains("print all credentials") || lower.contains("dump secrets") || lower.contains("export api keys") {
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

