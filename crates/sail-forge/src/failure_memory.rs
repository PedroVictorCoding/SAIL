//! Engineering failure memory (#121): what Forge tried, what broke, and what worked.
//!
//! Each finished Forge job appends one [`AttemptRecord`]: the issue, base commit, model,
//! files touched, outcome and a list of normalized *signatures* (rustc error codes, failed
//! test names, gate rules, missed mutants). Before the next solve, [`FailureMemory::brief`]
//! gives the model a short, bounded section:
//! - earlier attempts on the same issue and why they failed;
//! - the error classes that keep recurring in the files the issue targets;
//! - verified fixes that touched those files.
//!
//! Records are facts observed by Forge (exit codes, gate results, test names), not model
//! opinions. A record can be invalidated (for example, a "verified" fix that was later
//! reverted) and is then never shown as a success. Records hold no source code, issue text
//! or model output beyond short failure lines.
//!
//! Storage: JSON lines in `$SAIL_FORGE_STATE_DIR/failure_memory.jsonl`, appended under an
//! exclusive lock. `sail_forge_memory` lists, exports, invalidates and deletes records.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Canonical `owner/repo#N`, lowercase, so spellings of one issue share their history.
pub fn issue_key(raw: &str) -> String {
    crate::reviewer::ReviewBroker::normalize_canonical_issue_key(raw).to_ascii_lowercase()
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// Tests, gates and review passed.
    Verified,
    /// The attempt failed; `class` is the first failing stage.
    Failed { class: FailureClass },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// The patch did not compile.
    Compile,
    /// It compiled, but tests failed.
    Tests,
    /// A deterministic diff gate rejected it.
    Gate,
    /// The mutation gate found untested code.
    Mutation,
    /// Self-review or external review rejected it.
    Review,
    /// The model's edits did not apply, or it produced no meaningful diff.
    NoPatch,
    /// Model unreachable, timeout, sandbox error and other infrastructure failures.
    Infrastructure,
    Other,
}

impl FailureClass {
    pub fn label(self) -> &'static str {
        match self {
            FailureClass::Compile => "compile error",
            FailureClass::Tests => "failing tests",
            FailureClass::Gate => "diff gate",
            FailureClass::Mutation => "untested code (mutation gate)",
            FailureClass::Review => "review rejected",
            FailureClass::NoPatch => "no usable patch",
            FailureClass::Infrastructure => "infrastructure",
            FailureClass::Other => "other",
        }
    }
}

/// One finished Forge attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub id: String,
    pub recorded_at: DateTime<Utc>,
    /// Canonical `owner/repo#N`.
    pub issue: String,
    pub base_commit: String,
    pub job_id: String,
    #[serde(default)]
    pub model: Option<String>,
    pub outcome: AttemptOutcome,
    /// Normalized, comparable failure signatures: `rustc:E0308`, `test:gates::tests::x`,
    /// `gate:new_unwrap`, `mutation:missed`, `review:rejected`.
    #[serde(default)]
    pub signatures: Vec<String>,
    /// First relevant failure line, at most 300 characters.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub turns: u32,
    /// Set when the user (or later evidence) says this record must not be trusted.
    #[serde(default)]
    pub invalidated: Option<String>,
}

impl AttemptRecord {
    pub fn is_verified(&self) -> bool {
        self.outcome == AttemptOutcome::Verified && self.invalidated.is_none()
    }

    pub fn class(&self) -> Option<FailureClass> {
        match &self.outcome {
            AttemptOutcome::Failed { class } => Some(*class),
            AttemptOutcome::Verified => None,
        }
    }
}

/// Classify a failure from Forge's recorded reason and status.
pub fn classify(verification_status: Option<&str>, reason: &str) -> FailureClass {
    let r = reason.to_ascii_lowercase();
    let status = verification_status.unwrap_or("").to_ascii_lowercase();
    if status == "mutation_gate_failed" || r.contains("mutation gate") {
        FailureClass::Mutation
    } else if status == "gate_failed" || r.contains("pre-review gates failed") {
        FailureClass::Gate
    } else if r.contains("review") && (r.contains("reject") || r.contains("changes requested")) {
        FailureClass::Review
    } else if r.contains("error[e") || r.contains("could not compile") {
        FailureClass::Compile
    } else if r.contains("test result: failed")
        || r.contains("panicked at")
        || r.contains("failed (exit")
        || r.contains("tests_failed")
        || status == "tests_failed"
    {
        FailureClass::Tests
    } else if r.contains("no meaningful")
        || r.contains("empty diff")
        || r.contains("edit failed")
        || r.contains("no patch")
    {
        FailureClass::NoPatch
    } else if r.contains("timed out")
        || r.contains("timeout")
        || r.contains("unreachable")
        || r.contains("connection")
        || r.contains("sandbox")
        || r.contains("model server")
    {
        FailureClass::Infrastructure
    } else {
        FailureClass::Other
    }
}

