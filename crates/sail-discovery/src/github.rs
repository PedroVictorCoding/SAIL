//! GitHub activity detector (#451): owner-relevant events on the owner's repositories.
//!
//! # Contract (Antigravity implements the `unimplemented!()` bodies)
//!
//! **Events.** [`GithubEventKind`] lists what is worth noticing. Each event has a stable
//! `id` that is the dedupe identity: the same fact seen on two polls has the same id, so
//! the attention store keeps one item (`dedupe_key = "github:{id}"`).
//!
//! | Kind | id | Fact |
//! |---|---|---|
//! | `ReviewRequested` | `review:{repo}#{n}` | the owner is a requested reviewer on an open PR |
//! | `CiFailed` | `ci:{repo}#{n}:{head_sha}` | the latest check runs on the head of the owner's open PR include a failure |
//! | `PrMerged` | `merged:{repo}#{n}` | the owner's PR was merged |
//! | `IssueAssigned` | `assigned:{repo}#{n}` | an open issue is assigned to the owner |
//! | `ReleasePublished` | `release:{repo}:{tag}` | a release was published on a watched repo |
//!
//! A new commit on a failing PR is a new `head_sha`, so a second failure is a new item;
//! a re-run of the same commit is not.
//!
//! **Fetching ([`GhCliActivity`]).** Uses the `gh` CLI the owner already signed in to
//! (`SAIL_GH_BIN` or `gh`, as Forge's `GhCliClient` does), never a token of its own:
//! - `gh api user --jq .login` once for the owner's login (cached);
//! - `gh search prs --review-requested=@me --state=open --json …` for reviews;
//! - `gh search prs --author=@me --state=open --json …`, then for each (at most 10)
//!   `gh pr checks <n> --repo <r> --json name,state,bucket` for CI (a check in the `fail`
//!   bucket → CiFailed; head sha via `gh pr view <n> --repo <r> --json headRefOid`);
//! - `gh search prs --author=@me --merged --merged-at >=<since date> --json …` for merges;
//! - `gh search issues --assignee=@me --state=open --json …` for assignments;
//! - `gh release list --repo <r> --limit 3 --json tagName,publishedAt,name` for each repo
//!   in `watched_repos` (releases published after `since`).
//!
//! Run each command with a 30 s timeout, no shell, a cleared environment except `PATH`,
//! `HOME`, `GH_*`, `XDG_*`, `LANG`; parse JSON with serde. Any failure of one query is
//! logged and skipped; `fetch` fails only when `gh` is missing or not signed in
//! (`GithubError::Unavailable`), so the loop can say so once instead of each tick.
//! Bound every string (title 200 chars, repo 100, url 300).
//!
//! **Pacing ([`GithubPoller`]).** The initiative tick runs every 5 min; GitHub is polled
//! only when `due(now)`: never before `MIN_POLL_INTERVAL` (15 min) since the last attempt,
//! successful or not. `since` for a poll is the last successful poll (first poll: 3 days
//! back). The poller is in-memory; a restart polls once more, and dedupe absorbs it.
//!
//! **Signals ([`github_signal`]).** Deterministic, one signal per event:
//!
//! | Kind | urgency | relevance | novelty | provenance | title |
//! |---|---|---|---|---|---|
//! | `CiFailed` | 0.6 | 0.9 | 1.0 | Observation | `CI failed: {title}` |
//! | `ReviewRequested` | 0.5 | 0.8 | 1.0 | Observation | `Review requested: {title}` |
//! | `IssueAssigned` | 0.4 | 0.7 | 1.0 | Observation | `Assigned: {title}` |
//! | `PrMerged` | 0.15 | 0.5 | 0.8 | Observation | `Merged: {title}` |
//! | `ReleasePublished` | 0.2 | 0.5 | 0.9 | Observation | `Released {tag}` (body: repo) |
//!
//! `source = Source::GitHub`, `kind` = the snake_case kind, `confidence = 0.95`, body
//! `"{repo}#{n}"` (plus the failing check names for CI, at most 3), `link = Some(url)` when
//! the url starts with `https://github.com/` (else `None`), `deadline = None`,
//! `observed_at = now`. Events older than 7 days produce nothing. With these numbers a
//! failed CI run lands at Suggest or higher under default settings (the acceptance check).
//!
//! **Tests** (`crates/sail-discovery/tests/github_test.rs`, no network): a fake `gh`
//! (`#!/bin/sh` script dispatching on its arguments, printing fixture JSON) passed via
//! `GhCliActivity::with_bin`. Cover: each kind parsed into the right event and id; a failed
//! CI run on an open owner PR → one signal that `AttentionStore::ingest` delivers as
//! Suggest or Interrupt; a second poll with the same data adds no item; a new head sha
//! does; the poller refuses before 15 min and after a failure; `gh` missing →
//! `Unavailable`; one failing query doesn't lose the others; non-github urls get no link.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use sail_pulse::attention::Signal;
use serde::{Deserialize, Serialize};

