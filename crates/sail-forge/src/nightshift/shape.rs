//! K2: turn an ask into a validated work order.
//!
//! [`shaper_prompt`], [`parse_order`] and [`validate`] are pure; [`run_shaper`] performs
//! at most two model calls (the second only to repair a bad reply) and then moves the
//! errand from Shaping to Proposed or Unshapeable.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::gates::DiffGatePolicy;
use crate::repo_map::{rank, render_map, scan_repo};

use super::{ErrandId, ErrandState, ErrandStore, OwnerNote, ShaperModel, Size, WorkOrder};

/// The ranked repo map in the shaping prompt is capped at 4 000 characters (owner
/// decision: enough localization context without crowding out the ask).
const REPO_MAP_BUDGET: usize = 4_000;

/// A model reply longer than 64 000 characters is rejected without parsing (fixed a
/// priori to bound work on a hostile or runaway reply).
const MAX_MODEL_REPLY_CHARS: usize = 64_000;

/// At most 32 balanced-object candidates are tried per reply (fixed a priori to bound
/// hostile input); normal replies contain one.
const MAX_JSON_CANDIDATES: usize = 32;

/// Build the shaping prompt: a SAIL-authored system message and the user part made of the
/// ask, the owner notes, the ranked repo map and the crate list. Pure.
pub fn shaper_prompt(
    ask: &str,
    notes: &[OwnerNote],
    repo_map: &str,
    crates: &[String],
) -> (String, String) {
    let system = concat!(
        "You turn one owner request into a single small work order for SAIL Night Shift. ",
        "Reply with exactly one JSON object and no other text. The object has these keys: ",
        "\"title\" (string, 1-100 characters), \"why\" (string, at most 600), ",
        "\"acceptance\" (array of 1-8 strings, each 1-200), \"target_files\" ",
        "(array of 1-6 repo-relative paths that already exist and are not protected), ",
        "\"size\" (\"s\", \"m\" or \"l\"), and \"notes_for_solver\" (string, at most 1500). ",
        "Choose files only from the repository map. One night must finish the work: if it ",
        "cannot, use size \"l\" and say why. Do not invent paths."
    )
    .to_string();

    let mut user = String::new();
    user.push_str("Owner ask:\n");
    user.push_str(ask);
    user.push_str("\n\nOwner notes:\n");
    if notes.is_empty() {
        user.push_str("(none)\n");
    } else {
        for note in notes {
            user.push_str("- ");
            user.push_str(&note.at.to_rfc3339());
            user.push_str(": ");
            user.push_str(&note.text);
            user.push('\n');
        }
    }
    user.push_str("\nWorkspace crates: ");
    user.push_str(&crates.join(", "));
    user.push_str("\n\nRepository map (most relevant first):\n");
    user.push_str(repo_map);
    (system, user)
}

