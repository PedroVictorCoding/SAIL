//! K6: solve an errand through a vendor CLI when the local attempt failed.
//!
//! Stub in K0.

use tokio_util::sync::CancellationToken;

use crate::orchestrator::ForgeOrchestrator;

use super::{Errand, ErrandStore, ExternalSolver};

/// Try one external solve, then verify the patch like a local one. Stub until K6.
pub async fn external_attempt(
    orchestrator: &ForgeOrchestrator,
    store: &ErrandStore,
    solver: &dyn ExternalSolver,
    errand: &Errand,
    cancel: CancellationToken,
) -> Result<(), String> {
    let _ = (orchestrator, store, solver, errand, cancel);
    Err("the external solver path is not implemented yet".into())
}
