//! The one resident model. SAIL keeps a single large general model loaded and every
//! subsystem (voice, agents, Forge) shares it; Mind's scheduler orders their requests,
//! with live voice first. A second large model would not fit beside it in RAM, and
//! swapping models would take the main model away from voice. See `docs/SAIL_MIND.md`.

use std::path::{Path, PathBuf};

/// File name of the primary model's GGUF weights.
pub const PRIMARY_MODEL_FILE: &str = "Qwen3.6-35B-A3B-Q4_K_M.gguf";

/// `SAIL_MODEL_PATH` if set, else the primary model in `models/` or in
/// `$SAIL_RUNTIME/models` (default `/srv/sail/models`). Never another model: when the
/// primary weights are missing this returns the runtime path, so the error names the file
/// to install instead of silently starting a different model.
pub fn primary_model_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SAIL_MODEL_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let runtime = std::env::var_os("SAIL_RUNTIME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/srv/sail"));
    resolve_primary(&[Path::new("models"), &runtime.join("models")])
}

fn resolve_primary(dirs: &[&Path]) -> PathBuf {
    let candidates: Vec<PathBuf> = dirs.iter().map(|d| d.join(PRIMARY_MODEL_FILE)).collect();
    candidates
        .iter()
        .find(|p| p.is_file())
        .or(candidates.last())
        .cloned()
        .unwrap_or_else(|| PathBuf::from(PRIMARY_MODEL_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_directory_holding_the_primary_model() {
        let base = std::env::temp_dir().join(format!("sail-models-{}", uuid::Uuid::new_v4()));
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join(PRIMARY_MODEL_FILE), b"gguf").unwrap();
        assert_eq!(resolve_primary(&[&a, &b]), b.join(PRIMARY_MODEL_FILE));
        std::fs::write(a.join(PRIMARY_MODEL_FILE), b"gguf").unwrap();
        assert_eq!(resolve_primary(&[&a, &b]), a.join(PRIMARY_MODEL_FILE));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn other_models_are_never_chosen() {
        let base = std::env::temp_dir().join(format!("sail-models-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        for other in [
            "Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf",
            "gemma4-v2-Q4_K_M.gguf",
        ] {
            std::fs::write(base.join(other), b"gguf").unwrap();
        }
        // Missing primary: the runtime (last) path, so the error names what to install.
        assert_eq!(resolve_primary(&[&base]), base.join(PRIMARY_MODEL_FILE));
        let _ = std::fs::remove_dir_all(&base);
    }
}
