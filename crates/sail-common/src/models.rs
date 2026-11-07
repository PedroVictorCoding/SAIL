//! The one resident model. SAIL keeps a single large general model loaded and every
//! subsystem (voice, agents, Forge) shares it; Mind's scheduler orders their requests,
//! with live voice first. A second large model would not fit beside it in RAM, and
//! swapping models would take the main model away from voice. See `docs/SAIL_MIND.md`.
//!
//! Which model that is can be chosen from a short catalog of known models
//! ([`CATALOG`]). The owner's choice is saved in `<data root>/mind/default-model.json` and
//! applies the next time SAIL starts. Nothing is picked silently: without a saved choice
//! the primary model is used, and a saved choice whose weights are missing still names
//! that model's file, so the error says what to install.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File name of the primary model's GGUF weights.
pub const PRIMARY_MODEL_FILE: &str = "Qwen3.6-35B-A3B-Q4_K_M.gguf";

/// A model SAIL knows how to run as its resident reasoning model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ModelEntry {
    /// Stable id stored in the saved choice.
    pub id: &'static str,
    pub name: &'static str,
    /// GGUF weights file name, looked up in the model directories.
    pub file: &'static str,
    /// Optional multimodal projector (image input) next to the weights.
    pub projector: Option<&'static str>,
    /// Download size of the weights, in GB.
    pub size_gb: f32,
    /// Parameters used per token, in billions (what sets speed on CPU).
    pub active_params_b: f32,
    pub total_params_b: f32,
    pub summary: &'static str,
    /// Where the weights come from.
    pub source: &'static str,
}

/// Known models, the primary first.
pub const CATALOG: &[ModelEntry] = &[
    ModelEntry {
        id: "qwen3.6-35b-a3b",
        name: "Qwen3.6 35B-A3B",
        file: PRIMARY_MODEL_FILE,
        projector: None,
        size_gb: 20.4,
        active_params_b: 3.0,
        total_params_b: 35.0,
        summary: "The original default. Strongest reasoning and tool use here, and the slowest on this CPU.",
        source: "https://huggingface.co/Qwen",
    },
    ModelEntry {
        id: "lfm2.5-24b-a2b-uncensored",
        name: "LFM2.5 24B-A2B Uncensored",
        file: "LFM2.5-24B-A2B-Uncensored-AD-Q4_K_M.gguf",
        projector: None,
        size_gb: 15.4,
        active_params_b: 2.0,
        total_params_b: 24.0,
        summary: "Liquid AI mixture of experts with fewer active parameters, so faster replies. A community build with refusals removed; SAIL's own approvals still apply. Text only.",
        source: "https://huggingface.co/AtomicChat/LFM2.5-24B-A2B-Uncensored-GGUF",
    },
    ModelEntry {
        id: "lfm3-omni-2b",
        name: "LFM3 Omni 2B",
        file: "LFM3-Omni-2B-Q4_K_M.gguf",
        projector: Some("mmproj-LFM3-Omni-2B-Q4_K_M.gguf"),
        size_gb: 1.1,
        active_params_b: 2.2,
        total_params_b: 2.2,
        summary: "Small and fast, with image input. Noticeably weaker at reasoning and tool use than the larger models.",
        source: "https://huggingface.co/LiquidAI/LFM3-Omni-2B-GGUF",
    },
];

/// The catalog entry with this id.
pub fn entry(id: &str) -> Option<&'static ModelEntry> {
    CATALOG.iter().find(|m| m.id == id)
}

/// The primary model's entry.
pub fn primary() -> &'static ModelEntry {
    &CATALOG[0]
}

/// The owner's saved choice, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedChoice {
    pub model: String,
}

/// Where the saved choice lives: `<data root>/mind/default-model.json`.
pub fn choice_path() -> PathBuf {
    crate::storage::data_path("mind").join("default-model.json")
}

/// The saved default model, if one is saved and known. An unknown id is ignored (and
/// logged), so a stale file can never start an unexpected model.
pub fn saved_default() -> Option<&'static ModelEntry> {
    read_choice(&choice_path())
}

fn read_choice(path: &Path) -> Option<&'static ModelEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let choice: SavedChoice = match serde_json::from_str(&text) {
        Ok(choice) => choice,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "Saved default model is unreadable; using the primary model");
            return None;
        }
    };
    let found = entry(&choice.model);
    if found.is_none() {
        tracing::warn!(model = %choice.model, "Saved default model is not in the catalog; using the primary model");
    }
    found
}

/// Save `id` as the default model for the next start. Written atomically.
pub fn save_default(id: &str) -> std::io::Result<&'static ModelEntry> {
    let model = entry(id).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unknown model `{id}`"),
        )
    })?;
    write_choice(
        &crate::storage::data_dir("mind").join("default-model.json"),
        model,
    )?;
    Ok(model)
}