pub const MIN_POLL_INTERVAL: Duration = Duration::minutes(15);
pub const COMMAND_TIMEOUT: StdDuration = StdDuration::from_secs(30);
/// Events older than this are history.
pub const MAX_EVENT_AGE_DAYS: i64 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubEventKind {
    ReviewRequested,
    CiFailed,
    PrMerged,
    IssueAssigned,
    ReleasePublished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GithubEvent {
    /// Stable dedupe identity (table above).
    pub id: String,
    pub kind: GithubEventKind,
    /// `owner/name`.
    pub repo: String,
    /// PR or issue number; `None` for releases.
    pub number: Option<u64>,
    /// PR / issue title, or release name.
    pub title: String,
    /// Release tag, for `ReleasePublished`.
    pub tag: Option<String>,
    /// Failing check names, for `CiFailed` (at most 3).
    pub failing_checks: Vec<String>,
    pub url: String,
    /// When it happened (updated / merged / published), as GitHub reports it.
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubError {
    /// `gh` is missing or not signed in.
    Unavailable(String),
}

pub trait GithubActivity: Send + Sync {
    /// Owner-relevant events since `since`. Partial results on partial failure.
    fn fetch(&self, since: DateTime<Utc>) -> Result<Vec<GithubEvent>, GithubError>;
}

/// `gh`-backed activity source.
pub struct GhCliActivity {
    pub bin: PathBuf,
    /// Repos whose releases are watched (`owner/name`). PR and issue queries are global.
    pub watched_repos: Vec<String>,
}

impl GhCliActivity {
    /// `SAIL_GH_BIN` or `gh`; watched repos from `SAIL_GITHUB_WATCH` (comma-separated).
    pub fn from_env() -> Self {
        let bin = std::env::var("SAIL_GH_BIN").unwrap_or_else(|_| "gh".into());
        let watched_repos = std::env::var("SAIL_GITHUB_WATCH")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_owned)
            .collect();
        Self::with_bin(bin.into(), watched_repos)
    }

    pub fn with_bin(bin: PathBuf, watched_repos: Vec<String>) -> Self {
        Self { bin, watched_repos }
    }
}

impl GithubActivity for GhCliActivity {
    fn fetch(&self, since: DateTime<Utc>) -> Result<Vec<GithubEvent>, GithubError> {
        let _ = since;
        unimplemented!("Antigravity: gh queries per the module doc")
    }
}

/// Decides when GitHub may be polled.
#[derive(Debug, Clone, Default)]
pub struct GithubPoller {
    last_attempt: Option<DateTime<Utc>>,
    last_success: Option<DateTime<Utc>>,
}

impl GithubPoller {
    pub fn due(&self, now: DateTime<Utc>) -> bool {
        let _ = now;
        unimplemented!("Antigravity: at most every MIN_POLL_INTERVAL")
    }

    /// `since` for the next poll.
    pub fn since(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let _ = now;
        unimplemented!("Antigravity: last success, or 3 days back")
    }

    pub fn record(&mut self, now: DateTime<Utc>, success: bool) {
        let _ = (now, success, &self.last_attempt, &self.last_success);
        unimplemented!("Antigravity")
    }
}

/// The signal for one event (empty when too old). Deterministic.
pub fn github_signal(event: &GithubEvent, now: DateTime<Utc>) -> Vec<Signal> {
    let _ = (event, now);
    unimplemented!("Antigravity: table in the module doc")
}
