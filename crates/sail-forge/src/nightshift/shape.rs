//! K2: turn an ask into a validated work order.
//!
//! Stub in K0. Everything except [`run_shaper`] is pure.

use std::path::Path;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use super::{ErrandId, ErrandStore, OwnerNote, ShaperModel, WorkOrder};

/// Build the shaping prompt. Stub until K2.
pub fn shaper_prompt(
    ask: &str,
    notes: &[OwnerNote],
    repo_map: &str,
    crates: &[String],
) -> (String, String) {
    let _ = (ask, notes, repo_map, crates);
    (String::new(), String::new())
}

/// Parse one work order out of a model reply. Stub until K2.
pub fn parse_order(reply: &str) -> Result<WorkOrder, Vec<String>> {
    let _ = reply;
    Err(vec!["the work-order parser is not implemented yet".into()])
}

/// Check every work-order limit and target-path rule. Stub until K2.
pub fn validate(order: &WorkOrder, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let _ = (order, exists);
    Vec::new()
}

/// Shape one errand, with one repair turn. Stub until K2.
pub async fn run_shaper(
    store: &ErrandStore,
    model: &dyn ShaperModel,
    repo_root: &Path,
    id: &ErrandId,
    now: DateTime<Utc>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let _ = (store, model, repo_root, id, now, cancel);
    Err("the shaper is not implemented yet".into())
}
