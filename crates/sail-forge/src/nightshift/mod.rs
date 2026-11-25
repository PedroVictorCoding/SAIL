//! Night Shift: the owner states work in the day, SAIL shapes it into a work order,
//! Forge builds it overnight in the window, and the morning review ships or redoes it.
//!
//! Everything here stays inside the SAIL data root until the owner chooses **Ship**
//! (invariant 1). The deterministic core (work-order rendering, validation, planning and
//! the morning report) is pure: `now` is a parameter and there is no I/O, clock or
//! randomness. Model calls shape a work order and solve code; they never compose the
//! morning greeting or a result summary.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

pub mod external;
pub mod morning;
pub mod plan;
pub mod result;
pub mod shape;
pub mod store;

/// An errand identifier: `"e-"` followed by 12 lowercase hex characters.
///
/// The text form is the only serialized form; [`ErrandId::parse`] rejects anything else,
/// while the serde impl stays transparent so stored fixtures round-trip unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ErrandId(String);

impl ErrandId {
    /// Parse and validate an id. Returns `None` unless it is `"e-"` plus 12 lowercase
    /// hex characters.
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix("e-")?;
        if hex.len() == 12 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Some(Self(s.to_string()))
        } else {
            None
        }
    }

    /// Build an id from six random bytes. The caller supplies randomness so the rest of
    /// this module stays deterministic.
    pub fn new(rand6: [u8; 6]) -> Self {
        let mut hex = String::with_capacity(14);
        hex.push_str("e-");
        for byte in rand6 {
            hex.push_str(&format!("{byte:02x}"));
        }
        Self(hex)
    }

    /// The serialized id text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where the owner stated the errand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrandSource {
    Voice,
    Web,
    Phone,
    Telegram,
}

/// The shaper's estimate of the work. `L` is never planned: it is returned to the owner
/// as "too big for one night; split it".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    S,
    M,
    L,
}

/// A shaped, approvable unit of work. Limits are documented on the fields and enforced by
/// `shape::validate` (K2): title 1..=100, why ≤ 600, 1..=8 acceptance items of 1..=200,
/// 1..=6 existing non-protected repo-relative target files, solver notes ≤ 1500.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkOrder {
    pub title: String,
    pub why: String,
    pub acceptance: Vec<String>,
    pub target_files: Vec<String>,
    pub size: Size,
    #[serde(default)]
    pub notes_for_solver: String,
}

impl WorkOrder {
    /// Render the order as a GitHub-issue-shaped body so the existing admission policy and
    /// target-file parser accept it unchanged. Pure.
    ///
    /// The owner-feedback chunk is appended once per note, as specified by the contract.
    pub fn to_issue_body(&self, owner_notes: &[OwnerNote]) -> String {
        let mut body = format!("# {}\n\n{}\n\n## Acceptance\n", self.title, self.why);
        for item in &self.acceptance {
            body.push_str("- [ ] ");
            body.push_str(item);
            body.push('\n');
        }
        body.push_str("\n## Target files\n");
        for path in &self.target_files {
            body.push_str("- `");
            body.push_str(path);
            body.push_str("`\n");
        }
        if !self.notes_for_solver.is_empty() {
            body.push_str("\n## Notes\n");
            body.push_str(&self.notes_for_solver);
            body.push('\n');
        }
        for note in owner_notes {
            body.push_str("\n\n## Owner feedback\n- ");
            body.push_str(&note.at.to_rfc3339());
            body.push_str(": ");
            body.push_str(&note.text);
        }
        body
    }
}

/// A reshape or redo note from the owner. `text` is 1..=1000 chars (enforced by the store
/// and the HTTP layer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerNote {
    pub at: DateTime<Utc>,
    pub text: String,
}

/// Where a night job ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RanOnKind {
    Host,
    Mesh,
    Pooled,
    External,
}

/// The machine and model that produced a result: `"host"`, a worker name, or
/// `"Claude Code"` / `"Codex"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RanOn {
    pub kind: RanOnKind,
    pub name: String,
    pub model: String,
}

/// One changed file with its diff line counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// Test counts from the real run. `exit_ok` is the process exit status, never the model's
/// claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestSummary {
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
    pub duration_secs: u64,
    pub exit_ok: bool,
}

/// One deterministic gate result. `blocking` comes from `gates.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    pub name: String,
    pub passed: bool,
    pub blocking: bool,
    pub detail: String,
}

/// The self-review verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub passed: bool,
    pub concerns: Vec<String>,
}

/// Everything the morning review shows for one job. `Ready` is only ever produced when
/// the real tests passed, every blocking gate passed and self-review did not reject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightResult {
    pub job_id: String,
    /// 40 hex characters.
    pub base_commit: String,
    pub ran_on: RanOn,
    pub files: Vec<FileChange>,
    /// `None` means tests never ran; `summary` then says why.
    pub tests: Option<TestSummary>,
    pub gates: Vec<GateResult>,
    pub review: Option<ReviewSummary>,
    /// Deterministic template from `result::summary`, ≤ 300 chars.
    pub summary: String,
    /// Model-written note, ≤ 1200 chars, always shown labelled as model-written.
    #[serde(default)]
    pub model_note: Option<String>,
    pub patch_bytes: u64,
    pub finished_at: DateTime<Utc>,
}

