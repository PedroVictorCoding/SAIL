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
use super::{
    Catalog, HERMES_REPO, Kind, Listing, Lock, MAX_DESCRIPTION_CHARS, Page, PluginEntry, Query,
    Removed, Route, SkillEntry, Trust, safe_relative_path,
};
use serde_json::Value;
use std::collections::HashSet;

/// Counts reported after a parse, so the UI can say how many entries were unusable.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParseStats {
    pub kept: usize,
    pub skipped: usize,
}

fn non_empty_str<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn clean_tags(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(clean_name)
        .filter(|tag| !tag.is_empty())
        .take(12)
        .collect()
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn clean_description(text: &str) -> String {
    shorten(&collapse_whitespace(text), MAX_DESCRIPTION_CHARS)
}

fn trust_for_skill(level: Option<&str>) -> Trust {
    match level {
        Some("builtin") => Trust::Builtin,
        Some("trusted") => Trust::Trusted,
        _ => Trust::Community,
    }
}

fn trust_for_plugin(tier: Option<&str>) -> Trust {
    match tier {
        Some("official") => Trust::Builtin,
        Some("verified") => Trust::Trusted,
        _ => Trust::Community,
    }
}

fn is_sha40(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn parse_skills_index(bytes: &[u8]) -> Result<(String, Vec<SkillEntry>, ParseStats), String> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid skills index: {e}"))?;
    let skills = root
        .get("skills")
        .and_then(Value::as_array)
        .ok_or("skills index has no skills array")?;
    let generated_at = root
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut stats = ParseStats::default();
    let mut seen = HashSet::new();
    let mut entries = Vec::with_capacity(skills.len());
    for item in skills {
        let Some(object) = item.as_object() else {
            stats.skipped += 1;
            continue;
        };
        let (Some(raw_name), Some(source), Some(identifier)) = (
            non_empty_str(Some(item), "name"),
            non_empty_str(Some(item), "source"),
            non_empty_str(Some(item), "identifier"),
        ) else {
            stats.skipped += 1;
            continue;
        };
        let name = clean_name(raw_name);
        if name.is_empty() {
            stats.skipped += 1;
            continue;
        }
        if !seen.insert(identifier.to_string()) {
            stats.skipped += 1;
            continue;
        }
        let mut tags = clean_tags(object.get("tags"));
        tags.truncate(12);
        let resolved_github_id =
            non_empty_str(Some(item), "resolved_github_id").map(str::to_string);
        let owner = object
            .get("extra")
            .and_then(|extra| extra.get("owner"))
            .and_then(Value::as_str)
            .filter(|owner| !owner.trim().is_empty())
            .map(str::to_string);
        entries.push(SkillEntry {
            name,
            description: clean_description(
                item.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            source: source.to_string(),
            identifier: identifier.to_string(),
            trust: trust_for_skill(item.get("trust_level").and_then(Value::as_str)),
            repo: item
                .get("repo")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            path: item
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tags,
            resolved_github_id,
            owner,
        });
        stats.kept += 1;
    }
    Ok((generated_at, entries, stats))
}

fn requires_env(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            Value::String(name) => Some(name.trim().to_string()),
            Value::Object(object) => object
                .get("name")
                .and_then(Value::as_str)
                .map(|name| name.trim().to_string()),
            _ => None,
        })
        .filter(|name| !name.is_empty())
        .collect()
}