/// Extract comparable signatures from failure text.
pub fn signatures(class: Option<FailureClass>, text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) && out.len() < 12 {
            out.push(s);
        }
    };
    // rustc error codes: error[E0308]
    let mut rest = text;
    while let Some(i) = rest.find("error[E") {
        let code: String = rest[i + 6..].chars().take(5).collect();
        if code.len() == 5 && code[1..].chars().all(|c| c.is_ascii_digit()) {
            push(format!("rustc:{code}"));
        }
        rest = &rest[i + 7..];
    }
    for line in text.lines() {
        let t = line.trim();
        // nextest summary line from test_report: "FAILED suite :: name"
        if let Some(name) = t
            .strip_prefix("FAILED ")
            .and_then(|r| r.split(" :: ").nth(1))
        {
            push(format!("test:{}", name.trim()));
        }
        // libtest: "test path::name ... FAILED"
        if let Some(name) = t
            .strip_prefix("test ")
            .and_then(|r| r.strip_suffix(" ... FAILED"))
        {
            push(format!("test:{}", name.trim()));
        }
        // gate violations: "[new_unwrap] src/lib.rs ..."
        if let Some(rule) = t.split('[').nth(1).and_then(|r| r.split(']').next()) {
            if !rule.is_empty()
                && rule.len() <= 32
                && rule.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && class == Some(FailureClass::Gate)
            {
                push(format!("gate:{rule}"));
            }
        }
    }
    match class {
        Some(FailureClass::Mutation) => push("mutation:missed".into()),
        Some(FailureClass::Review) => push("review:rejected".into()),
        _ => {}
    }
    out
}

/// First line that says what went wrong, trimmed to 300 characters.
pub fn summary_line(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| {
            l.starts_with("error")
                || l.contains("panicked at")
                || l.starts_with("FAILED ")
                || l.contains("gates failed")
                || l.contains("Mutation gate")
        })
        .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))?;
    let mut s: String = line.chars().take(300).collect();
    if line.chars().count() > 300 {
        s.push('…');
    }
    Some(s)
}

/// The JSONL store.
#[derive(Debug, Clone)]
pub struct FailureMemory {
    path: PathBuf,
}

/// Maximum size of the brief given to the model.
pub const BRIEF_CHARS: usize = 1_500;

impl FailureMemory {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// `failure_memory.jsonl` in Forge's state directory (`SAIL_FORGE_STATE_DIR`, else the
    /// nightly runner's default), beside the digests and nightly state.
    pub fn open_default() -> Self {
        Self::new(crate::nightly::default_state_dir().join("failure_memory.jsonl"))
    }

    /// The default store, unless `SAIL_FORGE_FAILURE_MEMORY` is `0`/`false`/`off`.
    pub fn from_env() -> Option<Self> {
        match std::env::var("SAIL_FORGE_FAILURE_MEMORY").ok().as_deref() {
            Some("0") | Some("false") | Some("off") => None,
            _ => Some(Self::open_default()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::io::Result<fs::File> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.path.with_extension("lock"))?;
        // SAFETY: flock on an owned descriptor.
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(lock)
    }

    /// Append one record.
    pub fn append(&self, record: &AttemptRecord) -> std::io::Result<()> {
        let _lock = self.lock()?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)?;
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        f.write_all(line.as_bytes())?;
        f.sync_data()
    }

    /// All records, oldest first. Lines that do not parse are skipped.
    pub fn load(&self) -> std::io::Result<Vec<AttemptRecord>> {
        let f = match fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect())
    }

    /// Rewrite the store with `f` applied to the record list. Atomic (temp file + rename).
    fn rewrite(&self, f: impl FnOnce(&mut Vec<AttemptRecord>) -> usize) -> std::io::Result<usize> {
        let _lock = self.lock()?;
        let mut records = self.load()?;
        let changed = f(&mut records);
        if changed == 0 {
            return Ok(0);
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)?;
            for r in &records {
                let mut line = serde_json::to_string(r).map_err(std::io::Error::other)?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
            }
            out.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(changed)
    }

    /// Mark records as untrustworthy. A verified record that is invalidated never appears as a
    /// success again. Returns how many records changed.
    pub fn invalidate(&self, id_or_issue: &str, reason: &str) -> std::io::Result<usize> {
        let key = issue_key(id_or_issue);
        self.rewrite(|records| {
            let mut n = 0;
            for r in records.iter_mut() {
                if (r.id == id_or_issue || r.issue == key) && r.invalidated.is_none() {
                    r.invalidated = Some(reason.to_string());
                    n += 1;
                }
            }
            n
        })
    }

    /// Delete records by id, or all records of an issue. Returns how many were removed.
    pub fn delete(&self, id_or_issue: &str) -> std::io::Result<usize> {
        let key = issue_key(id_or_issue);
        self.rewrite(|records| {
            let before = records.len();
            records.retain(|r| r.id != id_or_issue && r.issue != key);
            before - records.len()
        })
    }

    /// The bounded section shown to the model before it starts on `issue`.
    pub fn brief(&self, issue: &str, target_files: &[String]) -> Option<String> {
        let records = self.load().ok()?;
        render_brief(&records, issue, target_files)
    }
}

