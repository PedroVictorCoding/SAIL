//! Brute-force vector baseline and model-independent bounded context composition.
use sail_protocol::memory::MemoryHit;
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() || a.iter().chain(b).any(|v| !v.is_finite()) {
        return None;
    }
    let dot: f64 = a.iter().zip(b).map(|(a, b)| *a as f64 * *b as f64).sum();
    let norm = |v: &[f32]| v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let d = norm(a) * norm(b);
    (d > 0.0).then(|| dot / d)
}
/// Inject a tokenizer when available; the default caller charges UTF-8 bytes, a
/// conservative budget for byte-level tokenizers. Whole records are omitted, never cut.
pub fn compose(hits: &[MemoryHit], budget: usize, count: impl Fn(&str) -> usize) -> String {
    let mut output = String::new();
    for hit in hits {
        let line = format!(
            "{}\n",
            serde_json::to_string(hit).expect("finite ranked hit")
        );
        let candidate = format!("{output}{line}");
        if count(&candidate) <= budget.min(64000) {
            output = candidate;
        }
    }
    output
}
