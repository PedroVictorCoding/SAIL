//! K1: the errand store and its change broadcast.
//!
//! `<forge state dir>/nightshift/errands.json` is written atomically (tmp + rename, like
//! [`crate::nightly::NightlyState::save`]); each patch lives at
//! `<forge state dir>/nightshift/patches/<errand id>.patch`. One mutex guards the state,
//! and every successful change is broadcast after the lock is released.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    Errand, ErrandId, ErrandSource, ErrandState, ErrandStore, MorningOutcome, OwnerNote,
    Transition, WorkOrder,
};

/// The owner's ask is capped at 2 000 characters (invariant 7, owner decision).
const MAX_ASK_CHARS: usize = 2_000;

/// At most 20 errands are open at once: not `Shipped` and not `Dropped` (invariant 7).
const MAX_OPEN_ERRANDS: usize = 20;

/// A reshape or redo note is 1..=1 000 characters (owner decision).
const MAX_NOTE_CHARS: usize = 1_000;

/// Stored patches are capped at 512 KB; a larger patch is not reviewable (invariant 7,
/// owner decision). The error text is the contract's `"patch too large to review"`.
const MAX_PATCH_BYTES: u64 = 512 * 1024;

/// Broadcast buffer for change events. Fixed a priori: a slow UI only needs the latest
/// few changes, and a lagging receiver can re-read the store.
const EVENT_CHANNEL_CAPACITY: usize = 32;

/// The only state shapes written to disk. Keeping the wrapper (rather than a bare array)
/// leaves room for a future version field without breaking old files.
#[derive(Debug, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    errands: Vec<Errand>,
}

/// The in-memory state behind the store's single mutex.
#[derive(Debug, Default)]
pub(super) struct Inner {
    errands: BTreeMap<String, Errand>,
}

impl ErrandStore {
    /// Open the store. A missing file starts empty. A corrupt file is renamed to
    /// `errands.json.corrupt-<ts>`, logged once, and replaced by an empty store. Restart
    /// recovery turns `Working` into `Queued` (dropping the job id) and `Shipping` into
    /// `Ready`.
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        let root = state_dir.join("nightshift");
        std::fs::create_dir_all(root.join("patches"))
            .map_err(|e| format!("could not create the night shift directory: {e}"))?;
        let path = root.join("errands.json");
        let mut inner = Inner::default();
        let mut migrated = false;
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Persisted>(&text) {
                Ok(persisted) => {
                    for errand in persisted.errands {
                        if ErrandId::parse(errand.id.as_str()).is_none() {
                            tracing::warn!(
                                target: "forge::nightshift",
                                "ignoring a stored errand with an invalid id"
                            );
                            continue;
                        }
                        inner.errands.insert(errand.id.as_str().to_string(), errand);
                    }
                }
                Err(error) => {
                    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
                    let renamed = root.join(format!("errands.json.corrupt-{stamp}"));
                    if let Err(rename_error) = std::fs::rename(&path, &renamed) {
                        tracing::error!(
                            target: "forge::nightshift",
                            "errands.json is corrupt ({error}) and could not be renamed ({rename_error}); starting empty"
                        );
                    } else {
                        tracing::error!(
                            target: "forge::nightshift",
                            "errands.json is corrupt ({error}); moved to {} and starting empty",
                            renamed.display()
                        );
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("could not read the night shift errands: {error}"));
            }
        }

        for errand in inner.errands.values_mut() {
            match &errand.state {
                ErrandState::Working {
                    order, started_at, ..
                } => {
                    errand.state = ErrandState::Queued {
                        order: order.clone(),
                        approved_at: *started_at,
                    };
                    migrated = true;
                }
                ErrandState::Shipping { order, result } => {
                    errand.state = ErrandState::Ready {
                        order: order.clone(),
                        result: result.clone(),
                    };
                    migrated = true;
                }
                _ => {}
            }
        }
        if migrated {
            save_inner(&root, &inner)?;
        }

