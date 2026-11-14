//! SAIL Market: browse and install Hermes Agent skills and plugins without copying
//! folders by hand (round 20).
//!
//! # What the market is
//!
//! Two public catalogs that Hermes Agent publishes and keeps current:
//!
//! | Catalog | URL | Size | Entries |
//! | --- | --- | --- | --- |
//! | Skills index | [`SKILLS_INDEX_URL`] | ~45 MB JSON | ~104k skills from `official` (Hermes' optional skills), `github` (curated taps), `skills.sh`, `clawhub`, `lobehub`, `browse-sh` |
//! | Plugin catalog | [`PLUGIN_CATALOG_URL`] | ~4.5 MB JSON | ~2.3k plugins, each pinned to a 40-hex commit SHA, human-reviewed admission |
//!
//! The listing never needs authentication. Installing reads files from GitHub
//! (`api.github.com` once per install for the tree, then `raw.githubusercontent.com`,
//! which has no API rate limit) or from ClawHub (`clawhub.ai/api/v1`, a zip whose files
//! are checked against the SHA-256 list the version metadata publishes).
//!
//! # Flow (every step is an owner action from the web UI; the model cannot install)
//!
//! 1. **Refresh** ([`Market::refresh`]) downloads both catalogs, parses them into the
//!    compact [`SkillEntry`] / [`PluginEntry`] forms ([`catalog`]) and caches the compact
//!    form at `$SAIL_DATA/market/catalog.json`. A failed refresh keeps the stale cache and
//!    reports the error; it never empties the market.
//! 2. **Search** ([`catalog::search`]) runs locally over the cache. No query leaves the
//!    host.
//! 3. **Stage** ([`Market::stage`]) downloads one item's files ([`fetch`]) into
//!    `$SAIL_DATA/market/quarantine/<stage_id>/`, scans them ([`guard`]) and returns a
//!    [`StagedReport`]: file list, content hash, findings, and the policy
//!    [`Decision`]. Nothing is installed and nothing runs.
//! 4. **Install** ([`Market::install`]) re-hashes the quarantined files, checks the
//!    decision (a `NeedsAck` item needs `acknowledge_caution: true`, a `Block` item can
//!    never be installed) and moves the folder into place ([`install`]):
//!    * a skill → `$SAIL_DATA/skills/<name>/` (the first skill root, so it wins over the
//!      owner's Hermes home);
//!    * a plugin → `$SAIL_DATA/plugins/hermes/<id>/`, where it is **quarantined** by the
//!      existing hash-bound approval ([`crate::hermes`]): the owner still approves its
//!      exact files, network and secrets before any of its code runs. The market's content
//!      hash for a plugin is [`crate::hermes::digest`], so the hash the owner saw at
//!      staging is the hash they approve.
//! 5. **Lock** (`$SAIL_DATA/market/lock.json`, [`Lock`]) records provenance per install.
//!    Uninstall removes only folders the lock says the market installed, and refuses a
//!    folder whose files changed since install unless forced.
//!
//! # Trust
//!
//! [`Trust`] comes from the catalog, never from an identifier a publisher chose: skills
//! use the index's `trust_level` (`builtin` / `trusted` / `community`), plugins map their
//! `tier` (`official` → `Builtin`, `verified` → `Trusted`, anything else → `Community`).
//! Self-proposed skills ([`crate::skills`] staging) are `AgentCreated`.
//!
//! # What the market does not do
//!
//! * It never runs downloaded code. Plugins run only after the existing approval, in the
//!   bubblewrap sandbox. Skill `scripts/` are not run by SAIL.
//! * It never sends the owner's data anywhere: requests carry only the item's public
//!   identifier and a generic `User-Agent` ([`USER_AGENT`]).
//! * GitLab-hosted plugins (3 entries), `lobehub` and `browse-sh` skills are listed but
//!   not installable in round 20 ([`Route::Unsupported`]).
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod catalog;
pub mod fetch;
pub mod guard;
pub mod install;
mod service;

pub use service::{CatalogStatus, Market, MarketPaths, global};

pub const SKILLS_INDEX_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/api/skills-index.json";
pub const PLUGIN_CATALOG_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/api/plugin-catalog.json";
pub const USER_AGENT: &str = "SAIL-market/1";
/// The Hermes repository that hosts `official` skills (`optional-skills/...`).
pub const HERMES_REPO: &str = "NousResearch/hermes-agent";
/// The catalogs are rebuilt upstream a few times a day.
pub const CATALOG_TTL_SECS: i64 = 6 * 3600;
/// Compact description length kept per entry (characters, not bytes).
pub const MAX_DESCRIPTION_CHARS: usize = 400;
/// Bundle limits: the same caps as an approvable plugin ([`crate::hermes::digest`]).
pub const MAX_BUNDLE_FILES: usize = 2000;
pub const MAX_BUNDLE_BYTES: u64 = 32 * 1024 * 1024;
/// A single file larger than this is a `oversized_file` finding (still installable).
pub const LARGE_FILE_BYTES: u64 = 1024 * 1024;

