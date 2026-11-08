//! Parses model replies into the contract types.
//!
//! The model is a data source with a mouth: replies may be fenced, wrapped in prose, or
//! malformed. This module finds the first balanced JSON object, refuses anything implausibly
//! large, parses with `deny_unknown_fields` contract types, and computes the SHA-256 that
//! validation receipts bind to. Every failure is a typed error carrying the serde message;
//! nothing here panics on hostile text.

use crate::courses::content::Step;
use crate::factory::contracts::{
    ConceptProposal, ContentHash, CurriculumProposal, LessonDraftOutput, sha256_hex,
};

/// Largest reply accepted, in bytes. A lesson draft with fixtures is a few tens of KiB; this
/// leaves generous room while keeping a hostile reply from exhausting memory.
pub const MAX_REPLY_BYTES: usize = 512 * 1024;

/// Why a reply could not be turned into a contract value.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The reply exceeded [`MAX_REPLY_BYTES`].
    #[error("reply is {size} bytes, over the {max} byte limit")]
    TooLarge { size: usize, max: usize },
    /// There was no `{` at all.
    #[error("reply contains no JSON object")]
    NoObject,
    /// A string or the object never closed.
    #[error("reply contains an unterminated JSON object")]
    Unbalanced,
    /// The object was well formed JSON but not the expected shape.
    #[error("reply does not match the schema: {0}")]
    Schema(String),
}

/// One parsed reply plus the exact text and hash it was parsed from.
///
/// Receipts bind to `output_hash`; storing `raw_json` means the draft can be re-parsed and
/// re-validated later without asking the model again.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed<T> {
    pub value: T,
    pub raw_json: String,
    pub output_hash: ContentHash,
}

/// Extracts the first balanced JSON object from a reply, ignoring fences and prose around it.
///
/// Braces inside JSON strings are ignored, and escapes are handled. The returned slice is the
/// object exactly as the model wrote it, which is what the hash covers.
pub fn extract_json_object(reply: &str) -> Result<&str, ParseError> {
    if reply.len() > MAX_REPLY_BYTES {
        return Err(ParseError::TooLarge {
            size: reply.len(),
            max: MAX_REPLY_BYTES,
        });
    }
    let bytes = reply.as_bytes();
    let Some(start) = bytes.iter().position(|byte| *byte == b'{') else {
        return Err(ParseError::NoObject);
    };
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + 1;
                    return Ok(&reply[start..end]);
                }
            }
            _ => {}
        }
    }
    Err(ParseError::Unbalanced)
}

/// Parses a concept-extraction reply.
pub fn parse_concept_proposal(reply: &str) -> Result<Parsed<ConceptProposal>, ParseError> {
    parse(reply)
}

/// Parses a curriculum-planning reply.
pub fn parse_curriculum_proposal(reply: &str) -> Result<Parsed<CurriculumProposal>, ParseError> {
    parse(reply)
}

/// Parses a lesson-authoring reply.
pub fn parse_lesson_draft(reply: &str) -> Result<Parsed<LessonDraftOutput>, ParseError> {
    parse(reply)
}

/// Parses a single review-variant step.
pub fn parse_review_variant(reply: &str) -> Result<Parsed<Step>, ParseError> {
    parse(reply)
}

/// The `output_hash` of a stored reply: SHA-256 of the exact `raw_json` text.
///
/// Hashing goes through [`crate::factory::contracts::sha256_hex`], the one hash function every
/// factory record uses.
pub fn output_hash(raw_json: &str) -> ContentHash {
    sha256_hex(raw_json.as_bytes())
}