        let (events, _) = tokio::sync::broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Ok(Self {
            root,
            inner: Mutex::new(inner),
            events,
        })
    }

    /// Add an errand in `Shaping`. Rejects an empty or over-long ask and a 21st open
    /// errand.
    pub fn add(
        &self,
        ask: &str,
        source: ErrandSource,
        now: DateTime<Utc>,
        rand6: [u8; 6],
    ) -> Result<Errand, String> {
        if ask.trim().is_empty() || ask.chars().count() > MAX_ASK_CHARS {
            return Err(format!("the ask must be 1..={MAX_ASK_CHARS} characters"));
        }
        let id = ErrandId::new(rand6);
        let errand = Errand {
            id: id.clone(),
            ask: ask.to_string(),
            source,
            created_at: now,
            updated_at: now,
            attempts: 0,
            external_used: false,
            notes: Vec::new(),
            state: ErrandState::Shaping,
        };
        {
            let mut inner = self.lock()?;
            if inner.errands.contains_key(id.as_str()) {
                return Err("an errand with that id already exists".into());
            }
            if inner.errands.values().filter(|e| is_open(&e.state)).count() >= MAX_OPEN_ERRANDS {
                return Err(format!(
                    "at most {MAX_OPEN_ERRANDS} errands can be open at once"
                ));
            }
            inner
                .errands
                .insert(id.as_str().to_string(), errand.clone());
            if let Err(error) = save_inner(&self.root, &inner) {
                inner.errands.remove(id.as_str());
                return Err(error);
            }
        }
        self.publish(&errand);
        Ok(errand)
    }

    /// Look one errand up by id.
    pub fn get(&self, id: &ErrandId) -> Option<Errand> {
        self.lock().ok()?.errands.get(id.as_str()).cloned()
    }

    /// Every errand, newest first. Ties are broken by id, descending, so the order is
    /// deterministic.
    pub fn list(&self) -> Vec<Errand> {
        let Ok(inner) = self.lock() else {
            return Vec::new();
        };
        let mut errands: Vec<Errand> = inner.errands.values().cloned().collect();
        errands.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        errands
    }

    /// Apply a legal transition, persist it and broadcast the updated errand. The error
    /// text is `can't <verb> while <state>` for an illegal move.
    pub fn transition(
        &self,
        id: &ErrandId,
        now: DateTime<Utc>,
        f: Transition,
    ) -> Result<Errand, String> {
        let ship_failure = match &f {
            Transition::ShipFailed(reason) => Some(reason.clone()),
            _ => None,
        };
        let updated = {
            let mut inner = self.lock()?;
            let current = inner
                .errands
                .get(id.as_str())
                .cloned()
                .ok_or_else(|| "no such errand".to_string())?;
            let mut updated = current.clone();
            updated.state = next_state(&mut updated, &current.state, f, now)?;
            updated.updated_at = now;
            inner
                .errands
                .insert(id.as_str().to_string(), updated.clone());
            if let Err(error) = save_inner(&self.root, &inner) {
                inner.errands.insert(id.as_str().to_string(), current);
                return Err(error);
            }
            updated
        };
        self.publish(&updated);
        if let Some(reason) = ship_failure {
            return Err(reason);
        }
        Ok(updated)
    }

    /// Store a patch and return its byte length. Rejects anything over 512 KB.
    pub fn save_patch(&self, id: &ErrandId, patch: &str) -> Result<u64, String> {
        if patch.len() as u64 > MAX_PATCH_BYTES {
            return Err("patch too large to review".into());
        }
        if self.get(id).is_none() {
            return Err("no such errand".into());
        }
        let dir = self.root.join("patches");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create the patch directory: {e}"))?;
        let path = dir.join(format!("{}.patch", id.as_str()));
        let tmp = dir.join(format!(
            "{}.patch.tmp.{}",
            id.as_str(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, patch).map_err(|e| format!("could not write the patch: {e}"))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| format!("could not store the patch atomically: {e}"))?;
        Ok(patch.len() as u64)
    }

    /// Read a stored patch.
    pub fn patch(&self, id: &ErrandId) -> Option<String> {
        std::fs::read_to_string(
            self.root
                .join("patches")
                .join(format!("{}.patch", id.as_str())),
        )
        .ok()
    }

    /// Subscribe to every successful change.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Errand> {
        self.events.subscribe()
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>, String> {
        self.inner
            .lock()
            .map_err(|_| "the night shift store lock is poisoned".to_string())
    }

    fn publish(&self, errand: &Errand) {
        // A broadcast with no receivers is not an error for the store.
        let _ = self.events.send(errand.clone());
    }
}