/// Skill or plugin. Serialized `"skill"` / `"plugin"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Skill,
    Plugin,
}

/// Serialized `"builtin"`, `"trusted"`, `"community"`, `"agent_created"`. Ordered from
/// most to least trusted (`Builtin < Trusted < ...`), which is the search tie-break order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    Builtin,
    Trusted,
    Community,
    AgentCreated,
}

/// One skill from the Hermes skills index, compacted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillEntry {
    /// Display name, cleaned (see [`catalog::clean_name`]). Not necessarily a valid Agent
    /// Skills name: the installed folder name comes from the downloaded `SKILL.md`.
    pub name: String,
    pub description: String,
    /// Index `source`: `official`, `github`, `skills.sh`, `clawhub`, `lobehub`,
    /// `browse-sh`, or anything newer (kept verbatim, routed as unsupported).
    pub source: String,
    /// Index `identifier`, unique per entry; the market id of the skill.
    pub identifier: String,
    pub trust: Trust,
    /// `owner/repo` or empty.
    #[serde(default)]
    pub repo: String,
    /// Path inside `repo`, or empty.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// skills.sh only: `owner/repo/path/to/skill`, the real GitHub location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_github_id: Option<String>,
    /// clawhub only: the publisher handle when the index has one (`extra.owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// One plugin from the Hermes plugin catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Catalog `name`; the market id and, if [`crate::hermes::valid_id`], the folder name.
    pub name: String,
    #[serde(default)]
    pub title: String,
    pub description: String,
    /// Full repository URL, e.g. `https://github.com/owner/repo`.
    pub repo: String,
    /// The reviewed commit, 40 lowercase hex characters.
    pub sha: String,
    /// Folder inside the repository holding `plugin.yaml`, or empty for the root.
    #[serde(default)]
    pub subdir: String,
    #[serde(default)]
    pub version: String,
    /// Catalog `tier`, verbatim (`official`, `verified`, `community`, or empty).
    #[serde(default)]
    pub tier: String,
    pub trust: Trust,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub maintainer: String,
    #[serde(default)]
    pub docs_url: String,
    #[serde(default)]
    pub requires_env: Vec<String>,
    #[serde(default)]
    pub provides_tools: Vec<String>,
    #[serde(default)]
    pub provides_hooks: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
}

/// A plugin the catalog withdrew; shown on installed copies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Removed {
    pub name: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub date: String,
}

/// The cached, compacted market. Serialized as-is to `$SAIL_DATA/market/catalog.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub skills: Vec<SkillEntry>,
    pub plugins: Vec<PluginEntry>,
    #[serde(default)]
    pub removed: Vec<Removed>,
    /// Upstream `generated_at` of each catalog, verbatim.
    #[serde(default)]
    pub skills_generated_at: String,
    #[serde(default)]
    pub plugins_generated_at: String,
    /// When SAIL last fetched each catalog successfully (Unix seconds; 0 = never).
    #[serde(default)]
    pub skills_fetched_at: i64,
    #[serde(default)]
    pub plugins_fetched_at: i64,
}

/// Where an item's files come from. Serialized with `"type"` as the tag, e.g.
/// `{"type":"github","repo":"owner/repo","path":"skills/pdf","reference":null}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Route {
    /// Files under `path` (empty = repository root) of `repo` (`owner/repo`) at
    /// `reference` (a 40-hex commit for plugins; `None` = the default branch's head).
    Github {
        repo: String,
        path: String,
        reference: Option<String>,
    },
    /// `clawhub.ai` skill `slug`, optionally disambiguated by publisher `owner`.
    Clawhub { slug: String, owner: Option<String> },
    /// Listed but not installable; `reason` is shown to the owner.
    Unsupported { reason: String },
}

/// What the market search returns per item. `id` is the [`SkillEntry::identifier`] or
/// the [`PluginEntry::name`]; `(kind, id)` is unique.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub kind: Kind,
    pub id: String,
    pub name: String,
    pub description: String,
    /// Skill index `source` (`official`, `github`, ...) or `"catalog"` for plugins.
    pub source: String,
    pub trust: Trust,
    /// Plugin category, or empty for skills.
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub route: Route,
    /// The lock key (`skill:<name>` / `plugin:<id>`) when this item is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
}

/// Search request. `text` matches name, tags and description, case-insensitively.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Query {
    pub text: String,
    pub kind: Option<Kind>,
    /// Exact skill source (`official`, `clawhub`, ...) or `"catalog"`.
    pub source: Option<String>,
    /// Only items at least this trusted (`Builtin` = only builtin).
    pub min_trust: Option<Trust>,
    pub offset: usize,
    /// 0 means the default of 50; capped at 200.
    pub limit: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub total: usize,
    pub items: Vec<Listing>,
}

