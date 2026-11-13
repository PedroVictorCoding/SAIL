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

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use sail_pulse::attention::{Provenance, Signal, Source};
use serde::{Deserialize, Serialize};
use tracing::warn;

pub const MIN_POLL_INTERVAL: Duration = Duration::minutes(15);
pub const COMMAND_TIMEOUT: StdDuration = StdDuration::from_secs(30);
/// Events older than this are history.
pub const MAX_EVENT_AGE_DAYS: i64 = 7;

const MAX_TITLE_CHARS: usize = 200;
const MAX_REPO_CHARS: usize = 100;
const MAX_URL_CHARS: usize = 300;
const MAX_OPEN_PRS_FOR_CI: usize = 10;
const MAX_FAILING_CHECKS: usize = 3;

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
    login_cache: Mutex<Option<String>>,
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
        Self {
            bin,
            watched_repos,
            login_cache: Mutex::new(None),
        }
    }

    fn ensure_login(&self) -> Result<String, GithubError> {
        if let Ok(guard) = self.login_cache.lock()
            && let Some(login) = guard.as_ref()
        {
            return Ok(login.clone());
        }
        let out = self
            .run_gh(&["api", "user", "--jq", ".login"])
            .map_err(|e| GithubError::Unavailable(e.into_message()))?;
        let login = out.trim().to_string();
        if login.is_empty() {
            return Err(GithubError::Unavailable(
                "gh api user returned an empty login".into(),
            ));
        }
        if let Ok(mut guard) = self.login_cache.lock() {
            *guard = Some(login.clone());
        }
        Ok(login)
    }

    fn run_gh(&self, args: &[&str]) -> Result<String, GhRunError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        for (k, v) in std::env::vars_os() {
            let key = k.to_string_lossy();
            if matches!(key.as_ref(), "PATH" | "HOME" | "LANG")
                || key.starts_with("GH_")
                || key.starts_with("XDG_")
            {
                cmd.env(k, v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GhRunError::NotFound(format!("gh CLI not found at {}", self.bin.display()))
            } else {
                GhRunError::Failed(format!("failed to spawn gh: {e}"))
            }
        })?;

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_handle = std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = pipe.read_to_string(&mut buf);
            }
            buf
        });
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(mut pipe) = stderr_pipe {
                let _ = pipe.read_to_string(&mut buf);
            }
            buf
        });

        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_handle.join();
                        let _ = stderr_handle.join();
                        return Err(GhRunError::Failed("gh command timed out".into()));
                    }
                    std::thread::sleep(StdDuration::from_millis(10));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    return Err(GhRunError::Failed(format!("waiting for gh failed: {e}")));
                }
            }
        };

        let stdout = stdout_handle.join().unwrap_or_default();
        let stderr = stderr_handle.join().unwrap_or_default();
        if !status.success() {
            let detail = if stderr.trim().is_empty() {
                format!("gh exited with {status}")
            } else {
                stderr.trim().to_string()
            };
            return Err(GhRunError::Failed(detail));
        }
        Ok(stdout)
    }

    fn run_json<T: for<'de> Deserialize<'de>>(&self, args: &[&str]) -> Option<T> {
        let out = match self.run_gh(args) {
            Ok(out) => out,
            Err(err) => {
                warn!(?args, error = %err.into_message(), "gh query failed; skipping");
                return None;
            }
        };
        match serde_json::from_str::<T>(&out) {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                warn!(?args, error = %err, "gh JSON parse failed; skipping");
                None
            }
        }
    }
}

enum GhRunError {
    NotFound(String),
    Failed(String),
}