/// An errand is open until the owner ships or drops it.
fn is_open(state: &ErrandState) -> bool {
    !matches!(
        state,
        ErrandState::Shipped { .. } | ErrandState::Dropped { .. }
    )
}

fn save_inner(root: &Path, inner: &Inner) -> Result<(), String> {
    let mut errands: Vec<Errand> = inner.errands.values().cloned().collect();
    errands.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let json = serde_json::to_string_pretty(&Persisted { errands })
        .map_err(|e| format!("could not serialize the night shift errands: {e}"))?;
    let path = root.join("errands.json");
    let tmp = root.join(format!("errands.json.tmp.{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, json)
        .map_err(|e| format!("could not write the night shift errands: {e}"))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("could not store the night shift errands atomically: {e}"))
}

fn state_label(state: &ErrandState) -> &'static str {
    match state {
        ErrandState::Shaping => "shaping",
        ErrandState::Proposed { .. } => "proposed",
        ErrandState::Unshapeable { .. } => "unshapeable",
        ErrandState::Queued { .. } => "queued",
        ErrandState::Working { .. } => "working",
        ErrandState::Ready { .. } => "ready",
        ErrandState::NeedsYou { .. } => "needs_you",
        ErrandState::Failed { .. } => "failed",
        ErrandState::Shipping { .. } => "shipping",
        ErrandState::Shipped { .. } => "shipped",
        ErrandState::Dropped { .. } => "dropped",
    }
}

fn illegal(verb: &str, state: &ErrandState) -> String {
    format!("can't {verb} while {}", state_label(state))
}

fn check_note(note: &OwnerNote) -> Result<(), String> {
    if note.text.trim().is_empty() || note.text.chars().count() > MAX_NOTE_CHARS {
        return Err(format!("the note must be 1..={MAX_NOTE_CHARS} characters"));
    }
    Ok(())
}

fn check_stage(stage: &str) -> Result<(), String> {
    if matches!(stage, "planning" | "coding" | "testing" | "reviewing") {
        Ok(())
    } else {
        Err(format!("unknown night shift stage `{stage}`"))
    }
}

