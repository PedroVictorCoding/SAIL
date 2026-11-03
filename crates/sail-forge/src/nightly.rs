//! Unattended overnight runner: pull `forge-ready` issues, solve them serially with the
//! local model, retry once with the failure fed back, and publish verified patches as
//! draft PRs. Caps, a time window and a kill-switch file bound what it can do; it never
//! merges.

use async_trait::async_trait;
use chrono::{DateTime, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::admission::{AdmissionPolicy, IssueCandidate};
use crate::contracts::{ForgeJobReport, IssueEnqueueRequest};
use crate::github::{GithubClient, mark_open_prs};
use crate::orchestrator::{EFFECT_PUBLISH, ForgeOrchestrator};
use crate::publish::{PublishConfig, publish_draft_pr};
use sail_continuity::{ContinuityError, EffectGate};

/// Runner limits and schedule.
#[derive(Debug, Clone)]
pub struct NightlyConfig {
    pub enabled: bool,
    pub repo_slug: String,
    /// Local hours `[start, end)`; wraps past midnight when `start > end`.
    pub window_start_hour: u32,
    pub window_end_hour: u32,
    pub max_issues_per_run: usize,
    pub max_attempts_per_issue: u32,
    pub max_prs_per_day: u32,
    pub publish: bool,
    pub state_dir: PathBuf,
    pub admission: AdmissionPolicy,
    pub pr_labels: Vec<String>,
    pub base_branch: String,
}

/// `SAIL_FORGE_STATE_DIR`, else `forge/` under the SAIL data root.
pub fn default_state_dir() -> PathBuf {
    std::env::var("SAIL_FORGE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| sail_common::storage::data_path("forge"))
}

/// Move Forge data from the old defaults (`~/.local/state/sail/forge` and the review ledger
/// in `~/.sail/forge`) into the data root, once. Explicit overrides are left alone. Call at
/// startup of the daemon and the nightly runner, before any store opens.
pub fn migrate_legacy_forge_data() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let mut moves = Vec::new();
    if std::env::var_os("SAIL_FORGE_STATE_DIR").is_none() {
        moves.push((home.join(".local/state/sail/forge"), default_state_dir()));
    }
    if std::env::var_os("SAIL_DATA_DIR").is_none() {
        let dir = sail_common::storage::data_path("forge");
        for name in ["review_ledger.json", "review_ledger.lock"] {
            moves.push((home.join(".sail/forge").join(name), dir.join(name)));
        }
    }
    for (old, new) in moves {
        if let Err(error) = sail_common::storage::migrate_legacy(&old, &new) {
            tracing::error!(old = %old.display(), new = %new.display(), %error, "Forge data migration failed; the old copy is untouched");
        }
    }
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

impl Default for NightlyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            repo_slug: "PedroVictorCoding/SAIL".into(),
            window_start_hour: 1,
            window_end_hour: 7,
            max_issues_per_run: 5,
            max_attempts_per_issue: 2,
            max_prs_per_day: 3,
            publish: true,
            state_dir: default_state_dir(),
            admission: AdmissionPolicy::default(),
            pr_labels: vec!["forge-generated".into()],
            base_branch: "main".into(),
        }
    }
}

impl NightlyConfig {
    /// Read overrides from `SAIL_FORGE_*` environment variables.
    pub fn from_env() -> Self {
        let d = Self::default();
        let (start, end) = std::env::var("SAIL_FORGE_WINDOW")
            .ok()
            .and_then(|w| {
                let (a, b) = w.split_once('-')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            })
            .unwrap_or((d.window_start_hour, d.window_end_hour));
        Self {
            enabled: std::env::var("SAIL_FORGE_NIGHTLY").is_ok_and(|v| v == "1"),
            repo_slug: std::env::var("SAIL_FORGE_REPO").unwrap_or(d.repo_slug),
            window_start_hour: start,
            window_end_hour: end,
            max_issues_per_run: env_num("SAIL_FORGE_MAX_ISSUES", d.max_issues_per_run),
            max_attempts_per_issue: env_num("SAIL_FORGE_MAX_ATTEMPTS", d.max_attempts_per_issue),
            max_prs_per_day: env_num("SAIL_FORGE_MAX_PRS_PER_DAY", d.max_prs_per_day),
            publish: std::env::var("SAIL_FORGE_PUBLISH").map_or(true, |v| v != "0"),
            ..d
        }
    }

    pub fn kill_switch_path(&self) -> PathBuf {
        self.state_dir.join("PAUSE")
    }