/// Downloaded files, keyed by `/`-separated relative path. Paths are validated with
/// [`safe_relative_path`] before they are inserted; a bundle never holds a symlink.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bundle {
    pub files: BTreeMap<String, Vec<u8>>,
    /// Upstream revision: the GitHub tree/commit SHA or the ClawHub version.
    pub revision: String,
}

impl Bundle {
    pub fn total_bytes(&self) -> u64 {
        self.files.values().map(|f| f.len() as u64).sum()
    }
}

/// True for a relative path SAIL may write: non-empty `/`-separated components, none of
/// them empty, `.`, `..`, or containing `\` or NUL, at most 512 bytes, no leading `/`.
pub fn safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', '\0']))
}

/// Severity of one scan finding. Serialized lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Serialized `"safe"`, `"caution"`, `"dangerous"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Safe,
    Caution,
    Dangerous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable rule id, e.g. `curl_pipe_shell`.
    pub rule: String,
    pub severity: Severity,
    /// `exfiltration`, `injection`, `destructive`, `execution`, `network`, `persistence`,
    /// `obfuscation`, `credentials` or `structure`.
    pub category: String,
    pub file: String,
    /// 1-based line, 0 for whole-file findings.
    pub line: usize,
    /// The matched text, at most 160 characters, control characters replaced by `�`.
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub verdict: Verdict,
    /// Sorted by severity (highest first), then file, then line, then rule.
    pub findings: Vec<Finding>,
    pub files_scanned: usize,
    pub scanner: String,
}

/// Serialized `"allow"`, `"needs_ack"`, `"block"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    /// The owner must tick "install anyway" (`acknowledge_caution: true`).
    NeedsAck,
    Block,
}

/// What staging returns to the owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StagedReport {
    /// 32 lowercase hex characters; names the quarantine folder.
    pub stage_id: String,
    pub listing: Listing,
    /// Folder name the item will be installed as (skill `name` from `SKILL.md`, or the
    /// plugin id).
    pub install_name: String,
    pub files: Vec<StagedFile>,
    /// Skills: SHA-256 over the bundle ([`install::content_hash`]). Plugins:
    /// [`crate::hermes::digest`] of the quarantine folder.
    pub content_hash: String,
    pub revision: String,
    pub report: Report,
    pub decision: Decision,
    /// Why the decision is not `Allow`, or why the name is refused; empty otherwise.
    #[serde(default)]
    pub reason: String,
    /// `SKILL.md` or `README.md` (plugins), at most 64 KiB, for the owner to read.
    #[serde(default)]
    pub preview: String,
    /// Unix seconds.
    pub staged_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedFile {
    pub path: String,
    pub bytes: u64,
}

/// `$SAIL_DATA/market/lock.json`. `installed` is keyed by `skill:<name>` / `plugin:<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lock {
    pub version: u32,
    #[serde(default)]
    pub installed: BTreeMap<String, LockEntry>,
}

impl Default for Lock {
    fn default() -> Self {
        Self {
            version: 1,
            installed: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockEntry {
    pub kind: Kind,
    /// Installed folder name.
    pub name: String,
    /// Market id ([`Listing::id`]).
    pub id: String,
    pub source: String,
    pub trust: Trust,
    pub route: Route,
    pub revision: String,
    pub content_hash: String,
    pub verdict: Verdict,
    pub scanner: String,
    pub findings: usize,
    /// True when the owner installed past a `NeedsAck` decision.
    #[serde(default)]
    pub acknowledged_caution: bool,
    pub install_path: std::path::PathBuf,
    pub files: Vec<String>,
    /// Unix seconds.
    pub installed_at: i64,
    pub updated_at: i64,
}

/// An installed item with its on-disk state, for the Installed list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    pub key: String,
    pub entry: LockEntry,
    /// `"ok"` (files match the lock), `"modified"`, or `"missing"`.
    pub state: String,
    /// The catalog's withdrawal notice for a removed plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<Removed>,
}

/// Unix seconds now. Only the service reads the clock; pure modules take `now`.
pub(crate) fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        for ok in ["SKILL.md", "references/api.md", "a/b/c.txt", ".hidden/x"] {
            assert!(safe_relative_path(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a/./b",
            "a\\b",
            "a/",
            "x\0",
        ] {
            assert!(!safe_relative_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn wire_names() {
        assert_eq!(
            serde_json::to_value(Trust::AgentCreated).unwrap(),
            "agent_created"
        );
        assert_eq!(
            serde_json::to_value(Decision::NeedsAck).unwrap(),
            "needs_ack"
        );
        assert_eq!(
            serde_json::to_value(Route::Clawhub {
                slug: "x".into(),
                owner: None
            })
            .unwrap(),
            serde_json::json!({"type":"clawhub","slug":"x","owner":null})
        );
        assert!(Trust::Builtin < Trust::Community);
    }
}