/// Parse one work order from a model reply. Fenced and prose-wrapped replies are
/// tolerated as long as they contain one valid object; Err is a plain-words reason list.
pub fn parse_order(reply: &str) -> Result<WorkOrder, Vec<String>> {
    if reply.chars().count() > MAX_MODEL_REPLY_CHARS {
        return Err(vec![format!(
            "the reply is longer than {MAX_MODEL_REPLY_CHARS} characters"
        )]);
    }
    let mut last_error = None;
    for candidate in top_level_objects(reply) {
        match serde_json::from_str::<WorkOrder>(&candidate) {
            Ok(order) => return Ok(order),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(vec![match last_error {
        Some(error) => format!("the reply is not a valid work order: {error}"),
        None => "the reply did not contain a JSON object".into(),
    }])
}

/// Every balanced top-level `{…}` candidate, in order of appearance. Braces inside
/// strings do not close a candidate.
fn top_level_objects(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (start, _) in text.char_indices().filter(|(_, c)| *c == '{') {
        if out.len() >= MAX_JSON_CANDIDATES {
            break;
        }
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, ch) in text[start..].char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        out.push(text[start..start + offset + ch.len_utf8()].to_string());
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Check every work-order limit and path rule. Empty means valid. Pure.
pub fn validate(order: &WorkOrder, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut reasons = Vec::new();
    let title_chars = order.title.chars().count();
    if order.title.trim().is_empty() || title_chars > 100 {
        reasons.push("the title must be 1-100 characters".to_string());
    }
    if order.why.chars().count() > 600 {
        reasons.push("the why must be at most 600 characters".to_string());
    }
    if order.acceptance.is_empty() || order.acceptance.len() > 8 {
        reasons.push("there must be 1-8 acceptance checks".to_string());
    }
    for (index, item) in order.acceptance.iter().enumerate() {
        let count = item.chars().count();
        if item.trim().is_empty() || count > 200 {
            reasons.push(format!(
                "acceptance check {} must be 1-200 characters",
                index + 1
            ));
        }
    }
    if order.target_files.is_empty() || order.target_files.len() > 6 {
        reasons.push("there must be 1-6 target files".to_string());
    }
    if order.notes_for_solver.chars().count() > 1_500 {
        reasons.push("the solver notes must be at most 1500 characters".to_string());
    }
    if order.size == Size::L {
        reasons.push("too big for one night; split it".to_string());
    }

    let protected = DiffGatePolicy::default().forbidden_path_prefixes;
    let mut seen = HashSet::new();
    for path in &order.target_files {
        if path.is_empty() {
            reasons.push("a target file path is empty".to_string());
            continue;
        }
        let mut shape_ok = true;
        if path.starts_with('/') {
            reasons.push(format!("`{path}` must be repo-relative, not absolute"));
            shape_ok = false;
        }
        if path.contains('\\') {
            reasons.push(format!("`{path}` must use '/' separators"));
            shape_ok = false;
        }
        if path.split('/').any(|part| part == "..") {
            reasons.push(format!("`{path}` must not contain `..`"));
            shape_ok = false;
        }
        if path.split('/').any(str::is_empty) {
            reasons.push(format!("`{path}` has an empty path segment"));
            shape_ok = false;
        }
        if let Some(prefix) = protected.iter().find(|p| path.starts_with(p.as_str())) {
            reasons.push(format!("`{path}` is under protected path `{prefix}`"));
            shape_ok = false;
        }
        if !seen.insert(path.clone()) {
            reasons.push(format!("`{path}` is listed more than once"));
        }
        if shape_ok && !exists(path) {
            reasons.push(format!("`{path}` does not exist at the base commit"));
        }
    }
    reasons
}

/// Shape one errand, with at most one repair turn. Infrastructure/model errors return Err
/// and leave the errand in Shaping; an unrepairable reply moves it to Unshapeable.
pub async fn run_shaper(
    store: &ErrandStore,
    model: &dyn ShaperModel,
    repo_root: &Path,
    id: &ErrandId,
    now: DateTime<Utc>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let errand = store.get(id).ok_or_else(|| "no such errand".to_string())?;
    if !matches!(errand.state, ErrandState::Shaping) {
        return Err("the errand is not in shaping".into());
    }
    if cancel.is_cancelled() {
        return Err("the shaping was cancelled".into());
    }

    let files = scan_repo(repo_root);
    let repo_map = render_map(&rank(&files, &errand.ask, &[]), REPO_MAP_BUDGET);
    let crates = workspace_crates(repo_root);
    let (system, user) = shaper_prompt(&errand.ask, &errand.notes, &repo_map, &crates);
    let exists = |path: &str| repo_root.join(path).is_file();

    let reply = model
        .complete(&system, &user, cancel.clone())
        .await
        .map_err(|error| format!("the shaper model failed: {error}"))?;
    if cancel.is_cancelled() {
        return Err("the shaping was cancelled".into());
    }
    let first = match parse_order(&reply) {
        Ok(order) => {
            let reasons = validate(&order, &exists);
            if reasons.is_empty() {
                Ok(order)
            } else {
                Err(reasons)
            }
        }
        Err(reasons) => Err(reasons),
    };
    let (order, reasons) = match first {
        Ok(order) => (Some(order), Vec::new()),
        Err(reasons) => {
            let repair_user = format!(
                "{user}\n\nYour previous reply was rejected:\n{}\n\nReply with one corrected JSON object only.",
                reasons
                    .iter()
                    .map(|reason| format!("- {reason}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let reply = model
                .complete(&system, &repair_user, cancel.clone())
                .await
                .map_err(|error| format!("the shaper model failed on repair: {error}"))?;
            if cancel.is_cancelled() {
                return Err("the shaping was cancelled".into());
            }
            match parse_order(&reply) {
                Ok(order) => {
                    let reasons = validate(&order, &exists);
                    if reasons.is_empty() {
                        (Some(order), Vec::new())
                    } else {
                        (None, reasons)
                    }
                }
                Err(reasons) => (None, reasons),
            }
        }
    };
    match order {
        Some(order) => {
            store.transition(id, now, super::Transition::Proposed(order))?;
            Ok(())
        }
        None => {
            store.transition(id, now, super::Transition::Unshapeable(reasons))?;
            Ok(())
        }
    }
}

/// Workspace crate directory names, sorted (crate list for the prompt).
fn workspace_crates(root: &Path) -> Vec<String> {
    let mut crates: Vec<String> = std::fs::read_dir(root.join("crates"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().join("Cargo.toml").is_file())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    crates.sort();
    crates
}
