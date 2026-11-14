//! Catalog parsing, routing and local search (round 20, **Kilo's slice**). Pure: no I/O,
//! no clock, no global state. **This module doc is the spec.**
//!
//! # Skills index ([`parse_skills_index`])
//!
//! Input: the bytes of [`super::SKILLS_INDEX_URL`]. Shape (verified 2026-11-14):
//!
//! ```json
//! {"version": 1, "generated_at": "2026-11-14T06:51:42.341661+00:00", "skill_count": 104089,
//!  "skills": [{"name": "1password", "description": "...", "source": "official",
//!              "identifier": "official/security/1password", "trust_level": "builtin",
//!              "repo": "NousResearch/hermes-agent", "path": "optional-skills/security/1password",
//!              "tags": ["security"], "extra": {}, "resolved_github_id": "o/r/p"}]}
//! ```
//!
//! * The top level must be an object with a `skills` array, else `Err`.
//! * Per entry: `name`, `source`, `identifier` are required non-empty strings; an entry
//!   without them is **skipped** (counted in [`ParseStats::skipped`]), never an error.
//!   `description`, `repo`, `path`, `tags` default to empty. `tags` keeps only strings,
//!   each through [`clean_name`], empty ones dropped, at most 12.
//! * `trust_level`: `builtin` → `Builtin`, `trusted` → `Trusted`, anything else
//!   (including missing) → `Community`. Never `AgentCreated`.
//! * `name` goes through [`clean_name`]. `description` is whitespace-collapsed and cut to
//!   [`super::MAX_DESCRIPTION_CHARS`] characters (on a char boundary; append `…` when cut).
//! * `resolved_github_id` → [`SkillEntry::resolved_github_id`] when a non-empty string.
//!   `extra.owner` (string) → [`SkillEntry::owner`].
//! * Duplicate `identifier`: the first wins, later ones are skipped.
//! * Output order is input order. Returns `(generated_at or "", entries, stats)`.
//! * Must parse the real 45 MB file in under 3 s in release builds and must not keep the
//!   original `serde_json::Value` tree alive afterwards (deserialize into small structs).
//!
//! # Plugin catalog ([`parse_plugin_catalog`])
//!
//! Input: the bytes of [`super::PLUGIN_CATALOG_URL`]:
//!
//! ```json
//! {"generated_at": "...", "removed": [{"name": "or-picker", "reason": "...", "date": "2026-09-28"}],
//!  "entries": [{"name": "aegis-gate", "title": "aegis-gate", "repo": "https://github.com/R/aegis-hermes",
//!               "sha": "54b2...", "subdir": "", "version": "0.1.1", "description": "...",
//!               "maintainer": "R", "tier": "community", "category": "tools", "docs_url": "...",
//!               "platforms": [], "capabilities": {"provides_tools": [], "provides_hooks": ["pre_tool_call"],
//!               "provides_middleware": [], "requires_env": []}}]}
//! ```
//!
//! * `entries` must be an array, else `Err`. `removed` defaults to empty.
//! * Required per entry: `name`, `repo`, `sha`; otherwise skipped. `sha` is lower-cased;
//!   an entry whose `sha` is not exactly 40 hex characters is skipped (the catalog's own
//!   rule: no branches, no short SHAs).
//! * `tier`: `official` → `Builtin`, `verified` → `Trusted`, else `Community`. Keep the raw
//!   string in [`PluginEntry::tier`].
//! * `description` as for skills. `requires_env` items may be strings or objects with a
//!   `name` field (Hermes allows both); keep the names.
//! * Duplicate `name`: first wins.
//!
//! # Names ([`clean_name`])
//!
//! Some index names are raw YAML (`"\"Dictionary\""`, `"\"AI 产品…\" metadata:"`).
//! `clean_name`: trim; if the text starts with a quote (`"` or `'`), keep only what is
//! inside the first matching pair (or everything after the opening quote if unmatched);
//! drop control characters; collapse whitespace; cut to 80 characters (append `…`).
//!
//! # Routes ([`skill_route`], [`plugin_route`])
//!
//! | Entry | Route |
//! | --- | --- |
//! | skill `official` | `Github { repo: HERMES_REPO, path: entry.path, reference: None }` |
//! | skill `github` | `Github { repo: entry.repo, path: entry.path, reference: None }` |
//! | skill `skills.sh` with `resolved_github_id` `o/r/rest` | `Github { repo: "o/r", path: "rest", None }` |
//! | skill `skills.sh` without it | `Github { repo: entry.repo, path: entry.path, None }` |
//! | skill `clawhub` | `Clawhub { slug: identifier, owner: entry.owner }`; an identifier of the form `@owner/slug` splits into both |
//! | any other skill source | `Unsupported { reason: "<source> skills cannot be installed yet" }` |
//! | plugin on `https://github.com/<o>/<r>` (optional trailing `.git` or `/`) | `Github { repo: "o/r", path: subdir, reference: Some(sha) }` |
//! | plugin on any other host | `Unsupported { reason: "only GitHub-hosted plugins can be installed" }` |
//!
//! Validation, any failure → `Unsupported` with a reason naming the field: `repo` matches
//! `owner/repo` with each part `[A-Za-z0-9_.-]{1,100}` and not `.`/`..`; `path` is empty
//! or [`super::safe_relative_path`] (strip one trailing `/` first); a ClawHub slug is
//! `[a-z0-9][a-z0-9._-]{0,127}` (case-insensitive check, keep the original).
//!
//! # Search ([`search`])
//!
//! Filters, all must hold: `query.kind` (if set), `query.source` (skills: exact
//! `entry.source`; plugins match only `"catalog"`), `query.min_trust` (`entry.trust <=
//! min_trust` in [`Trust`]'s order).
//!
//! Text: lowercase the query and split on whitespace into terms; an empty query matches
//! everything. Every term must occur (substring, case-insensitive) in the name, a tag or
//! the description. Score per item = sum over terms of the best of:
//! name equals term 100 · name starts with term 60 · name contains term 40 · a tag
//! equals term 30 · description contains term 10.
//!
//! Order: score descending, then trust (Builtin first), then name (case-insensitive),
//! then id. Skills and plugins are ranked together. `offset`/`limit` apply after
//! sorting (`limit` 0 → 50, capped at 200). `total` counts all matches.
//!
//! `installed` is filled from `lock`: a listing is installed when some lock entry has the
//! same `kind` and `id`; the value is that entry's key.
//!
//! Performance: an empty-text query over 104k skills returns in under 150 ms in release.
use super::{Catalog, Kind, Listing, Lock, Page, PluginEntry, Query, Removed, Route, SkillEntry};