/// The errand lifecycle. Internally tagged: `{"state":"proposed","order":{…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ErrandState {
    Shaping,
    Proposed {
        order: WorkOrder,
    },
    Unshapeable {
        reasons: Vec<String>,
    },
    Queued {
        order: WorkOrder,
        approved_at: DateTime<Utc>,
    },
    Working {
        order: WorkOrder,
        job_id: String,
        /// `"planning"`, `"coding"`, `"testing"` or `"reviewing"`.
        stage: String,
        started_at: DateTime<Utc>,
        ran_on: RanOn,
    },
    Ready {
        order: WorkOrder,
        result: NightResult,
    },
    NeedsYou {
        order: WorkOrder,
        result: NightResult,
        question: String,
    },
    Failed {
        order: WorkOrder,
        result: Option<NightResult>,
        reason: String,
    },
    Shipping {
        order: WorkOrder,
        result: NightResult,
    },
    Shipped {
        order: WorkOrder,
        result: NightResult,
        pr_url: String,
        shipped_at: DateTime<Utc>,
    },
    Dropped {
        at: DateTime<Utc>,
    },
}

/// One owner errand. `ask` stores the owner's words unchanged (1..=2000 chars).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Errand {
    pub id: ErrandId,
    pub ask: String,
    pub source: ErrandSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Nights tried, across redos.
    pub attempts: u32,
    /// Whether an external CLI saw this errand.
    pub external_used: bool,
    /// Reshape and redo notes, oldest first.
    pub notes: Vec<OwnerNote>,
    pub state: ErrandState,
}

/// Night Shift settings. Defaults are owner decisions or the existing Forge defaults:
/// disabled until the owner turns it on; 01:00–07:00; 5 approved errands;
/// use the Mesh when ready; no external help; greet in the morning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NightShiftSettings {
    pub enabled: bool,
    pub window_start_hour: u8,
    pub window_end_hour: u8,
    pub max_items: u8,
    /// Compatibility with older clients. Night Shift only schedules approved errands.
    #[serde(skip_deserializing)]
    pub include_forge_ready_issues: bool,
    pub use_mesh: bool,
    /// Prefer the pooled heavy model for Night Shift errands.
    pub use_heavy: bool,
    /// Allow the host model when the heavy lane is unavailable.
    pub fallback_to_host: bool,
    pub external_solves_per_night: u8,
    pub external_provider: Option<String>,
    pub morning_greeting: bool,
}

impl Default for NightShiftSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            window_start_hour: 1,
            window_end_hour: 7,
            max_items: 5,
            include_forge_ready_issues: false,
            use_mesh: true,
            use_heavy: true,
            fallback_to_host: false,
            external_solves_per_night: 0,
            external_provider: None,
            morning_greeting: true,
        }
    }
}

/// One item the night can run: an owner errand or an admitted GitHub issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NightItem {
    Errand { id: ErrandId },
    Issue { number: u64 },
}

/// An item the plan left out, with the plain-words reason shown to the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deferred {
    pub item: NightItem,
    pub reason: String,
}

/// The night's ordered plan and its deferrals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightPlan {
    pub items: Vec<NightItem>,
    pub deferred: Vec<Deferred>,
}

/// How one planned item ended overnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MorningOutcome {
    Ready,
    NeedsYou,
    Failed,
    NotReached,
}

/// One line of the morning report, in plan order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MorningItem {
    pub item: NightItem,
    pub title: String,
    pub outcome: MorningOutcome,
    pub line: String,
}

/// The morning report: deterministic counts, per-item lines and at most two spoken
/// sentences (empty when nothing ran).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MorningReport {
    /// Local `YYYY-MM-DD` of the morning.
    pub date: String,
    pub ready: u32,
    pub needs_you: u32,
    pub failed: u32,
    pub not_reached: u32,
    pub items: Vec<MorningItem>,
    pub spoken: String,
    /// For example `"the model server stopped responding at 03:12"`.
    #[serde(default)]
    pub halted: Option<String>,
}

/// The local model that shapes asks into work orders, at the background work priority.
#[async_trait]
pub trait ShaperModel: Send + Sync {
    /// Err means infrastructure trouble (for example the model server is down): the
    /// errand stays `Shaping` and is retried in ten minutes.
    async fn complete(
        &self,
        system: &str,
        user: &str,
        cancel: CancellationToken,
    ) -> Result<String, String>;
}

