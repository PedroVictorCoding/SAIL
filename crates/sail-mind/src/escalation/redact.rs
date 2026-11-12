//! Fail-closed redaction before egress (Kilo). Spec: the module doc in `escalation/mod.rs`.

/// Placeholder → original value, for [`rehydrate`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionMap {
    pub entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub map: RedactionMap,
    /// Counts per placeholder class, e.g. ("EMAIL", 2), for the egress receipt.
    pub counts: Vec<(String, u32)>,
}

/// Replace private values with stable placeholders, then strip credential patterns.
/// `known_names` are extra literal strings (people, places) to replace with `[NAME_n]`.
pub fn redact(text: &str, known_names: &[String]) -> Result<Redacted, String> {
    let _ = (text, known_names);
    unimplemented!("Kilo: escalation::redact::redact")
}

/// Put the original values back into a reply.
pub fn rehydrate(text: &str, map: &RedactionMap) -> String {
    let _ = (text, map);
    unimplemented!("Kilo: escalation::redact::rehydrate")
}
