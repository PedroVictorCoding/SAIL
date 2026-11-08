//! HTML/text normalisation, the deterministic PT/EN date finder, extraction prompts and
//! validation of model replies. **Owner: Gemini.** See *Gemini details* in the handoff.

pub mod dates;
pub mod docs;
pub mod merge;
pub mod pattern;
pub mod prompt;
pub mod structured;
pub mod text;
pub mod validate;

pub use dates::{DateMention, MAX_MENTIONS, find_dates};
pub use docs::{CHUNK_SIZE_BYTES, docs_from_file_text, docs_from_snapshot};
pub use merge::{item_id, merge, normalize_title};
pub use pattern::items_from_patterns;
pub use prompt::{build_prompt, should_ask_model};
pub use structured::items_from_snapshot;
pub use text::{MAX_TEXT_OUTPUT_BYTES, html_to_text};
pub use validate::{MAX_COMPONENTS_PER_REPLY, MAX_ITEMS_PER_REPLY, parse_reply};
