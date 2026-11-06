//! Pure domain core. Nothing in this module performs IO, reads a clock or calls a model;
//! `tests/core_purity_test.rs` enforces that by scanning these sources.

pub mod artifacts;
pub mod events;
pub mod graph;
pub mod judgment;
pub mod messages;
pub mod plan;
pub mod ports;
pub mod practice;
pub mod question;
pub mod view;

/// A correctable domain error. Its text is returned to the model as a tool result, so it
/// names the field and the fix in one short sentence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

impl Invalid {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Bounds on model-supplied text, in bytes.
pub const MAX_TEXT: usize = 8_000;
pub const MAX_SHORT: usize = 300;
/// Propositions picked as session context are re-sent in every system prompt, so their
/// total size must fit [`view::PROMPT_RESERVE_TOKENS`] beside the plan and tool schemas.
pub const MAX_GRAPH_CONTEXT_BYTES: usize = 4_000;

pub(crate) fn bounded(field: &str, text: &str, max: usize) -> Result<(), Invalid> {
    if text.trim().is_empty() {
        return Err(Invalid::new(format!("`{field}` must not be empty")));
    }
    if text.len() > max {
        return Err(Invalid::new(format!(
            "`{field}` is too long ({} bytes, max {max})",
            text.len()
        )));
    }
    Ok(())
}