fn write_choice(path: &Path, model: &ModelEntry) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(&SavedChoice {
        model: model.id.to_string(),
    })?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text + "\n")?;
    std::fs::rename(&tmp, path)
}

/// The model SAIL starts with: the saved default, else the primary.
pub fn default_model() -> &'static ModelEntry {
    saved_default().unwrap_or_else(primary)
}

/// Directories searched for weights, in order: `models/`, `$SAIL_RUNTIME/models`
/// (default `/srv/sail/models`) and `<data root>/models`.
pub fn model_dirs() -> Vec<PathBuf> {
    let runtime = std::env::var_os("SAIL_RUNTIME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/srv/sail"));
    vec![
        PathBuf::from("models"),
        runtime.join("models"),
        crate::storage::data_path("models"),
    ]
}

/// Where `file` is installed, else where it should go (the runtime directory), so an
/// error names the file to install.
pub fn locate(file: &str) -> PathBuf {
    let dirs = model_dirs();
    let refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    resolve_file(&refs, file)
}

/// Whether the model's weights (and projector, if any) are installed.
pub fn installed(model: &ModelEntry) -> bool {
    locate(model.file).is_file() && model.projector.is_none_or(|p| locate(p).is_file())
}

/// `SAIL_MODEL_PATH` if set, else the default model (saved choice, else primary) in the
/// model directories. Never another model: when the chosen weights are missing this
/// returns the runtime path, so the error names the file to install instead of silently
/// starting a different model.
pub fn primary_model_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SAIL_MODEL_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    locate(default_model().file)
}

/// The catalog entry for a weights path, if it is a known file.
pub fn running_entry(path: &Path) -> Option<&'static ModelEntry> {
    let name = path.file_name()?.to_str()?;
    CATALOG.iter().find(|m| m.file == name)
}

fn resolve_file(dirs: &[&Path], file: &str) -> PathBuf {
    let candidates: Vec<PathBuf> = dirs.iter().map(|d| d.join(file)).collect();
    candidates
        .iter()
        .find(|p| p.is_file())
        .or(candidates.get(1).or(candidates.last()))
        .cloned()
        .unwrap_or_else(|| PathBuf::from(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("sail-models-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn picks_the_first_directory_holding_the_file() {
        let base = temp("dirs");
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join(PRIMARY_MODEL_FILE), b"gguf").unwrap();
        assert_eq!(
            resolve_file(&[&a, &b], PRIMARY_MODEL_FILE),
            b.join(PRIMARY_MODEL_FILE)
        );
        std::fs::write(a.join(PRIMARY_MODEL_FILE), b"gguf").unwrap();
        assert_eq!(
            resolve_file(&[&a, &b], PRIMARY_MODEL_FILE),
            a.join(PRIMARY_MODEL_FILE)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_file_resolves_to_the_runtime_directory() {
        let base = temp("missing");
        let (repo, runtime, data) = (base.join("repo"), base.join("runtime"), base.join("data"));
        // Another model being present never stands in for the requested one.
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("gemma4-v2-Q4_K_M.gguf"), b"gguf").unwrap();
        assert_eq!(
            resolve_file(&[&repo, &runtime, &data], PRIMARY_MODEL_FILE),
            runtime.join(PRIMARY_MODEL_FILE)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn weights_on_the_data_disk_are_found() {
        let base = temp("data");
        let (repo, runtime, data) = (base.join("repo"), base.join("runtime"), base.join("data"));
        std::fs::create_dir_all(&data).unwrap();
        let file = CATALOG[1].file;
        std::fs::write(data.join(file), b"gguf").unwrap();
        assert_eq!(
            resolve_file(&[&repo, &runtime, &data], file),
            data.join(file)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn saved_choice_round_trips_and_unknown_ids_are_ignored() {
        let base = temp("choice");
        let path = base.join("default-model.json");
        assert_eq!(read_choice(&path), None, "no file: no saved choice");
        write_choice(&path, entry("lfm3-omni-2b").unwrap()).unwrap();
        assert_eq!(read_choice(&path).map(|m| m.id), Some("lfm3-omni-2b"));
        std::fs::write(&path, r#"{"model":"something-else"}"#).unwrap();
        assert_eq!(read_choice(&path), None, "an unknown model never starts");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_choice(&path), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn catalog_ids_and_files_are_unique_and_primary_is_first() {
        let ids: std::collections::BTreeSet<_> = CATALOG.iter().map(|m| m.id).collect();
        let files: std::collections::BTreeSet<_> = CATALOG.iter().map(|m| m.file).collect();
        assert_eq!(ids.len(), CATALOG.len());
        assert_eq!(files.len(), CATALOG.len());
        assert_eq!(primary().file, PRIMARY_MODEL_FILE);
        assert_eq!(
            running_entry(Path::new("/x/LFM3-Omni-2B-Q4_K_M.gguf")).map(|m| m.id),
            Some("lfm3-omni-2b")
        );
    }
}