#[allow(clippy::type_complexity)]
pub fn parse_plugin_catalog(
    bytes: &[u8],
) -> Result<(String, Vec<PluginEntry>, Vec<Removed>, ParseStats), String> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid plugin catalog: {e}"))?;
    let plugins = root
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("plugin catalog has no entries array")?;
    let generated_at = root
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let removed: Vec<Removed> = root
        .get("removed")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = non_empty_str(Some(item), "name")?;
            Some(Removed {
                name: name.to_string(),
                reason: item
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                date: item
                    .get("date")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect();
    let mut stats = ParseStats::default();
    let mut seen = HashSet::new();
    let mut entries = Vec::with_capacity(plugins.len());
    for item in plugins {
        let Some(object) = item.as_object() else {
            stats.skipped += 1;
            continue;
        };
        let (Some(name), Some(repo), Some(sha)) = (
            non_empty_str(Some(item), "name"),
            non_empty_str(Some(item), "repo"),
            non_empty_str(Some(item), "sha"),
        ) else {
            stats.skipped += 1;
            continue;
        };
        let sha = sha.to_ascii_lowercase();
        if !is_sha40(&sha) {
            stats.skipped += 1;
            continue;
        }
        if !seen.insert(name.to_string()) {
            stats.skipped += 1;
            continue;
        }
        let capabilities = object.get("capabilities");
        let tier = item
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        entries.push(PluginEntry {
            name: name.to_string(),
            title: item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            description: clean_description(
                item.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            repo: repo.to_string(),
            sha,
            subdir: item
                .get("subdir")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            version: item
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            trust: trust_for_plugin(Some(&tier)),
            tier,
            category: item
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            maintainer: item
                .get("maintainer")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            docs_url: item
                .get("docs_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            requires_env: requires_env(capabilities.and_then(|c| c.get("requires_env"))),
            provides_tools: string_list(capabilities.and_then(|c| c.get("provides_tools"))),
            provides_hooks: string_list(capabilities.and_then(|c| c.get("provides_hooks"))),
            platforms: string_list(item.get("platforms")),
        });
        stats.kept += 1;
    }
    Ok((generated_at, entries, removed, stats))
}

pub fn clean_name(raw: &str) -> String {
    let mut text = raw.trim();
    if let Some(quote) = text.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let rest = &text[quote.len_utf8()..];
        text = match rest.find(quote) {
            Some(end) => &rest[..end],
            None => rest,
        };
    }
    let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
    shorten(&collapse_whitespace(&cleaned), 80)
}

fn valid_repo_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 100
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// `owner/repo`, exactly two valid parts.
fn split_repo(repo: &str) -> Option<(&str, &str)> {
    let (owner, name) = repo.split_once('/')?;
    if name.contains('/') || !valid_repo_part(owner) || !valid_repo_part(name) {
        return None;
    }
    Some((owner, name))
}

fn clean_route_path(path: &str) -> Result<String, String> {
    let path = path.strip_suffix('/').unwrap_or(path);
    if path.is_empty() {
        Ok(String::new())
    } else if safe_relative_path(path) {
        Ok(path.to_string())
    } else {
        Err(format!("path {path:?} is not a safe relative path"))
    }
}

fn github_route(repo: &str, path: &str) -> Route {
    if split_repo(repo).is_none() {
        return Route::Unsupported {
            reason: format!("repo {repo:?} is not a valid owner/repo"),
        };
    }
    match clean_route_path(path) {
        Ok(path) => Route::Github {
            repo: repo.to_string(),
            path,
            reference: None,
        },
        Err(reason) => Route::Unsupported { reason },
    }
}

fn valid_slug(slug: &str) -> bool {
    let mut bytes = slug.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    slug.len() <= 128
        && (first.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn skill_route(entry: &SkillEntry) -> Route {
    match entry.source.as_str() {
        "official" => github_route(HERMES_REPO, &entry.path),
        "github" => github_route(&entry.repo, &entry.path),
        "skills.sh" => {
            if let Some(resolved) = entry
                .resolved_github_id
                .as_deref()
                .filter(|resolved| !resolved.trim().is_empty())
            {
                let mut parts = resolved.splitn(3, '/');
                if let (Some(owner), Some(repo), Some(path)) =
                    (parts.next(), parts.next(), parts.next())
                    && valid_repo_part(owner)
                    && valid_repo_part(repo)
                    && !path.is_empty()
                {
                    return github_route(&format!("{owner}/{repo}"), path);
                }
            }
            github_route(&entry.repo, &entry.path)
        }
        "clawhub" => {
            let identifier = &entry.identifier;
            let (owner, slug) = match identifier.strip_prefix('@') {
                Some(rest) => match rest.split_once('/') {
                    Some((owner, slug)) if !owner.is_empty() && !slug.is_empty() => {
                        (Some(owner.to_string()), slug.to_string())
                    }
                    _ => {
                        return Route::Unsupported {
                            reason: format!("ClawHub identifier {identifier:?} is not valid"),
                        };
                    }
                },
                None => (entry.owner.clone(), identifier.clone()),
            };
            if !valid_slug(&slug) {
                return Route::Unsupported {
                    reason: format!("ClawHub slug {slug:?} is not valid"),
                };
            }
            Route::Clawhub { slug, owner }
        }
        other => Route::Unsupported {
            reason: format!("{other} skills cannot be installed yet"),
        },
    }
}

pub fn plugin_route(entry: &PluginEntry) -> Route {
    let Some(rest) = entry.repo.strip_prefix("https://github.com/") else {
        return Route::Unsupported {
            reason: "only GitHub-hosted plugins can be installed".into(),
        };
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let repo = rest.strip_suffix('/').unwrap_or(rest);
    if split_repo(repo).is_none() {
        return Route::Unsupported {
            reason: format!("repo {:?} is not a valid owner/repo", entry.repo),
        };
    }
    match clean_route_path(&entry.subdir) {
        Ok(path) => Route::Github {
            repo: repo.to_string(),
            path,
            reference: Some(entry.sha.to_ascii_lowercase()),
        },
        Err(reason) => Route::Unsupported { reason },
    }
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

struct Candidate<'a> {
    kind: Kind,
    index: usize,
    score: usize,
    trust: Trust,
    name_key: String,
    id: &'a str,
}

fn installed_key(lock: &Lock, kind: Kind, id: &str) -> Option<String> {
    lock.installed
        .iter()
        .find(|(_, entry)| entry.kind == kind && entry.id == id)
        .map(|(key, _)| key.clone())
}

/// Score one item against the lowercased query terms; `None` when a term is missing.
fn score_terms(name: &str, tags: &[String], description: &str, terms: &[String]) -> Option<usize> {
    let name = name.to_lowercase();
    let description = description.to_lowercase();
    let tags: Vec<String> = tags.iter().map(|tag| tag.to_lowercase()).collect();
    let mut score = 0;
    for term in terms {
        let in_name = name.contains(term);
        let in_tag = tags.iter().any(|tag| tag.contains(term));
        if !in_name && !in_tag && !description.contains(term) {
            return None;
        }
        score += if name == *term {
            100
        } else if name.starts_with(term) {
            60
        } else if in_name {
            40
        } else if tags.iter().any(|tag| tag == term) {
            30
        } else if description.contains(term) {
            10
        } else {
            0
        };
    }
    Some(score)
}

pub fn search(catalog: &Catalog, query: &Query, lock: &Lock) -> Page {
    let terms: Vec<String> = query
        .text
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut candidates = Vec::new();
    for (index, entry) in catalog.skills.iter().enumerate() {
        if query.kind.is_some_and(|kind| kind != Kind::Skill) {
            continue;
        }
        if let Some(source) = &query.source
            && source != &entry.source
        {
            continue;
        }
        if let Some(min_trust) = query.min_trust
            && entry.trust > min_trust
        {
            continue;
        }
        let score = if terms.is_empty() {
            0
        } else {
            let Some(score) = score_terms(&entry.name, &entry.tags, &entry.description, &terms)
            else {
                continue;
            };
            score
        };
        candidates.push(Candidate {
            kind: Kind::Skill,
            index,
            score,
            trust: entry.trust,
            name_key: entry.name.to_lowercase(),
            id: &entry.identifier,
        });
    }
    for (index, entry) in catalog.plugins.iter().enumerate() {
        if query.kind.is_some_and(|kind| kind != Kind::Plugin) {
            continue;
        }
        if let Some(source) = &query.source
            && source != "catalog"
        {
            continue;
        }
        if let Some(min_trust) = query.min_trust
            && entry.trust > min_trust
        {
            continue;
        }
        let name = if entry.title.is_empty() {
            &entry.name
        } else {
            &entry.title
        };
        let score = if terms.is_empty() {
            0
        } else {
            match score_terms(name, &[], &entry.description, &terms) {
                Some(score) => score,
                // The market id is also a name the owner may type.
                None => {
                    let id = entry.name.to_lowercase();
                    if terms.iter().all(|term| id.contains(term)) {
                        0
                    } else {
                        continue;
                    }
                }
            }
        };
        candidates.push(Candidate {
            kind: Kind::Plugin,
            index,
            score,
            trust: entry.trust,
            name_key: name.to_lowercase(),
            id: &entry.name,
        });
    }
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.trust.cmp(&b.trust))
            .then_with(|| a.name_key.cmp(&b.name_key))
            .then_with(|| a.id.cmp(b.id))
    });
    let total = candidates.len();
    let limit = match query.limit {
        0 => 50,
        limit => limit.min(200),
    };
    let items = candidates
        .into_iter()
        .skip(query.offset)
        .take(limit)
        .map(|candidate| {
            let mut listing = match candidate.kind {
                Kind::Skill => skill_listing(&catalog.skills[candidate.index]),
                Kind::Plugin => plugin_listing(&catalog.plugins[candidate.index]),
            };
            listing.installed = installed_key(lock, listing.kind, &listing.id);
            listing
        })
        .collect();
    Page { total, items }
}