fn next_state(
    errand: &mut Errand,
    current: &ErrandState,
    f: Transition,
    now: DateTime<Utc>,
) -> Result<ErrandState, String> {
    let f_verb_before = f_verb(&f).unwrap_or("change");
    match (f, current) {
        (Transition::Proposed(order), ErrandState::Shaping) => Ok(ErrandState::Proposed { order }),
        (Transition::Unshapeable(reasons), ErrandState::Shaping) => {
            let reasons: Vec<String> = reasons
                .into_iter()
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty())
                .collect();
            if reasons.is_empty() {
                return Err("can't mark unshapeable without reasons".into());
            }
            Ok(ErrandState::Unshapeable { reasons })
        }
        (
            Transition::Reshape(note),
            ErrandState::Proposed { .. } | ErrandState::Unshapeable { .. },
        ) => {
            check_note(&note)?;
            errand.notes.push(note);
            Ok(ErrandState::Shaping)
        }
        (
            Transition::Approve(order),
            ErrandState::Proposed {
                order: current_order,
            },
        ) => {
            let order: WorkOrder = order.unwrap_or_else(|| current_order.clone());
            Ok(ErrandState::Queued {
                order,
                approved_at: now,
            })
        }
        (
            Transition::Working {
                job_id,
                stage,
                ran_on,
            },
            ErrandState::Queued { order, .. },
        ) => {
            check_stage(&stage)?;
            errand.attempts = errand.attempts.saturating_add(1);
            Ok(ErrandState::Working {
                order: order.clone(),
                job_id,
                stage,
                started_at: now,
                ran_on,
            })
        }
        (
            Transition::Stage(stage),
            ErrandState::Working {
                order,
                job_id,
                started_at,
                ran_on,
                ..
            },
        ) => {
            check_stage(&stage)?;
            Ok(ErrandState::Working {
                order: order.clone(),
                job_id: job_id.clone(),
                stage,
                started_at: *started_at,
                ran_on: ran_on.clone(),
            })
        }
        (
            Transition::Finished {
                result,
                outcome,
                question_or_reason,
            },
            ErrandState::Working { order, .. }
            | ErrandState::Failed { order, .. }
            | ErrandState::NeedsYou { order, .. },
        ) => match outcome {
            MorningOutcome::Ready => Ok(ErrandState::Ready {
                order: order.clone(),
                result,
            }),
            MorningOutcome::NeedsYou => Ok(ErrandState::NeedsYou {
                order: order.clone(),
                result,
                question: question_or_reason
                    .filter(|q| !q.trim().is_empty())
                    .unwrap_or_else(|| "needs your review".into()),
            }),
            MorningOutcome::Failed => Ok(ErrandState::Failed {
                order: order.clone(),
                result: Some(result),
                reason: question_or_reason
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or_else(|| "the night run failed".into()),
            }),
            MorningOutcome::NotReached => Err(illegal("finish", current)),
        },
        (
            Transition::Redo(note),
            ErrandState::Ready { order, .. }
            | ErrandState::NeedsYou { order, .. }
            | ErrandState::Failed { order, .. },
        ) => {
            check_note(&note)?;
            errand.notes.push(note);
            Ok(ErrandState::Queued {
                order: order.clone(),
                approved_at: now,
            })
        }
        (Transition::Shipping, ErrandState::Ready { order, result }) => Ok(ErrandState::Shipping {
            order: order.clone(),
            result: result.clone(),
        }),
        (Transition::Shipped(url), ErrandState::Shipping { order, result }) => {
            Ok(ErrandState::Shipped {
                order: order.clone(),
                result: result.clone(),
                pr_url: url,
                shipped_at: now,
            })
        }
        (Transition::ShipFailed(_), ErrandState::Shipping { order, result }) => {
            Ok(ErrandState::Ready {
                order: order.clone(),
                result: result.clone(),
            })
        }
        (Transition::Drop, ErrandState::Shipping { .. } | ErrandState::Shipped { .. }) => {
            Err(illegal("drop", current))
        }
        (Transition::Drop, _) => Ok(ErrandState::Dropped { at: now }),
        (
            Transition::MarkExternalUsed,
            ErrandState::Shipped { .. } | ErrandState::Dropped { .. },
        ) => Err(illegal("mark external", current)),
        (Transition::MarkExternalUsed, _) => {
            errand.external_used = true;
            Ok(current.clone())
        }
        (_, _) => Err(illegal(f_verb_before, current)),
    }
}

fn f_verb(f: &Transition) -> Option<&'static str> {
    match f {
        Transition::Proposed(_) => Some("propose"),
        Transition::Unshapeable(_) => Some("mark unshapeable"),
        Transition::Reshape(_) => Some("reshape"),
        Transition::Approve(_) => Some("approve"),
        Transition::Working { .. } => Some("start working"),
        Transition::Stage(_) => Some("change stage"),
        Transition::Finished { .. } => Some("finish"),
        Transition::Redo(_) => Some("redo"),
        Transition::Shipping => Some("ship"),
        Transition::Shipped(_) => Some("mark shipped"),
        Transition::ShipFailed(_) => Some("fail shipping"),
        Transition::Drop => Some("drop"),
        Transition::MarkExternalUsed => Some("mark external"),
    }
}
