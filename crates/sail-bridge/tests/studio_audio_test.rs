//! Integration tests for Studio Audio analyzer and the Music Strategist.

use sail_agents::tools::{ToolContext, ToolRegistry, ToolScheduler};
use sail_bridge::studio_audio::{
    TOOL_ID, analyze_audio_file, generate_strategist_review, parse_wav_file,
    register_studio_audio_tool,
};
use serde_json::json;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use sail_guardian::{
    ApprovalBroker, AuditLedger, GuardianExecutionBroker, PolicyEngine, SandboxPolicy,
};

fn test_scheduler(registry: Arc<ToolRegistry>) -> ToolScheduler {
    let guardian = Arc::new(GuardianExecutionBroker::new(
        Arc::new(PolicyEngine::new()),
        Arc::new(AuditLedger::new()),
        Arc::new(ApprovalBroker::new()),
        Arc::new(SandboxPolicy::new()),
    ));
    ToolScheduler::new(registry, guardian, 4)
}

struct TempTestDir(PathBuf);
impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Helper to generate a valid RIFF/WAVE PCM audio file with synthetic tones.
fn create_synthetic_wav(
    path: &Path,
    sample_rate: u32,
    channels: u16,
    duration_secs: f64,
    clip_peak: bool,
) {
    let mut file = File::create(path).expect("failed to create test audio file");
    let num_samples = (sample_rate as f64 * duration_secs) as usize;
    let bytes_per_sample = 2; // 16-bit
    let block_align = channels * bytes_per_sample;
    let byte_rate = sample_rate * block_align as u32;
    let data_chunk_size = (num_samples * block_align as usize) as u32;
    let file_size = 36 + data_chunk_size;

    // RIFF header
    file.write_all(b"RIFF").unwrap();
    file.write_all(&file_size.to_le_bytes()).unwrap();
    file.write_all(b"WAVE").unwrap();

    // fmt subchunk
    file.write_all(b"fmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap(); // Subchunk1Size (16 for PCM)
    file.write_all(&1u16.to_le_bytes()).unwrap(); // AudioFormat (1 = PCM)
    file.write_all(&channels.to_le_bytes()).unwrap();
    file.write_all(&sample_rate.to_le_bytes()).unwrap();
    file.write_all(&byte_rate.to_le_bytes()).unwrap();
    file.write_all(&block_align.to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap(); // BitsPerSample

    // data subchunk
    file.write_all(b"data").unwrap();
    file.write_all(&data_chunk_size.to_le_bytes()).unwrap();

    // Generate samples:
    // Section 1 (Intro): 0.0 - 0.4*duration: quiet 440 Hz tone (-18 dBFS, amp ~ 0.12)
    // Section 2 (Chorus/Drop): 0.4 - 1.0*duration: loud 55 Hz sub-bass + 440 Hz lead (-4 dBFS, amp ~ 0.63)
    let mid_point = (num_samples as f64 * 0.4) as usize;
    for i in 0..num_samples {
        let t = i as f64 / sample_rate as f64;
        let sample_f = if i < mid_point {
            0.12 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
        } else {
            let sub = 0.40 * (2.0 * std::f64::consts::PI * 55.0 * t).sin();
            let lead = 0.25 * (2.0 * std::f64::consts::PI * 440.0 * t).sin();
            if clip_peak && i == mid_point + 100 {
                1.5 // Deliberate digital clip
            } else {
                sub + lead
            }
        };

        let sample_i16 = (sample_f.clamp(-1.0, 1.0) * 32767.0) as i16;
        for _ in 0..channels {
            file.write_all(&sample_i16.to_le_bytes()).unwrap();
        }
    }
}

/// Writes a 1 kHz stereo/mono sine whose segments use the given peak levels in dBFS, which
/// is what EBU Tech 3341 test signals are defined in.
fn create_tone_wav(path: &Path, sample_rate: u32, channels: u16, segments: &[(f64, f64)]) {
    let frames: usize = segments
        .iter()
        .map(|(seconds, _)| (sample_rate as f64 * seconds) as usize)
        .sum();
    let bytes_per_sample = 2u16;
    let block_align = channels * bytes_per_sample;
    let data_chunk_size = (frames * block_align as usize) as u32;

    let mut file = File::create(path).expect("failed to create tone file");
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + data_chunk_size).to_le_bytes())
        .unwrap();
    file.write_all(b"WAVE").unwrap();
    file.write_all(b"fmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&channels.to_le_bytes()).unwrap();
    file.write_all(&sample_rate.to_le_bytes()).unwrap();
    file.write_all(&(sample_rate * block_align as u32).to_le_bytes())
        .unwrap();
    file.write_all(&block_align.to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data_chunk_size.to_le_bytes()).unwrap();

    let mut frame = 0usize;
    for (seconds, dbfs) in segments {
        let amplitude = 10f64.powf(dbfs / 20.0);
        let count = (sample_rate as f64 * seconds) as usize;
        for _ in 0..count {
            let t = frame as f64 / sample_rate as f64;
            let sample = (amplitude * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as f32;
            let sample_i16 = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
            for _ in 0..channels {
                file.write_all(&sample_i16.to_le_bytes()).unwrap();
            }
            frame += 1;
        }
    }
}

#[tokio::test]
async fn test_wav_parsing_and_analysis() {
    let temp_dir = TempTestDir::new("sail-audio-test");
    let wav_path = temp_dir.path().join("test_song.wav");
    create_synthetic_wav(&wav_path, 44100, 2, 4.0, false);

    // 1. Test pure WAV parsing
    let pcm = parse_wav_file(&wav_path).expect("WAV parsing must succeed");
    assert_eq!(pcm.sample_rate, 44100);
    assert_eq!(pcm.channels, 2);
    assert_eq!(pcm.bits_per_sample, 16);
    assert!(!pcm.samples.is_empty());

    // 2. Test empirical audio analysis
    let receipt = analyze_audio_file(&wav_path, Some(-14.0)).expect("Analysis must succeed");
    assert_eq!(receipt.file_name, "test_song.wav");
    assert!(receipt.duration_seconds >= 3.9 && receipt.duration_seconds <= 4.1);
    assert!(!receipt.clipping_detected, "No clipping should be detected");
    assert!(receipt.integrated_lufs < -5.0 && receipt.integrated_lufs > -30.0);
    assert!(receipt.true_peak_db <= 0.0);

    // Check frequency bands: 55 Hz sub-bass should be detected with significant energy
    assert!(receipt.frequency_bands.sub_bass_db > -50.0);

    // Check arrangement sections: later section (drop) should have higher energy than intro
    assert!(receipt.sections.len() >= 2);
    let intro = &receipt.sections[0];
    let drop_sec = &receipt.sections[receipt.sections.len() - 1];
    assert!(
        drop_sec.rms_db > intro.rms_db,
        "Drop section ({:.1} dB) must be louder than intro ({:.1} dB)",
        drop_sec.rms_db,
        intro.rms_db
    );

    // 3. Test Music Strategist review generation
    let review = generate_strategist_review(&receipt, Some("electronic"), Some(-8.0));
    assert_eq!(review.genre, "electronic");
    assert_eq!(review.target_lufs, -8.0);
    assert!(!review.loudness_verdict.is_empty());
    assert!(!review.headroom_verdict.is_empty());
    assert!(!review.strategic_recommendations.is_empty());
    assert!(review.canvas_card_mermaid.starts_with("xychart-beta"));
}

#[tokio::test]
async fn test_clipping_detection_and_headroom_alert() {
    let temp_dir = TempTestDir::new("sail-audio-clip");
    let clipped_path = temp_dir.path().join("clipped_song.wav");
    create_synthetic_wav(&clipped_path, 44100, 2, 2.0, true);

    let receipt = analyze_audio_file(&clipped_path, None).expect("Analysis must succeed");
    assert!(receipt.clipping_detected, "Clipping must be flagged");

    let review = generate_strategist_review(&receipt, Some("pop"), None);
    assert!(
        review.headroom_verdict.contains("CRITICAL")
            || review.headroom_verdict.contains("clipping"),
        "Must flag critical clipping in review: {}",
        review.headroom_verdict
    );
    assert!(
        review
            .strategic_recommendations
            .iter()
            .any(|r| r.contains("Limiter Ceiling")),
        "Must recommend lowering limiter ceiling"
    );
}

#[tokio::test]
async fn test_studio_audio_tool_scheduler_execution() {
    let temp_dir = TempTestDir::new("sail-audio-sched");
    let wav_path = temp_dir.path().join("mix_v1.wav");
    create_synthetic_wav(&wav_path, 44100, 2, 3.0, false);

    let registry = Arc::new(ToolRegistry::default());
    register_studio_audio_tool(&registry).expect("Tool registration must succeed");
    let scheduler = test_scheduler(registry);

    let request_id = Uuid::new_v4();
    let correlation_id = Uuid::new_v4();
    let context = ToolContext {
        request_id,
        correlation_id,
        actor: sail_common::types::SubsystemId::Bridge,
        cancel: CancellationToken::new(),
    };

    let input = json!({
        "file_path": wav_path.to_string_lossy(),
        "genre_hint": "hip-hop",
        "target_lufs": -9
    });

    let observation = scheduler.execute(context, TOOL_ID, input, None, true).await;
    assert!(
        observation.success,
        "Tool execution must succeed: {:?}",
        observation.error
    );

    // Verify observation data contains formatted fields and raw receipts
    let data = &observation.data;
    assert_eq!(data["file_name"], "mix_v1.wav");
    assert!(data.get("integrated_lufs").is_some());
    assert!(data.get("true_peak_db").is_some());
    assert!(data.get("verdict").is_some());
    assert!(
        data["mermaid"]
            .as_str()
            .unwrap()
            .starts_with("xychart-beta")
    );

    let review_str = data["review_json"].as_str().unwrap();
    let review: serde_json::Value = serde_json::from_str(review_str).unwrap();
    assert_eq!(review["genre"], "hip-hop");
    assert_eq!(review["target_lufs"], -9.0);
}

#[tokio::test]
async fn test_negative_cases() {
    let registry = Arc::new(ToolRegistry::default());
    register_studio_audio_tool(&registry).expect("Tool registration must succeed");
    let scheduler = test_scheduler(registry);

    let make_context = || ToolContext {
        request_id: Uuid::new_v4(),
        correlation_id: Uuid::new_v4(),
        actor: sail_common::types::SubsystemId::Bridge,
        cancel: CancellationToken::new(),
    };

    // Case 1: Non-existent file
    let input1 = json!({
        "file_path": "/tmp/definitely_not_a_real_song_12345.wav"
    });
    let obs1 = scheduler
        .execute(make_context(), TOOL_ID, input1, None, true)
        .await;
    assert!(!obs1.success);
    assert!(obs1.error.unwrap().contains("Audio file does not exist"));

    // Case 2: Corrupted non-WAV file
    let temp_dir = TempTestDir::new("sail-audio-corrupt");
    let corrupt_path = temp_dir.path().join("not_audio.wav");
    std::fs::write(&corrupt_path, b"Hello I am just plain text").unwrap();

    let input2 = json!({
        "file_path": corrupt_path.to_string_lossy()
    });
    let obs2 = scheduler
        .execute(make_context(), TOOL_ID, input2, None, true)
        .await;
    assert!(!obs2.success);
    assert!(obs2.error.unwrap().contains("Not a valid RIFF/WAVE"));

    // Case 3: Invalid input schema (missing required file_path)
    let input3 = json!({
        "genre_hint": "pop"
    });
    let obs3 = scheduler
        .execute(make_context(), TOOL_ID, input3, None, true)
        .await;
    assert!(!obs3.success);
}

// EBU Tech 3341 case 1: a stereo 1 kHz sine at -23.0 dBFS reads -23.0 LUFS. This fails if
// the K-weighting stages or the absolute/relative gates are wrong, which a range assertion
// on synthetic music would not catch.
#[tokio::test]
async fn ebu_tech_3341_calibration_sine_reads_its_own_level() {
    let dir = TempTestDir::new("sail-ebu3341");
    let path = dir.path().join("minus23.wav");
    create_tone_wav(&path, 48_000, 2, &[(10.0, -23.0)]);

    let receipt = analyze_audio_file(&path, None).expect("analysis must succeed");
    assert!(
        (receipt.integrated_lufs + 23.0).abs() <= 0.1,
        "-23.0 dBFS stereo 1 kHz sine must read -23.0 LUFS, got {:.2}",
        receipt.integrated_lufs
    );
}

// The relative gate must discard material far below the programme level instead of letting a
// quiet intro drag the integrated value down.
#[tokio::test]
async fn relative_gate_excludes_a_quiet_passage() {
    let dir = TempTestDir::new("sail-ebu3341-gate");
    let path = dir.path().join("gated.wav");
    create_tone_wav(&path, 48_000, 2, &[(5.0, -45.0), (15.0, -23.0)]);

    let receipt = analyze_audio_file(&path, None).expect("analysis must succeed");
    assert!(
        (receipt.integrated_lufs + 23.0).abs() <= 0.3,
        "the gated value must stay at the -23.0 dBFS level, got {:.2}",
        receipt.integrated_lufs
    );
}

// A range needs gated short-term windows; a short clip must report 0.0 and the review must
// say so instead of inventing a typical dynamic range.
#[tokio::test]
async fn a_clip_too_short_for_three_second_windows_reports_no_loudness_range() {
    let dir = TempTestDir::new("sail-lra-short");
    let path = dir.path().join("short.wav");
    create_tone_wav(&path, 48_000, 2, &[(2.0, -20.0)]);

    let receipt = analyze_audio_file(&path, None).expect("analysis must succeed");
    assert_eq!(receipt.loudness_range_lra, 0.0);

    let review = generate_strategist_review(&receipt, None, None);
    assert!(
        review.dynamic_contrast_verdict.contains("not measurable"),
        "review must not claim a loudness range it could not measure: {}",
        review.dynamic_contrast_verdict
    );
}