/// A solver endpoint and the machine it runs on. `client` carries the pinned TLS and
/// bearer for a Mesh worker; `None` means a plain local URL. The client is never
/// persisted or printed.
#[derive(Clone)]
pub struct Endpoint {
    pub url: String,
    pub ran_on: RanOn,
    pub client: Option<reqwest::Client>,
}

impl PartialEq for Endpoint {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url && self.ran_on == other.ran_on
    }
}

impl Eq for Endpoint {}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("url", &self.url)
            .field("ran_on", &self.ran_on)
            .field(
                "client",
                &if self.client.is_some() {
                    "<set>"
                } else {
                    "<none>"
                },
            )
            .finish()
    }
}

/// Receives observed generation throughput from a completed model reply.
pub type CompletionTiming = std::sync::Arc<dyn Fn(f64) + Send + Sync>;

/// Chooses an endpoint once per job attempt.
pub trait EndpointPicker: Send + Sync {
    /// Pick the endpoint for one attempt. Must always return something.
    fn pick(&self, use_mesh: bool) -> Endpoint;

    /// The pooled endpoint, while the heavy model is running.
    fn heavy(&self) -> Option<Endpoint> {
        None
    }

    /// Observe completion timing for the current pooled server session.
    fn pooled_timing(&self) -> Option<CompletionTiming> {
        None
    }

    /// The host endpoint, used to retry an attempt whose Mesh endpoint failed in
    /// transport.
    fn host(&self) -> Endpoint;
}

/// A vendor CLI (Claude Code or Codex) that can return a patch as text. Text in, text out
/// (the CLI contract of #449); the patch is verified exactly like a local one.
#[async_trait]
pub trait ExternalSolver: Send + Sync {
    /// The external machine and model for the result card.
    fn ran_on(&self) -> RanOn;

    /// Return the raw reply. It must contain SEARCH/REPLACE blocks (`edit.rs` format).
    async fn solve(
        &self,
        system: &str,
        prompt: &str,
        cancel: CancellationToken,
    ) -> Result<String, String>;
}

/// Ships an approved result: push `nightshift/<errand id>` from a throwaway worktree on
/// `origin/main`, apply the stored patch and open a **draft** PR.
#[async_trait]
pub trait Shipper: Send + Sync {
    /// Returns the PR URL. Never merges and never touches the owner's checkout.
    async fn ship(&self, errand: &Errand, order: &WorkOrder, patch: &str)
    -> Result<String, String>;
}

/// A legal state change requested for one errand.
#[derive(Debug, Clone)]
pub enum Transition {
    /// Proposed or Unshapeable → Shaping.
    Proposed(WorkOrder),
    /// → Unshapeable with plain-words reasons.
    Unshapeable(Vec<String>),
    /// Proposed or Unshapeable → Shaping with an owner note.
    Reshape(OwnerNote),
    /// Proposed → Queued; an edited order is re-validated by the caller.
    Approve(Option<WorkOrder>),
    /// Queued → Working.
    Working {
        job_id: String,
        stage: String,
        ran_on: RanOn,
    },
    /// Working → Working with a new stage.
    Stage(String),
    /// Working → Ready / NeedsYou / Failed. Also accepted from Failed/NeedsYou so an
    /// external solve can replace tonight's failed local result.
    Finished {
        result: NightResult,
        outcome: MorningOutcome,
        question_or_reason: Option<String>,
    },
    /// Ready, NeedsYou or Failed → Queued.
    Redo(OwnerNote),
    /// Ready → Shipping.
    Shipping,
    /// Shipping → Shipped with the draft PR URL.
    Shipped(String),
    /// Shipping → Ready again; the failure is reported without a note.
    ShipFailed(String),
    /// Working → Queued for another try without spending the attempt: used for a host
    /// infrastructure failure or a voice pre-emption.
    Requeue { reason: String },
    /// Any state but Shipping or Shipped → Dropped.
    Drop,
    /// Record that an external CLI saw this errand.
    MarkExternalUsed,
}

/// The errand store: one mutex over `<state dir>/nightshift/errands.json`, the stored
/// patches, and a broadcast of every successful change.
#[derive(Debug)]
pub struct ErrandStore {
    root: std::path::PathBuf,
    inner: std::sync::Mutex<store::Inner>,
    events: tokio::sync::broadcast::Sender<Errand>,
    morning_events: tokio::sync::broadcast::Sender<MorningReport>,
}

#[cfg(test)]
mod pooled_contract_tests {
    use super::*;

    #[test]
    fn existing_settings_opt_into_heavy_without_automatic_host_fallback() {
        let settings: NightShiftSettings =
            serde_json::from_str(r#"{"enabled":true,"use_mesh":false}"#).unwrap();
        assert!(settings.use_heavy);
        assert!(!settings.fallback_to_host);
        assert!(!settings.use_mesh);
    }

    #[test]
    fn pooled_result_serializes_with_the_wire_label() {
        assert_eq!(
            serde_json::to_string(&RanOnKind::Pooled).unwrap(),
            "\"pooled\""
        );
    }
}