    pub fn in_window(&self, hour: u32) -> bool {
        let (s, e) = (self.window_start_hour, self.window_end_hour);
        if s <= e {
            hour >= s && hour < e
        } else {
            hour >= s || hour < e
        }
    }
}

/// Durable per-issue history so retries, caps and "already published" survive restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueRecord {
    pub attempts: u32,
    pub last_outcome: String,
    pub last_reason: Option<String>,
    pub last_job_id: Option<String>,
    pub pr_url: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NightlyState {
    pub issues: BTreeMap<u64, IssueRecord>,
    /// PRs opened per local date (`YYYY-MM-DD`).
    pub prs_opened: BTreeMap<String, u32>,
    pub last_run_date: Option<String>,
}

impl NightlyState {
    fn path(dir: &Path) -> PathBuf {
        dir.join("nightly_state.json")
    }

    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("nightly_state.json.tmp.{}", uuid::Uuid::new_v4()));
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, Self::path(dir))
    }
}

/// How a job is executed. Production runs the local solver; tests inject patches.
#[async_trait]
pub trait JobExecutor: Send + Sync {
    async fn execute(
        &self,
        orchestrator: &ForgeOrchestrator,
        job_id: &str,
    ) -> Result<ForgeJobReport, String>;
}

pub struct LocalSolverExecutor;

#[async_trait]
impl JobExecutor for LocalSolverExecutor {
    async fn execute(
        &self,
        orchestrator: &ForgeOrchestrator,
        job_id: &str,
    ) -> Result<ForgeJobReport, String> {
        orchestrator.run_job_lifecycle(job_id, None).await
    }
}

/// One line of the morning digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestEntry {
    pub issue: u64,
    pub title: String,
    pub outcome: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct NightlyDigest {
    pub date: String,
    pub dry_run: bool,
    pub halted: Option<String>,
    pub entries: Vec<DigestEntry>,
    pub rejected: Vec<(u64, Vec<String>)>,
}

impl NightlyDigest {
    pub fn to_markdown(&self) -> String {
        let mut md = format!(
            "# Forge nightly digest {}{}\n\n",
            self.date,
            if self.dry_run { " (dry run)" } else { "" }
        );
        if let Some(h) = &self.halted {
            md.push_str(&format!("**Run halted:** {h}\n\n"));
        }
        if self.entries.is_empty() {
            md.push_str(&format!(
                "**Summary:** 0 worked, {} not admitted\n\n",
                self.rejected.len()
            ));
        } else {
            let mut counts: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for entry in &self.entries {
                *counts.entry(&entry.outcome).or_default() += 1;
            }
            let outcome_parts: Vec<String> = counts
                .iter()
                .map(|(outcome, count)| format!("{count} {outcome}"))
                .collect();
            md.push_str(&format!(
                "**Summary:** {} worked ({}), {} not admitted\n\n",
                self.entries.len(),
                outcome_parts.join(", "),
                self.rejected.len()
            ));
        }
        md.push_str("## Worked issues\n");
        if self.entries.is_empty() {
            md.push_str("_none_\n");
        }
        for e in &self.entries {
            md.push_str(&format!(
                "- #{} {}: **{}**. {}\n",
                e.issue, e.title, e.outcome, e.detail
            ));
        }
        md.push_str("\n## Not admitted\n");
        if self.rejected.is_empty() {
            md.push_str("_none_\n");
        }
        for (n, reasons) in &self.rejected {
            md.push_str(&format!("- #{n}: {}\n", reasons.join("; ")));
        }
        md
    }
}

/// Failures that are about the host (no model server), not the patch: stop the run and
/// do not spend the issue's attempt.
fn is_infrastructure_failure(reason: &str) -> bool {
    reason.contains("Local model unavailable")
        || reason.contains("unreachable")
        || reason.contains("inference server")
        || reason.contains("Concurrency semaphore")
        || reason.contains("Failed to create isolated worktree")
        || (reason.contains("Model request to ") && reason.contains(" timed out"))
        || reason.contains("Cannot connect to model endpoint")
        || reason.contains("Model transport failure at")
}

fn feedback_body(issue: &IssueCandidate, record: &IssueRecord) -> String {
    match (&record.last_reason, record.attempts) {
        (Some(reason), n) if n > 0 && record.last_outcome != "aborted" => format!(
            "{}\n\n## Previous attempt feedback\nA previous attempt was rejected: {reason}\nFix the cause; do not repeat the same change.\n",
            issue.body
        ),
        _ => issue.body.clone(),
    }
}