/// Build the model-facing brief from `records`. `None` when there is nothing relevant.
pub fn render_brief(
    records: &[AttemptRecord],
    issue: &str,
    target_files: &[String],
) -> Option<String> {
    let key = issue_key(issue);
    let targets: HashSet<&str> = target_files.iter().map(String::as_str).collect();
    let touches = |r: &AttemptRecord| {
        r.files.iter().any(|f| {
            targets.contains(f.as_str())
                || targets
                    .iter()
                    .any(|t| t.ends_with('/') && f.starts_with(*t))
        })
    };

    let mut out = String::new();

    let same: Vec<&AttemptRecord> = records
        .iter()
        .rev()
        .filter(|r| r.issue == key && r.invalidated.is_none() && !r.is_verified())
        .take(3)
        .collect();
    if !same.is_empty() {
        out.push_str("Earlier attempts on this issue failed:\n");
        for r in same {
            out.push_str(&format!(
                "- {} ({}): {}\n",
                r.class().map(FailureClass::label).unwrap_or("failed"),
                r.recorded_at.format("%Y-%m-%d"),
                r.summary.as_deref().unwrap_or("no detail recorded")
            ));
        }
    }

    if !targets.is_empty() {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for r in records
            .iter()
            .filter(|r| r.invalidated.is_none() && !r.is_verified() && touches(r))
        {
            for s in &r.signatures {
                *counts.entry(s.as_str()).or_default() += 1;
            }
        }
        let mut recurring: Vec<(&str, usize)> =
            counts.into_iter().filter(|(_, n)| *n >= 2).collect();
        recurring.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        if !recurring.is_empty() {
            out.push_str("Errors that keep recurring in these files:\n");
            for (sig, n) in recurring.into_iter().take(5) {
                out.push_str(&format!("- {sig} ({n} attempts)\n"));
            }
        }

        let fixes: Vec<&AttemptRecord> = records
            .iter()
            .rev()
            .filter(|r| r.is_verified() && r.issue != key && touches(r))
            .take(3)
            .collect();
        if !fixes.is_empty() {
            out.push_str("Verified fixes that changed these files:\n");
            for r in fixes {
                out.push_str(&format!("- {} changed {}\n", r.issue, r.files.join(", ")));
            }
        }
    }

    if out.is_empty() {
        return None;
    }
    let mut brief = format!("## Forge failure memory\n{out}");
    if brief.chars().count() > BRIEF_CHARS {
        brief = brief.chars().take(BRIEF_CHARS).collect();
        brief.push_str("\n…");
    }
    Some(brief)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(issue: &str, outcome: AttemptOutcome, sigs: &[&str], files: &[&str]) -> AttemptRecord {
        AttemptRecord {
            id: uuid::Uuid::new_v4().to_string(),
            recorded_at: Utc::now(),
            issue: issue.into(),
            base_commit: "abc".into(),
            job_id: "j".into(),
            model: Some("m".into()),
            outcome,
            signatures: sigs.iter().map(|s| s.to_string()).collect(),
            summary: Some("error[E0308]: mismatched types".into()),
            files: files.iter().map(|s| s.to_string()).collect(),
            turns: 3,
            invalidated: None,
        }
    }

    fn failed(class: FailureClass) -> AttemptOutcome {
        AttemptOutcome::Failed { class }
    }

    #[test]
    fn classifies_failure_stages() {
        assert_eq!(classify(Some("gate_failed"), "x"), FailureClass::Gate);
        assert_eq!(
            classify(Some("mutation_gate_failed"), "x"),
            FailureClass::Mutation
        );
        assert_eq!(
            classify(None, "Solver failed verification: error[E0599]: no method"),
            FailureClass::Compile
        );
        assert_eq!(
            classify(
                Some("tests_failed"),
                "test result: FAILED. 1 passed; 1 failed"
            ),
            FailureClass::Tests
        );
        assert_eq!(
            classify(None, "Model server unreachable"),
            FailureClass::Infrastructure
        );
        assert_eq!(
            classify(None, "self-review rejected the patch"),
            FailureClass::Review
        );
        assert_eq!(classify(None, "something odd"), FailureClass::Other);
    }

    #[test]
    fn extracts_signatures() {
        let text = "error[E0308]: mismatched types\nerror[E0308]: again\nerror[E0599]: no method\n\
                    FAILED demo :: tests::doubles\ntest gates::tests::x ... FAILED\n";
        assert_eq!(
            signatures(Some(FailureClass::Compile), text),
            vec![
                "rustc:E0308",
                "rustc:E0599",
                "test:tests::doubles",
                "test:gates::tests::x"
            ]
        );
        let gate = "Pre-review gates failed: [new_unwrap] src/lib.rs: adds `.unwrap()`";
        assert_eq!(
            signatures(Some(FailureClass::Gate), gate),
            vec!["gate:new_unwrap"]
        );
        assert!(signatures(Some(FailureClass::Tests), gate).is_empty());
        assert_eq!(
            signatures(Some(FailureClass::Mutation), ""),
            vec!["mutation:missed"]
        );
    }

    #[test]
    fn brief_reports_same_issue_recurring_errors_and_fixes() {
        let f = "crates/a/src/lib.rs";
        let records = vec![
            rec(
                "o/r#1",
                failed(FailureClass::Compile),
                &["rustc:E0308"],
                &[f],
            ),
            rec(
                "o/r#2",
                failed(FailureClass::Compile),
                &["rustc:E0308"],
                &[f],
            ),
            rec("o/r#3", AttemptOutcome::Verified, &[], &[f]),
            rec(
                "o/r#4",
                failed(FailureClass::Tests),
                &["test:x"],
                &["other.rs"],
            ),
        ];
        let brief = render_brief(&records, "O/R#1", &[f.to_string()]).unwrap();
        assert!(brief.starts_with("## Forge failure memory"));
        assert!(
            brief.contains("Earlier attempts on this issue failed:\n- compile error"),
            "{brief}"
        );
        assert!(brief.contains("- rustc:E0308 (2 attempts)"), "{brief}");
        assert!(
            brief.contains("- o/r#3 changed crates/a/src/lib.rs"),
            "{brief}"
        );
        assert!(
            !brief.contains("test:x"),
            "unrelated files stay out: {brief}"
        );

        // A directory target matches files under it.
        assert!(render_brief(&records, "o/r#9", &["crates/a/".into()]).is_some());
        // Nothing relevant: no brief at all.
        assert!(render_brief(&records, "o/r#9", &["zzz.rs".into()]).is_none());
    }

    #[test]
    fn invalidated_records_are_never_shown() {
        let f = "src/lib.rs";
        let mut fix = rec("o/r#3", AttemptOutcome::Verified, &[], &[f]);
        fix.invalidated = Some("reverted in #40".into());
        let mut fail = rec(
            "o/r#1",
            failed(FailureClass::Compile),
            &["rustc:E0308"],
            &[f],
        );
        fail.invalidated = Some("flaky runner".into());
        assert!(!fix.is_verified());
        assert!(render_brief(&[fix, fail], "o/r#1", &[f.into()]).is_none());
    }

    #[test]
    fn store_appends_invalidates_and_deletes() {
        let dir = std::env::temp_dir().join(format!("forge-mem-{}", uuid::Uuid::new_v4()));
        let mem = FailureMemory::new(dir.join("failure_memory.jsonl"));
        let a = rec(
            "o/r#1",
            failed(FailureClass::Tests),
            &["test:x"],
            &["src/lib.rs"],
        );
        let b = rec("o/r#2", AttemptOutcome::Verified, &[], &["src/lib.rs"]);
        mem.append(&a).unwrap();
        mem.append(&b).unwrap();
        // A corrupt line does not lose the others.
        fs::OpenOptions::new()
            .append(true)
            .open(mem.path())
            .unwrap()
            .write_all(b"{not json\n")
            .unwrap();
        assert_eq!(mem.load().unwrap(), vec![a.clone(), b.clone()]);

        assert_eq!(mem.invalidate(&b.id, "reverted").unwrap(), 1);
        assert!(!mem.load().unwrap()[1].is_verified());
        assert_eq!(mem.invalidate(&b.id, "again").unwrap(), 0);

        assert_eq!(mem.delete("O/R#1").unwrap(), 1);
        let left = mem.load().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, b.id);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn summary_prefers_the_error_line() {
        let t = "Compiling demo\nerror[E0308]: mismatched types\n  --> src/lib.rs:3:5";
        assert_eq!(summary_line(t).unwrap(), "error[E0308]: mismatched types");
        assert_eq!(summary_line("").as_deref(), None);
        assert_eq!(summary_line(&"x".repeat(400)).unwrap().chars().count(), 301);
    }
}
