//! Speaker labels are advisory observations, never authentication or permissions.
use serde::{Deserialize, Serialize};
pub const VERSION: u32 = 1;
pub const MAX_SAMPLES: usize = 16_000 * 60;
#[derive(Clone, Serialize, Deserialize)]
pub struct SpeakerVector {
    pub speaker: String,
    pub values: Vec<f32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub speaker: String,
    pub start_ms: u64,
    pub end_ms: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SpeakerOutput {
    pub version: u32,
    pub model_revision: String,
    pub segments: Vec<SpeakerSegment>,
    pub embeddings: Vec<SpeakerVector>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerMatch {
    pub speaker: String,
    pub profile_id: Option<String>,
    pub name: Option<String>,
    pub similarity: Option<f64>,
    pub decision: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerAnalysis {
    pub version: u32,
    pub segments: Vec<SpeakerSegment>,
    pub matches: Vec<SpeakerMatch>,
    pub advisory_only: bool,
}
impl SpeakerOutput {
    pub fn validate(&self, duration_ms: u64) -> Result<(), String> {
        if self.version != VERSION
            || self.model_revision.len() != 64
            || !self.model_revision.bytes().all(|c| c.is_ascii_hexdigit())
            || self.embeddings.len() > 16
            || self.segments.len() > 2000
        {
            return Err("invalid speaker result version, revision or bounds".into());
        }
        let mut labels = std::collections::HashSet::new();
        let mut dimensions = None;
        for e in &self.embeddings {
            if !e.speaker.starts_with("SPEAKER_")
                || e.speaker.len() > 32
                || !labels.insert(&e.speaker)
                || e.values.is_empty()
                || e.values.len() > 4096
                || e.values.iter().any(|v| !v.is_finite())
                || !e.values.iter().any(|v| *v != 0.0)
                || dimensions.is_some_and(|d| d != e.values.len())
            {
                return Err("invalid speaker embedding".into());
            }
            dimensions = Some(e.values.len());
        }
        for s in &self.segments {
            if !labels.contains(&s.speaker) || s.start_ms >= s.end_ms || s.end_ms > duration_ms {
                return Err("invalid speaker timeline".into());
            }
        }
        if self
            .embeddings
            .iter()
            .any(|e| !self.segments.iter().any(|s| s.speaker == e.speaker))
        {
            return Err("Speaker embedding has no detected speech".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_timestamps_labels_nan_zero_and_wrong_versions() {
        let valid = SpeakerOutput {
            version: 1,
            model_revision: "a".repeat(64),
            segments: vec![SpeakerSegment {
                speaker: "SPEAKER_00".into(),
                start_ms: 0,
                end_ms: 1000,
            }],
            embeddings: vec![SpeakerVector {
                speaker: "SPEAKER_00".into(),
                values: vec![1., 0.],
            }],
        };
        assert!(valid.validate(1000).is_ok());
        let mut x = valid.clone();
        x.segments[0].end_ms = 1001;
        assert!(x.validate(1000).is_err());
        let mut x = valid.clone();
        x.embeddings[0].values[0] = f32::NAN;
        assert!(x.validate(1000).is_err());
        let mut x = valid.clone();
        x.embeddings[0].values = vec![0., 0.];
        assert!(x.validate(1000).is_err());
        let mut x = valid.clone();
        x.version = 2;
        assert!(x.validate(1000).is_err());
        let mut x = valid;
        x.segments[0].speaker = "other".into();
        assert!(x.validate(1000).is_err());
    }
}