pub struct ForgeNightlyRunner {
    orchestrator: Arc<ForgeOrchestrator>,
    github: Arc<dyn GithubClient>,
    executor: Arc<dyn JobExecutor>,
    config: NightlyConfig,
    publish: Option<PublishConfig>,
}

impl ForgeNightlyRunner {
    pub fn new(
        orchestrator: Arc<ForgeOrchestrator>,
        github: Arc<dyn GithubClient>,
        config: NightlyConfig,
    ) -> Self {
        let publish = config.publish.then(|| PublishConfig {
            repo_root: orchestrator.config().workspace_root.clone(),
            repo_slug: config.repo_slug.clone(),
            scratch_dir: config.state_dir.join("publish"),
            base_branch: config.base_branch.clone(),
            labels: config.pr_labels.clone(),
        });
        Self {
            orchestrator,
            github,
            executor: Arc::new(LocalSolverExecutor),
            config,
            publish,
        }
    }

    pub fn with_executor(mut self, executor: Arc<dyn JobExecutor>) -> Self {
        self.executor = executor;
        self
    }

    pub fn with_publish_config(mut self, publish: Option<PublishConfig>) -> Self {
        self.publish = publish;
        self
    }

    pub fn config(&self) -> &NightlyConfig {
        &self.config
    }

    pub fn is_paused(&self) -> bool {
        self.config.kill_switch_path().exists()
    }