impl GhRunError {
    fn into_message(self) -> String {
        match self {
            Self::NotFound(m) | Self::Failed(m) => m,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhSearchItem {
    number: u64,
    title: String,
    url: String,
    repository: GhRepo,
    updated_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRepo {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct GhPrCheck {
    name: String,
    #[serde(default)]
    bucket: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrHead {
    head_ref_oid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRelease {
    tag_name: String,
    published_at: DateTime<Utc>,
    #[serde(default)]
    name: String,
}

impl GithubActivity for GhCliActivity {
    fn fetch(&self, since: DateTime<Utc>) -> Result<Vec<GithubEvent>, GithubError> {
        let _login = self.ensure_login()?;
        let mut events = Vec::new();
        let fields = "number,title,url,repository,updatedAt,closedAt";

        // 1. Review requests.
        if let Some(prs) = self.run_json::<Vec<GhSearchItem>>(&[
            "search",
            "prs",
            "--review-requested=@me",
            "--state=open",
            "--json",
            fields,
        ]) {
            for pr in prs {
                let repo = bound(&pr.repository.name_with_owner, MAX_REPO_CHARS);
                events.push(GithubEvent {
                    id: format!("review:{repo}#{}", pr.number),
                    kind: GithubEventKind::ReviewRequested,
                    repo,
                    number: Some(pr.number),
                    title: bound(&pr.title, MAX_TITLE_CHARS),
                    tag: None,
                    failing_checks: Vec::new(),
                    url: bound(&pr.url, MAX_URL_CHARS),
                    at: pr.updated_at,
                });
            }
        }

        // 2. CI failures on owner's open PRs.
        if let Some(open_prs) = self.run_json::<Vec<GhSearchItem>>(&[
            "search",
            "prs",
            "--author=@me",
            "--state=open",
            "--json",
            fields,
        ]) {
            for pr in open_prs.into_iter().take(MAX_OPEN_PRS_FOR_CI) {
                let num_str = pr.number.to_string();
                let repo = bound(&pr.repository.name_with_owner, MAX_REPO_CHARS);
                let Some(checks) = self.run_json::<Vec<GhPrCheck>>(&[
                    "pr",
                    "checks",
                    &num_str,
                    "--repo",
                    &repo,
                    "--json",
                    "name,state,bucket",
                ]) else {
                    continue;
                };
                let failing: Vec<String> = checks
                    .into_iter()
                    .filter(|c| c.bucket.eq_ignore_ascii_case("fail"))
                    .map(|c| bound(&c.name, MAX_REPO_CHARS))
                    .take(MAX_FAILING_CHECKS)
                    .collect();
                if failing.is_empty() {
                    continue;
                }
                let Some(head) = self.run_json::<GhPrHead>(&[
                    "pr",
                    "view",
                    &num_str,
                    "--repo",
                    &repo,
                    "--json",
                    "headRefOid",
                ]) else {
                    continue;
                };
                let sha = head.head_ref_oid.trim();
                if sha.is_empty() {
                    continue;
                }
                events.push(GithubEvent {
                    id: format!("ci:{repo}#{}:{sha}", pr.number),
                    kind: GithubEventKind::CiFailed,
                    repo,
                    number: Some(pr.number),
                    title: bound(&pr.title, MAX_TITLE_CHARS),
                    tag: None,
                    failing_checks: failing,
                    url: bound(&pr.url, MAX_URL_CHARS),
                    at: pr.updated_at,
                });
            }
        }

        // 3. Merged PRs since `since`.
        let merged_since = format!(">={}", since.format("%Y-%m-%d"));
        if let Some(merged) = self.run_json::<Vec<GhSearchItem>>(&[
            "search",
            "prs",
            "--author=@me",
            "--merged",
            "--merged-at",
            &merged_since,
            "--json",
            fields,
        ]) {
            for pr in merged {
                let repo = bound(&pr.repository.name_with_owner, MAX_REPO_CHARS);
                events.push(GithubEvent {
                    id: format!("merged:{repo}#{}", pr.number),
                    kind: GithubEventKind::PrMerged,
                    repo,
                    number: Some(pr.number),
                    title: bound(&pr.title, MAX_TITLE_CHARS),
                    tag: None,
                    failing_checks: Vec::new(),
                    url: bound(&pr.url, MAX_URL_CHARS),
                    at: pr.closed_at.unwrap_or(pr.updated_at),
                });
            }
        }

        // 4. Assigned open issues.
        if let Some(issues) = self.run_json::<Vec<GhSearchItem>>(&[
            "search",
            "issues",
            "--assignee=@me",
            "--state=open",
            "--json",
            fields,
        ]) {
            for issue in issues {
                let repo = bound(&issue.repository.name_with_owner, MAX_REPO_CHARS);
                events.push(GithubEvent {
                    id: format!("assigned:{repo}#{}", issue.number),
                    kind: GithubEventKind::IssueAssigned,
                    repo,
                    number: Some(issue.number),
                    title: bound(&issue.title, MAX_TITLE_CHARS),
                    tag: None,
                    failing_checks: Vec::new(),
                    url: bound(&issue.url, MAX_URL_CHARS),
                    at: issue.updated_at,
                });
            }
        }

        // 5. Releases on watched repos published after `since`.
        for watched in &self.watched_repos {
            let repo = bound(watched, MAX_REPO_CHARS);
            let Some(releases) = self.run_json::<Vec<GhRelease>>(&[
                "release",
                "list",
                "--repo",
                &repo,
                "--limit",
                "3",
                "--json",
                "tagName,publishedAt,name",
            ]) else {
                continue;
            };
            for rel in releases {
                if rel.published_at <= since {
                    continue;
                }
                let tag = bound(&rel.tag_name, MAX_REPO_CHARS);
                let title_raw = if rel.name.trim().is_empty() {
                    &tag
                } else {
                    &rel.name
                };
                events.push(GithubEvent {
                    id: format!("release:{repo}:{tag}"),
                    kind: GithubEventKind::ReleasePublished,
                    repo: repo.clone(),
                    number: None,
                    title: bound(title_raw, MAX_TITLE_CHARS),
                    tag: Some(tag.clone()),
                    failing_checks: Vec::new(),
                    url: bound(
                        &format!("https://github.com/{repo}/releases/tag/{tag}"),
                        MAX_URL_CHARS,
                    ),
                    at: rel.published_at,
                });
            }
        }

        Ok(events)
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
        self.last_attempt
            .is_none_or(|last| now - last >= MIN_POLL_INTERVAL)
    }

    /// `since` for the next poll.
    pub fn since(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        self.last_success.unwrap_or_else(|| now - Duration::days(3))
    }

    pub fn record(&mut self, now: DateTime<Utc>, success: bool) {
        self.last_attempt = Some(now);
        if success {
            self.last_success = Some(now);
        }
    }
}

/// The signal for one event (empty when too old). Deterministic.
pub fn github_signal(event: &GithubEvent, now: DateTime<Utc>) -> Vec<Signal> {
    if now - event.at > Duration::days(MAX_EVENT_AGE_DAYS) {
        return Vec::new();
    }

    let (kind, urgency, relevance, novelty, title, body) = match event.kind {
        GithubEventKind::CiFailed => {
            let base = format!("{}#{}", event.repo, event.number.unwrap_or(0));
            let body = if event.failing_checks.is_empty() {
                base
            } else {
                let checks = event
                    .failing_checks
                    .iter()
                    .take(MAX_FAILING_CHECKS)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{base} ({checks})")
            };
            (
                "ci_failed",
                0.6,
                0.9,
                1.0,
                format!("CI failed: {}", event.title),
                body,
            )
        }
        GithubEventKind::ReviewRequested => (
            "review_requested",
            0.5,
            0.8,
            1.0,
            format!("Review requested: {}", event.title),
            format!("{}#{}", event.repo, event.number.unwrap_or(0)),
        ),
        GithubEventKind::IssueAssigned => (
            "issue_assigned",
            0.4,
            0.7,
            1.0,
            format!("Assigned: {}", event.title),
            format!("{}#{}", event.repo, event.number.unwrap_or(0)),
        ),
        GithubEventKind::PrMerged => (
            "pr_merged",
            0.15,
            0.5,
            0.8,
            format!("Merged: {}", event.title),
            format!("{}#{}", event.repo, event.number.unwrap_or(0)),
        ),
        GithubEventKind::ReleasePublished => (
            "release_published",
            0.2,
            0.5,
            0.9,
            format!("Released {}", event.tag.as_deref().unwrap_or(&event.title)),
            event.repo.clone(),
        ),
    };

    let link = if event.url.starts_with("https://github.com/") {
        Some(event.url.clone())
    } else {
        None
    };

    vec![Signal {
        source: Source::GitHub,
        kind: kind.into(),
        dedupe_key: format!("github:{}", event.id),
        title,
        body,
        urgency,
        relevance,
        novelty,
        confidence: 0.95,
        deadline: None,
        provenance: Provenance::Observation,
        link,
        observed_at: now,
    }]
}

fn bound(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}