fn parse<T>(reply: &str) -> Result<Parsed<T>, ParseError>
where
    T: serde::de::DeserializeOwned,
{
    let raw_json = extract_json_object(reply)?.to_owned();
    let value =
        serde_json::from_str(&raw_json).map_err(|error| ParseError::Schema(error.to_string()))?;
    let output_hash = output_hash(&raw_json);
    Ok(Parsed {
        value,
        raw_json,
        output_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::contracts::sha256_hex;

    const LESSON: &str = r#"{"lesson":{"id":"lesson.1","title":"Moves","minutes":5,"objective_ids":["o"],"steps":[]},"claims":[]}"#;

    #[test]
    fn fenced_json_with_prose_around_it_parses() {
        let reply = format!(
            "Sure, here is the draft:\n\n```json\n{LESSON}\n```\n\nLet me know if you want changes."
        );
        let parsed = parse_lesson_draft(&reply).expect("valid lesson");
        assert_eq!(parsed.value.lesson.id, "lesson.1");
        assert_eq!(parsed.raw_json, LESSON);
        assert_eq!(parsed.output_hash, output_hash(LESSON));
    }

    #[test]
    fn braces_inside_strings_do_not_end_the_object() {
        let reply = r#"prefix {"lesson":{"id":"l","title":"A {brace} in a title","minutes":5,"objective_ids":[],"steps":[]},"claims":[]} suffix"#;
        let parsed = parse_lesson_draft(reply).expect("valid lesson");
        assert_eq!(parsed.value.lesson.title, "A {brace} in a title");
    }

    #[test]
    fn unknown_fields_are_rejected_with_the_serde_message() {
        let reply = r#"{"lesson":{"id":"l","title":"t","minutes":5,"objective_ids":[],"steps":[]},"claims":[],"published":true}"#;
        let error = parse_lesson_draft(reply).expect_err("unknown field rejected");
        assert!(matches!(error, ParseError::Schema(_)));
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn a_truncated_reply_is_a_typed_error() {
        let reply = r#"{"lesson":{"id":"l","title":"t","minutes":5,"#;
        assert!(matches!(
            parse_lesson_draft(reply),
            Err(ParseError::Unbalanced)
        ));
    }

    #[test]
    fn a_reply_without_json_is_a_typed_error() {
        assert!(matches!(
            parse_lesson_draft("no object here"),
            Err(ParseError::NoObject)
        ));
        assert!(matches!(extract_json_object(""), Err(ParseError::NoObject)));
    }

    #[test]
    fn an_oversized_reply_is_refused_before_scanning() {
        let reply = "x".repeat(MAX_REPLY_BYTES + 1);
        assert!(matches!(
            extract_json_object(&reply),
            Err(ParseError::TooLarge { .. })
        ));
    }

    #[test]
    fn hash_matches_the_published_sha256_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_concept_reply_parses_into_the_contract_shape() {
        let reply = r#"{"concepts":[{"id":"rust.ownership","name":"Ownership","description":"A value has one owner.","objectives":["The learner can explain moves."],"prerequisites":{},"fragment_ids":["f-1"],"difficulty":2,"validator":"exact_key"}]}"#;
        let parsed = parse_concept_proposal(reply).expect("valid concepts");
        assert_eq!(parsed.value.concepts.len(), 1);
        assert_eq!(parsed.value.concepts[0].id, "rust.ownership");
    }

    #[test]
    fn a_curriculum_reply_parses_into_the_contract_shape() {
        let reply = r#"{"title":"Rust","level":"intro","scope":"ownership","outcomes":["explain moves"],"units":[]}"#;
        let parsed = parse_curriculum_proposal(reply).expect("valid curriculum");
        assert_eq!(parsed.value.title, "Rust");
    }

    #[test]
    fn a_single_review_step_parses() {
        let reply = r#"{"id":"r.1","kind":"choice","family":"new.family","title":"Which binding is valid?","objective_ids":["o"],"options":[{"id":"a","text":"x"},{"id":"b","text":"y"}],"key":{"choice":["a"]}}"#;
        let parsed = parse_review_variant(reply).expect("valid step");
        assert_eq!(parsed.value.id, "r.1");
        assert!(parsed.output_hash.len() == 64);
    }
}
