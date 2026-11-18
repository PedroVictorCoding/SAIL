//! K4: build a [`NightResult`] from a Forge job report and summarize it.
//!
//! Stub in K0.

use chrono::{DateTime, Utc};

use crate::contracts::ForgeJobReport;

use super::{MorningOutcome, NightResult, RanOn};

/// Extract the reviewable result from a Forge report and checkpoint. Stub until K4.
pub fn result_from_report(
    report: &ForgeJobReport,
    ran_on: RanOn,
    now: DateTime<Utc>,
) -> (NightResult, MorningOutcome, Option<String>, String) {
    let result = NightResult {
        job_id: report.issue.clone(),
        base_commit: report.base_commit.clone(),
        ran_on,
        files: Vec::new(),
        tests: None,
        gates: Vec::new(),
        review: None,
        summary: String::new(),
        model_note: None,
        patch_bytes: 0,
        finished_at: now,
    };
    (
        result,
        MorningOutcome::Failed,
        Some("night shift result extraction is not implemented yet".into()),
        String::new(),
    )
}

/// Deterministic result summary, ≤ 300 chars. Stub until K4.
pub fn summary(r: &NightResult) -> String {
    let _ = r;
    String::new()
}
