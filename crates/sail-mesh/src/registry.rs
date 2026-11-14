//! The host's list of paired workers. The file holds worker secrets: it is written 0600 and
//! never returned by status APIs.
use crate::{MeshError, PairingCode, Result, write_private};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Model names a worker may answer for when the owner does not say: any request that would
/// otherwise run on the host's default model.
pub const SERVES_ANY: &str = "*";

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerRecord {
    pub name: String,
    /// `host:port`.
    pub address: String,
    pub fingerprint: String,
    pub secret: String,
    /// Host model names (or aliases) this worker may run requests for; `*` means any.
    #[serde(default = "serves_any")]
    pub serves: Vec<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// RFC 3339 pairing time.
    #[serde(default)]
    pub paired_at: String,
}
fn serves_any() -> Vec<String> {
    vec![SERVES_ANY.into()]
}
fn enabled() -> bool {
    true
}

impl std::fmt::Debug for WorkerRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerRecord")
            .field("name", &self.name)
            .field("address", &self.address)
            .field("fingerprint", &self.fingerprint)
            .field("secret", &"…")
            .field("serves", &self.serves)
            .field("enabled", &self.enabled)
            .field("paired_at", &self.paired_at)
            .finish()
    }
}

impl WorkerRecord {
    pub fn from_code(code: &PairingCode, serves: Vec<String>, paired_at: String) -> Self {
        Self {
            name: code.name.clone(),
            address: code.address.clone(),
            fingerprint: code.fingerprint.clone(),
            secret: code.secret.clone(),
            serves: if serves.is_empty() {
                serves_any()
            } else {
                serves
            },
            enabled: true,
            paired_at,
        }
    }

    /// Whether this worker may run a request the host resolved to `model`, which is known
    /// on the host under `aliases` too.
    pub fn serves_model(&self, model: &str, aliases: &[String]) -> bool {
        self.serves
            .iter()
            .any(|s| s == SERVES_ANY || s == model || aliases.iter().any(|a| a == s))
    }

    pub fn base_url(&self) -> String {
        format!("https://{}", self.address)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub workers: Vec<WorkerRecord>,
}

impl Registry {
    /// Default location on the SAIL host: `<data root>/mesh/workers.json` is chosen by the
    /// caller (sail-mind), which knows the data root; this crate stays host-agnostic.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join("workers.json")
    }

    /// A missing file is an empty registry; a corrupt one is an error (never silently
    /// dropping the owner's pairings).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(data) => serde_json::from_slice(&data)
                .map_err(|e| MeshError::new(format!("{} is corrupt: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(MeshError::new(format!(
                "Cannot read {}: {e}",
                path.display()
            ))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let data = serde_json::to_vec_pretty(self).expect("serializable");
        write_private(path, &data)
            .map_err(|e| MeshError::new(format!("Cannot save {}: {e}", path.display())))
    }

    /// Add or replace a worker. A worker with the same name or certificate is replaced (it
    /// was re-initialised or re-paired); returns whether one was replaced.
    pub fn upsert(&mut self, record: WorkerRecord) -> bool {
        let before = self.workers.len();
        self.workers
            .retain(|w| w.name != record.name && w.fingerprint != record.fingerprint);
        let replaced = self.workers.len() != before;
        self.workers.push(record);
        replaced
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.workers.len();
        self.workers.retain(|w| w.name != name);
        self.workers.len() != before
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut WorkerRecord> {
        self.workers.iter_mut().find(|w| w.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(name: &str, fp: char) -> PairingCode {
        PairingCode {
            address: "10.0.0.2:7710".into(),
            name: name.into(),
            fingerprint: fp.to_string().repeat(64),
            secret: crate::identity::new_secret(),
        }
    }

    #[test]
    fn upsert_replaces_by_name_or_certificate() {
        let mut registry = Registry::default();
        assert!(!registry.upsert(WorkerRecord::from_code(&code("a", 'a'), vec![], "t".into())));
        assert!(registry.upsert(WorkerRecord::from_code(&code("a", 'b'), vec![], "t".into())));
        assert!(registry.upsert(WorkerRecord::from_code(&code("c", 'b'), vec![], "t".into())));
        assert_eq!(registry.workers.len(), 1);
        assert_eq!(registry.workers[0].name, "c");
        assert!(registry.remove("c"));
        assert!(!registry.remove("c"));
    }

    #[test]
    fn serves_matches_names_aliases_and_any() {
        let mut record = WorkerRecord::from_code(&code("a", 'a'), vec![], "t".into());
        assert!(record.serves_model("Qwen", &[]));
        record.serves = vec!["qwen".into()];
        assert!(record.serves_model("Qwen3.6", &["qwen".into()]));
        assert!(!record.serves_model("gemma", &["vision".into()]));
    }

    #[test]
    fn save_load_round_trip_and_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = Registry::path_in(dir.path());
        assert!(Registry::load(&path).unwrap().workers.is_empty());
        let mut registry = Registry::default();
        registry.upsert(WorkerRecord::from_code(&code("a", 'a'), vec![], "t".into()));
        registry.save(&path).unwrap();
        assert_eq!(Registry::load(&path).unwrap().workers, registry.workers);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(Registry::load(&path).is_err());
    }
}