    fn today() -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }

    fn should_stop(&self, respect_window: bool) -> Option<String> {
        if self.is_paused() {
            return Some(format!(
                "kill-switch present at {}",
                self.config.kill_switch_path().display()
            ));
        }
        if respect_window && !self.config.in_window(Local::now().hour()) {
            return Some("outside the configured run window".into());
        }
        None
    }

    /// Fetch candidates and split them into workable issues and rejections.
    fn select_work(
        &self,
        state: &NightlyState,
        digest: &mut NightlyDigest,
    ) -> Result<Vec<IssueCandidate>, String> {
        let mut issues = self.github.list_open_issues(
            &self.config.repo_slug,
            &self.config.admission.required_label,
        )?;
        let prs = self.github.open_pull_requests(&self.config.repo_slug)?;
        mark_open_prs(&mut issues, &prs);

        let (ready, rejected) = self.config.admission.select(&issues);
        digest.rejected = rejected;

        let mut work = Vec::new();
        for issue in ready {
            let record = state.issues.get(&issue.number).cloned().unwrap_or_default();
            // Already published, or verified and waiting for `publish_pending`.
            if record.pr_url.is_some() || record.last_outcome == "ready" {
                continue;
            }
            // Never start a second job for an issue whose job is still active or whose
            // outcome needs the user (e.g. a publish interrupted by a crash).
            if let Some(task) = self
                .orchestrator
                .active_task_for_issue(&self.config.repo_slug, issue.number)
            {
                let why = if task.state == sail_continuity::TaskState::NeedsIntervention {
                    format!(
                        "job {} needs intervention: {}",
                        task.id,
                        task.reason.unwrap_or_default()
                    )
                } else {
                    format!("job {} is already {}", task.id, task.state)
                };
                digest.rejected.push((issue.number, vec![why]));
                continue;
            }
            if record.attempts >= self.config.max_attempts_per_issue {
                digest.rejected.push((
                    issue.number,
                    vec![format!(
                        "attempts exhausted ({}); last: {}",
                        record.attempts,
                        record.last_reason.unwrap_or_default()
                    )],
                ));
                continue;
            }
            work.push(issue.clone());
        }
        work.truncate(self.config.max_issues_per_run);
        Ok(work)
    }

    /// Publish a ready job if publishing is enabled and the daily cap allows it.
    fn try_publish(&self, state: &mut NightlyState, job_id: &str) -> (String, String) {
        let Some(publish) = &self.publish else {
            return (
                "ready".into(),
                format!("verified; publishing disabled (job {job_id})"),
            );
        };
        let today = Self::today();
        let opened = state.prs_opened.get(&today).copied().unwrap_or(0);
        if opened >= self.config.max_prs_per_day {
            return (
                "ready".into(),
                format!(
                    "verified; daily PR cap ({}) reached, will publish next run",
                    self.config.max_prs_per_day
                ),
            );
        }
        let cp = match self.orchestrator.checkpoint_store().load(job_id) {
            Ok(Some(cp)) => cp,
            _ => return ("publish_failed".into(), "checkpoint missing".into()),
        };

        // Journal the push + PR so a crash cannot make it happen twice or be forgotten.
        let continuity = self.orchestrator.continuity();
        let description = format!(
            "push branch agent/forge-issue-{} and open a draft PR",
            cp.issue_number
        );
        let journaled = match continuity.begin_effect(job_id, EFFECT_PUBLISH, &description) {
            Ok(EffectGate::Proceed) => true,
            Ok(EffectGate::AlreadyDone { receipt }) => {
                info!("Forge nightly: job {job_id} was already published ({receipt})");
                return ("published".into(), receipt);
            }
            Err(ContinuityError::NotFound(_)) => {
                warn!(
                    "Forge nightly: job {job_id} has no Continuity record; publishing unjournaled"
                );
                false
            }
            Err(e @ ContinuityError::EffectInDoubt { .. }) => {
                return ("needs_intervention".into(), e.to_string());
            }
            Err(e) => return ("publish_failed".into(), e.to_string()),
        };
        let result = publish_draft_pr(publish, &cp, self.github.as_ref());
        if journaled {
            let recorded = match &result {
                Ok(url) => continuity.commit_effect(job_id, EFFECT_PUBLISH, url),
                Err(e) => continuity.fail_effect(job_id, EFFECT_PUBLISH, e),
            };
            if let Err(e) = recorded {
                warn!("Forge nightly: could not journal publish of {job_id}: {e}");
            }
        }
        match result {
            Ok(url) => {
                *state.prs_opened.entry(today).or_insert(0) += 1;
                ("published".into(), url)
            }
            Err(e) => ("publish_failed".into(), e),
        }
    }

    /// Publish jobs that finished verified in an earlier run but were held back (cap,
    /// restart) before a PR was opened.
    fn publish_pending(&self, state: &mut NightlyState, digest: &mut NightlyDigest) {
        let pending: Vec<(u64, String)> = state
            .issues
            .iter()
            .filter(|(_, r)| r.pr_url.is_none() && r.last_outcome == "ready")
            .filter_map(|(n, r)| r.last_job_id.clone().map(|j| (*n, j)))
            .collect();
        for (issue, job_id) in pending {
            let (outcome, detail) = self.try_publish(state, &job_id);
            if let Some(r) = state.issues.get_mut(&issue) {
                if outcome == "published" {
                    r.pr_url = Some(detail.clone());
                }
                r.last_outcome = outcome.clone();
                r.updated_at = Some(Utc::now());
            }
            if outcome != "ready" {
                digest.entries.push(DigestEntry {
                    issue,
                    title: String::new(),
                    outcome,
                    detail,
                });
            }
        }
    }

    /// One full pass. `respect_window` is set by the scheduler; manual runs ignore it.
    pub async fn run_once(&self, dry_run: bool, respect_window: bool) -> NightlyDigest {
        let mut digest = NightlyDigest {
            date: Self::today(),
            dry_run,
            ..Default::default()
        };
        if let Some(stop) = self.should_stop(respect_window) {
            digest.halted = Some(stop);
            return self.finish(digest);
        }

        let mut state = NightlyState::load(&self.config.state_dir);
        let work = match self.select_work(&state, &mut digest) {
            Ok(w) => w,
            Err(e) => {
                digest.halted = Some(format!("GitHub intake failed: {e}"));
                return self.finish(digest);
            }
        };
        if dry_run {
            for issue in &work {
                digest.entries.push(DigestEntry {
                    issue: issue.number,
                    title: issue.title.clone(),
                    outcome: "would_attempt".into(),
                    detail: String::new(),
                });
            }
            return self.finish(digest);
        }

        self.publish_pending(&mut state, &mut digest);
        let _ = state.save(&self.config.state_dir);

        'issues: for issue in work {
            loop {
                if let Some(stop) = self.should_stop(respect_window) {
                    digest.halted = Some(stop);
                    break 'issues;
                }
                let mut record = state.issues.get(&issue.number).cloned().unwrap_or_default();
                if record.attempts >= self.config.max_attempts_per_issue {
                    break;
                }

                let req = IssueEnqueueRequest {
                    repo: self.config.repo_slug.clone(),
                    issue_number: issue.number,
                    priority: Some("nightly".into()),
                    issue_title: Some(issue.title.clone()),
                    issue_body: Some(feedback_body(&issue, &record)),
                    ..Default::default()
                };
                let job_id = match self.orchestrator.enqueue_managed_issue(req) {
                    Ok(id) => id,
                    Err(e) => {
                        digest.halted = Some(format!("enqueue failed: {e}"));
                        break 'issues;
                    }
                };
                record.attempts += 1;
                record.last_job_id = Some(job_id.clone());
                record.last_outcome = "running".into();
                record.updated_at = Some(Utc::now());
                state.issues.insert(issue.number, record.clone());
                let _ = state.save(&self.config.state_dir);
                info!(
                    "Forge nightly: issue #{} attempt {} as job {}",
                    issue.number, record.attempts, job_id
                );

                let result = self.executor.execute(&self.orchestrator, &job_id).await;
                let (outcome, detail, retry) = match result {
                    Ok(report) if report.ready_to_publish => {
                        let (o, d) = self.try_publish(&mut state, &job_id);
                        (o, d, false)
                    }
                    Ok(report) => {
                        let reason = report
                            .review_result
                            .map(|r| r.verdict.detected_bugs.join("; "))
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "completed without a passing review".into());
                        ("rejected".into(), reason, true)
                    }
                    Err(reason) if is_infrastructure_failure(&reason) => {
                        record.attempts -= 1;
                        record.last_outcome = "aborted".into();
                        record.last_reason = Some(reason.clone());
                        state.issues.insert(issue.number, record);
                        let _ = state.save(&self.config.state_dir);
                        warn!("Forge nightly halted: {}", reason);
                        digest.entries.push(DigestEntry {
                            issue: issue.number,
                            title: issue.title.clone(),
                            outcome: "aborted".into(),
                            detail: reason.clone(),
                        });
                        digest.halted = Some(format!("infrastructure failure: {reason}"));
                        break 'issues;
                    }
                    Err(reason) => ("failed".into(), reason, true),
                };

                record.last_outcome = outcome.clone();
                record.last_reason = (outcome != "published").then(|| detail.clone());
                if outcome == "published" {
                    record.pr_url = Some(detail.clone());
                }
                record.updated_at = Some(Utc::now());
                let exhausted = record.attempts >= self.config.max_attempts_per_issue;
                state.issues.insert(issue.number, record.clone());
                let _ = state.save(&self.config.state_dir);

                if !retry || exhausted {
                    digest.entries.push(DigestEntry {
                        issue: issue.number,
                        title: issue.title.clone(),
                        outcome: if retry {
                            format!("{outcome} after {} attempts", record.attempts)
                        } else {
                            outcome
                        },
                        detail,
                    });
                    break;
                }
            }
        }

        state.last_run_date = Some(Self::today());
        let _ = state.save(&self.config.state_dir);
        self.finish(digest)
    }

    fn finish(&self, digest: NightlyDigest) -> NightlyDigest {
        let dir = self.config.state_dir.join("digests");
        if !digest.dry_run && std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(
                dir.join(format!("{}.md", digest.date)),
                digest.to_markdown(),
            );
        }
        info!(
            "Forge nightly finished: {} worked, {} not admitted{}",
            digest.entries.len(),
            digest.rejected.len(),
            digest
                .halted
                .as_deref()
                .map(|h| format!(", halted: {h}"))
                .unwrap_or_default()
        );
        digest
    }

    /// Wake every 10 minutes and run once per night inside the window.
    pub fn spawn_scheduler(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "Forge nightly scheduler active for {} (window {:02}:00-{:02}:00)",
                self.config.repo_slug, self.config.window_start_hour, self.config.window_end_hour
            );
            loop {
                let today = Self::today();
                let ran_today = NightlyState::load(&self.config.state_dir)
                    .last_run_date
                    .as_deref()
                    == Some(today.as_str());
                if !ran_today && !self.is_paused() && self.config.in_window(Local::now().hour()) {
                    self.run_once(false, true).await;
                }
                tokio::time::sleep(Duration::from_secs(600)).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_handles_wraparound() {
        let mut c = NightlyConfig::default();
        assert!(c.in_window(1) && c.in_window(6));
        assert!(!c.in_window(7) && !c.in_window(0));
        c.window_start_hour = 22;
        c.window_end_hour = 6;
        assert!(c.in_window(23) && c.in_window(0) && c.in_window(5));
        assert!(!c.in_window(6) && !c.in_window(21));
    }

    #[test]
    fn feedback_is_appended_only_on_retry() {
        let issue = IssueCandidate {
            number: 1,
            body: "## Acceptance\n- [ ] x".into(),
            ..Default::default()
        };
        let mut record = IssueRecord::default();
        assert_eq!(feedback_body(&issue, &record), issue.body);
        record.attempts = 1;
        record.last_reason = Some("[new_unwrap] src/lib.rs".into());
        let body = feedback_body(&issue, &record);
        assert!(body.starts_with(&issue.body));
        assert!(body.contains("## Previous attempt feedback"));
        assert!(body.contains("new_unwrap"));
        record.last_outcome = "aborted".into();
        record.last_reason = Some("Cannot connect to model endpoint http://model.test".into());
        assert_eq!(feedback_body(&issue, &record), issue.body);
    }

    #[test]
    fn infrastructure_failures_are_detected() {
        assert!(is_infrastructure_failure(
            "Solver failed to produce a valid diff: Endpoint http://x unreachable: refused"
        ));
        assert!(is_infrastructure_failure(
            "Local model unavailable after 4 attempt(s): http://127.0.0.1:8081/v1/chat/completions: no reply within 600s (model busy)"
        ));
        assert!(!is_infrastructure_failure(
            "Pre-review gates failed: [new_unwrap] src/lib.rs"
        ));
        for reason in [
            "Model request to http://127.0.0.1:8081/v1/chat/completions timed out (deadline 600s). The server may be busy or generating slowly; configure SAIL_FORGE_MODEL_TIMEOUT_SECS for this host.",
            "Cannot connect to model endpoint http://localhost:9090/v1/chat/completions: connection refused",
            "Model transport failure at http://model.test/completions: connection reset",
        ] {
            assert!(is_infrastructure_failure(reason), "{reason}");
            let wrapped = format!("Solver failed to produce a valid diff: {reason}");
            assert!(is_infrastructure_failure(&wrapped), "{wrapped}");
        }
        for reason in [
            "Pre-review gates failed: [protected_path] src/config.rs",
            "Tests failed: cargo test timed out",
            "Tests failed: assertion failed in model_transport_failure",
            "Self-review rejected: missing requirements",
            "External review rejected: model request parser is broken",
            "Model request to http://model.test returned an invalid patch",
        ] {
            assert!(!is_infrastructure_failure(reason), "{reason}");
        }
    }

    #[test]
    fn to_markdown_empty_digest() {
        let digest = NightlyDigest {
            date: "2026-11-03".into(),
            dry_run: false,
            halted: None,
            entries: vec![],
            rejected: vec![
                (10, vec!["missing label".into()]),
                (11, vec!["blocked".into()]),
            ],
        };
        let md = digest.to_markdown();
        assert!(md.starts_with(
            "# Forge nightly digest 2026-11-03\n\n**Summary:** 0 worked, 2 not admitted\n\n## Worked issues\n_none_\n"
        ));
    }

    #[test]
    fn to_markdown_mixed_outcomes() {
        let digest = NightlyDigest {
            date: "2026-11-03".into(),
            dry_run: true,
            halted: None,
            entries: vec![
                DigestEntry {
                    issue: 101,
                    title: "Issue 101".into(),
                    outcome: "pr_created".into(),
                    detail: "opened PR".into(),
                },
                DigestEntry {
                    issue: 102,
                    title: "Issue 102".into(),
                    outcome: "failure".into(),
                    detail: "gates failed".into(),
                },
                DigestEntry {
                    issue: 103,
                    title: "Issue 103".into(),
                    outcome: "pr_created".into(),
                    detail: "opened PR".into(),
                },
            ],
            rejected: vec![(104, vec!["no targets".into()])],
        };
        let md = digest.to_markdown();
        assert!(md.contains(
            "# Forge nightly digest 2026-11-03 (dry run)\n\n**Summary:** 3 worked (1 failure, 2 pr_created), 1 not admitted\n\n## Worked issues\n"
        ));
    }

    #[test]
    fn to_markdown_halted_run() {
        let digest = NightlyDigest {
            date: "2026-11-03".into(),
            dry_run: false,
            halted: Some("Local model unavailable".into()),
            entries: vec![DigestEntry {
                issue: 50,
                title: "Issue 50".into(),
                outcome: "infra_failure".into(),
                detail: "timeout".into(),
            }],
            rejected: vec![],
        };
        let md = digest.to_markdown();
        assert!(md.starts_with(
            "# Forge nightly digest 2026-11-03\n\n**Run halted:** Local model unavailable\n\n**Summary:** 1 worked (1 infra_failure), 0 not admitted\n\n## Worked issues\n"
        ));
    }
}
