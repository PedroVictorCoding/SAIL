//! Memory visibility policy. Guardian is the only place that decides which knowledge scopes a
//! purpose may read; Arena enforces the resulting grant in SQL before ranking.
use sail_protocol::memory::{MemoryGrant, MemoryPurpose, MemorySensitivity, VERSION};

/// Scopes holding the owner's own conversational knowledge. Health and other `Sensitive`
/// records stay out because the Dialogue grant stops at `Private`.
pub const DIALOGUE_SCOPES: &[&str] = &["voice", "bus", "user", "life"];
/// Archive scopes that background maintenance consolidates and expires.
pub const MAINTENANCE_SCOPES: &[&str] = &["voice", "bus"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MemoryGrantError {
    #[error("invalid repository identity for a Forge memory grant: {0:?}")]
    InvalidRepository(String),
}

/// Issue the memory grant for `purpose`. Isolation is by scope: Forge sees only its
/// repository's project scope, so personal, life, social and health knowledge never reaches
/// it. No grant reads `Sensitive` records.
pub fn memory_grant(purpose: &MemoryPurpose) -> Result<MemoryGrant, MemoryGrantError> {
    let (scopes, max_sensitivity) = match purpose {
        MemoryPurpose::Dialogue => (owned(DIALOGUE_SCOPES), MemorySensitivity::Private),
        MemoryPurpose::Maintenance => (owned(MAINTENANCE_SCOPES), MemorySensitivity::Private),
        MemoryPurpose::Forge { repo } => (
            vec![format!("project:{}", repository_key(repo)?)],
            MemorySensitivity::Private,
        ),
    };
    Ok(MemoryGrant {
        version: VERSION,
        purpose: purpose.clone(),
        scopes,
        max_sensitivity,
    })
}

fn owned(scopes: &[&str]) -> Vec<String> {
    scopes.iter().map(|s| s.to_string()).collect()
}

/// `owner/name` with GitHub-style characters only, lowercased so scopes cannot be aliased.
fn repository_key(repo: &str) -> Result<String, MemoryGrantError> {
    let invalid = || MemoryGrantError::InvalidRepository(repo.to_string());
    let (owner, name) = repo.split_once('/').ok_or_else(invalid)?;
    let part_ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p != "."
            && p != ".."
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if !part_ok(owner) || !part_ok(name) {
        return Err(invalid());
    }
    Ok(repo.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialogue_sees_owner_scopes_but_never_sensitive_records() {
        let g = memory_grant(&MemoryPurpose::Dialogue).unwrap();
        assert_eq!(g.scopes, DIALOGUE_SCOPES);
        assert_eq!(g.max_sensitivity, MemorySensitivity::Private);
    }

    #[test]
    fn forge_sees_only_its_repository_project_scope() {
        let g = memory_grant(&MemoryPurpose::Forge {
            repo: "PedroVictorCoding/SAIL".into(),
        })
        .unwrap();
        assert_eq!(g.scopes, vec!["project:pedrovictorcoding/sail".to_string()]);
        assert_eq!(g.max_sensitivity, MemorySensitivity::Private);
        for scope in DIALOGUE_SCOPES {
            assert!(!g.scopes.iter().any(|s| s == scope));
        }
    }

    #[test]
    fn malformed_repositories_get_no_grant() {
        for repo in ["", "sail", "a/b/c", "../x", "a/..", "a/b c", "a/b*", "a/"] {
            assert!(
                memory_grant(&MemoryPurpose::Forge { repo: repo.into() }).is_err(),
                "{repo:?} should be rejected"
            );
        }
    }
}