/// Counts reported after a parse, so the UI can say how many entries were unusable.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParseStats {
    pub kept: usize,
    pub skipped: usize,
}

pub fn parse_skills_index(bytes: &[u8]) -> Result<(String, Vec<SkillEntry>, ParseStats), String> {
    let _ = bytes;
    unimplemented!("Kilo, round 20")
}

#[allow(clippy::type_complexity)]
pub fn parse_plugin_catalog(
    bytes: &[u8],
) -> Result<(String, Vec<PluginEntry>, Vec<Removed>, ParseStats), String> {
    let _ = bytes;
    unimplemented!("Kilo, round 20")
}

pub fn clean_name(raw: &str) -> String {
    let _ = raw;
    unimplemented!("Kilo, round 20")
}

pub fn skill_route(entry: &SkillEntry) -> Route {
    let _ = entry;
    unimplemented!("Kilo, round 20")
}

pub fn plugin_route(entry: &PluginEntry) -> Route {
    let _ = entry;
    unimplemented!("Kilo, round 20")
}

/// The listing for one skill (route via [`skill_route`]; `installed` = None).
pub fn skill_listing(entry: &SkillEntry) -> Listing {
    Listing {
        kind: Kind::Skill,
        id: entry.identifier.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        source: entry.source.clone(),
        trust: entry.trust,
        category: String::new(),
        tags: entry.tags.clone(),
        route: skill_route(entry),
        installed: None,
    }
}

/// The listing for one plugin (route via [`plugin_route`]; `installed` = None).
pub fn plugin_listing(entry: &PluginEntry) -> Listing {
    Listing {
        kind: Kind::Plugin,
        id: entry.name.clone(),
        name: if entry.title.is_empty() {
            entry.name.clone()
        } else {
            entry.title.clone()
        },
        description: entry.description.clone(),
        source: "catalog".into(),
        trust: entry.trust,
        category: entry.category.clone(),
        tags: Vec::new(),
        route: plugin_route(entry),
        installed: None,
    }
}

/// Look up one item by kind and id.
pub fn find(catalog: &Catalog, kind: Kind, id: &str) -> Option<Listing> {
    match kind {
        Kind::Skill => catalog
            .skills
            .iter()
            .find(|s| s.identifier == id)
            .map(skill_listing),
        Kind::Plugin => catalog
            .plugins
            .iter()
            .find(|p| p.name == id)
            .map(plugin_listing),
    }
}

pub fn search(catalog: &Catalog, query: &Query, lock: &Lock) -> Page {
    let _ = (catalog, query, lock);
    unimplemented!("Kilo, round 20")
}
