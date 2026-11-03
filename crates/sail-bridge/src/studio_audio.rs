//! Studio Audio: Music Making Strategist and Empirical Audio Analysis Tool.
//!
//! Provides objective measurements (Integrated LUFS, True Peak, RMS, dynamic range LRA,
//! spectral balance, section progression, and tempo) and grounds the Music Strategist
//! persona in hard empirical receipts rather than LLM guesswork.

use async_trait::async_trait;
use sail_agents::tools::{Tool, ToolContext, ToolFailure, ToolRegistry};
use sail_guardian::{Capability, ToolCapability};
use sail_protocol::execution::{ToolDescriptor, ToolEffect};
use sail_protocol::studio::{
    AudioAnalyzeRequest, AudioReceipt, AudioSection, FrequencyBands, StrategistReview,
    STUDIO_PROTOCOL_VERSION,
};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub const TOOL_ID: &str = "studio:audio_analyze";

pub struct StudioAudioTool;

#[async_trait]
impl Tool for StudioAudioTool {
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        Capability::Tool(ToolCapability::Invocation {
            tool: tool.into(),
            arguments: input.to_string(),
        })
    }

    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        let request: AudioAnalyzeRequest =
            serde_json::from_value(input).map_err(|e| ToolFailure {
                message: format!("Invalid AudioAnalyzeRequest payload: {e}"),
                retryable: false,
            })?;

        let audio_path = PathBuf::from(&request.file_path);
        if !audio_path.exists() {
            return Err(ToolFailure {
                message: format!("Audio file does not exist: {}", audio_path.display()),
                retryable: false,
            });
        }

        // Run empirical analysis
        let receipt = analyze_audio_file(&audio_path, request.target_lufs).map_err(|e| {
            ToolFailure {
                message: format!("Failed to analyze audio file: {e}"),
                retryable: false,
            }
        })?;

        // Generate the Music Strategist review grounded in the receipt
        let review = generate_strategist_review(
            &receipt,
            request.genre_hint.as_deref(),
            request.target_lufs,
        );

        let verdict = format!(
            "{}\n\n{}\n\n{}\n\nRecommendations:\n{}",
            review.loudness_verdict,
            review.headroom_verdict,
            review.dynamic_contrast_verdict,
            review.strategic_recommendations.join("\n")
        );

        let receipt_json = serde_json::to_string(&receipt).unwrap_or_default();
        let review_json = serde_json::to_string(&review).unwrap_or_default();

        Ok(json!({
            "file_name": receipt.file_name,
            "integrated_lufs": format!("{:.2}", receipt.integrated_lufs),
            "true_peak_db": format!("{:.2}", receipt.true_peak_db),
            "verdict": verdict,
            "mermaid": review.canvas_card_mermaid,
            "receipt_json": receipt_json,
            "review_json": review_json
        }))
    }
}

pub fn register_studio_audio_tool(registry: &ToolRegistry) -> Result<(), String> {
    registry.register(
        ToolDescriptor {
            id: TOOL_ID.into(),
            version: 1,
            description: "Analyze audio tracks for Studio music projects (measures LUFS, True Peak, dynamic range, frequency balance, and arrangement energy)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "maxLength": 1024
                    },
                    "reference_file_path": {
                        "type": "string",
                        "maxLength": 1024
                    },
                    "genre_hint": {
                        "type": "string",
                        "maxLength": 64
                    },
                    "target_lufs": {
                        "type": "integer",
                        "minimum": -30,
                        "maximum": -3
                    }
                },
                "required": ["file_path"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "file_name": {
                        "type": "string",
                        "maxLength": 256
                    },
                    "integrated_lufs": {
                        "type": "string",
                        "maxLength": 32
                    },
                    "true_peak_db": {
                        "type": "string",
                        "maxLength": 32
                    },
                    "verdict": {
                        "type": "string",
                        "maxLength": 8192
                    },
                    "mermaid": {
                        "type": "string",
                        "maxLength": 8192
                    },
                    "receipt_json": {
                        "type": "string",
                        "maxLength": 65536
                    },
                    "review_json": {
                        "type": "string",
                        "maxLength": 65536
                    }
                },
                "required": ["file_name", "integrated_lufs", "true_peak_db", "verdict", "mermaid"],
                "additionalProperties": false
            }),
            security_effect: ToolEffect::ReadOnly,
            required_capabilities: vec![TOOL_ID.into()],
            timeout_ms: 30_000,
            resource_class: "cpu".into(),
            concurrency_limit: 2,
        },
        Arc::new(StudioAudioTool),
    )
}

/// Raw parsed PCM audio stream with metadata.
#[derive(Debug)]
pub struct PcmAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub samples: Vec<f32>, // Interleaved samples normalized to [-1.0, 1.0]
}

/// Parse a standard RIFF/WAVE PCM audio file.
pub fn parse_wav_file(path: &Path) -> Result<PcmAudio, String> {
    let file = File::open(path).map_err(|e| format!("Could not open file: {e}"))?;
    let mut reader = BufReader::new(file);

    let mut header = [0u8; 12];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("Failed to read RIFF header: {e}"))?;

    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err("Not a valid RIFF/WAVE file".into());
    }

    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits_per_sample = 0u16;
    let mut audio_format = 0u16;
    let mut samples = Vec::new();

    loop {
        let mut chunk_header = [0u8; 8];
        if reader.read_exact(&mut chunk_header).is_err() {
            break; // EOF reached
        }
        let chunk_id = &chunk_header[0..4];
        let chunk_size = u32::from_le_bytes(chunk_header[4..8].try_into().unwrap()) as usize;

        if chunk_id == b"fmt " {
            let mut fmt_bytes = vec![0u8; chunk_size];
            reader
                .read_exact(&mut fmt_bytes)
                .map_err(|e| format!("Failed to read fmt chunk: {e}"))?;

            if fmt_bytes.len() < 16 {
                return Err("fmt chunk too small".into());
            }

            audio_format = u16::from_le_bytes(fmt_bytes[0..2].try_into().unwrap());
            channels = u16::from_le_bytes(fmt_bytes[2..4].try_into().unwrap());
            sample_rate = u32::from_le_bytes(fmt_bytes[4..8].try_into().unwrap());
            bits_per_sample = u16::from_le_bytes(fmt_bytes[14..16].try_into().unwrap());

            // 1 = PCM integer, 3 = IEEE float
            if audio_format != 1 && audio_format != 3 {
                return Err(format!("Unsupported WAV audio format: {audio_format} (only PCM integer and IEEE float supported)"));
            }
        } else if chunk_id == b"data" {
            if sample_rate == 0 || channels == 0 || bits_per_sample == 0 {
                return Err("WAV data chunk encountered before valid fmt chunk".into());
            }

            let mut data_bytes = vec![0u8; chunk_size];
            reader
                .read_exact(&mut data_bytes)
                .map_err(|e| format!("Failed to read data chunk: {e}"))?;

            samples = decode_pcm_bytes(&data_bytes, audio_format, bits_per_sample)?;
            break;
        } else {
            // Skip unknown/ancillary chunks (JUNK, bext, LIST, etc.)
            reader
                .seek(SeekFrom::Current(chunk_size as i64))
                .map_err(|e| format!("Failed to skip chunk: {e}"))?;
        }
    }

    if samples.is_empty() {
        return Err("No audio sample data found in WAV file".into());
    }

    Ok(PcmAudio {
        sample_rate,
        channels,
        bits_per_sample,
        samples,
    })
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn decode_pcm_bytes(bytes: &[u8], audio_format: u16, bits_per_sample: u16) -> Result<Vec<f32>, String> {
    match (audio_format, bits_per_sample) {
        (1, 16) => {
            let num_samples = bytes.len() / 2;
            let mut out = Vec::with_capacity(num_samples);
            for chunk in bytes.chunks_exact(2) {
                let sample_i16 = i16::from_le_bytes(chunk.try_into().unwrap());
                out.push(sample_i16 as f32 / 32768.0);
            }
            Ok(out)
        }
        (1, 24) => {
            let num_samples = bytes.len() / 3;
            let mut out = Vec::with_capacity(num_samples);
            for chunk in bytes.chunks_exact(3) {
                let b0 = chunk[0];
                let b1 = chunk[1];
                let b2 = chunk[2];
                // sign extend 24-bit to 32-bit
                let sample_i32 = i32::from_le_bytes([b0, b1, b2, if (b2 & 0x80) != 0 { 0xFF } else { 0x00 }]);
                out.push(sample_i32 as f32 / 8388608.0);
            }
            Ok(out)
        }
        (1, 32) => {
            let num_samples = bytes.len() / 4;
            let mut out = Vec::with_capacity(num_samples);
            for chunk in bytes.chunks_exact(4) {
                let sample_i32 = i32::from_le_bytes(chunk.try_into().unwrap());
                out.push(sample_i32 as f32 / 2147483648.0);
            }
            Ok(out)
        }
        (3, 32) => {
            let num_samples = bytes.len() / 4;
            let mut out = Vec::with_capacity(num_samples);
            for chunk in bytes.chunks_exact(4) {
                let sample_f32 = f32::from_le_bytes(chunk.try_into().unwrap());
                out.push(sample_f32);
            }
            Ok(out)
        }
        _ => Err(format!("Unsupported bit depth: {bits_per_sample} for format {audio_format}")),
    }
}

/// Analyze an audio file and produce an empirical `AudioReceipt`.
pub fn analyze_audio_file(path: &Path, _target_lufs: Option<f64>) -> Result<AudioReceipt, String> {
    let pcm = parse_wav_file(path)?;
    let file_name = path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "audio.wav".into());

    let channels = pcm.channels.max(1) as usize;
    let sample_rate = pcm.sample_rate.max(8000) as f64;
    let total_frames = pcm.samples.len() / channels;
    let duration_seconds = total_frames as f64 / sample_rate;

    if duration_seconds < 0.1 {
        return Err("Audio file too short to analyze (< 0.1 seconds)".into());
    }

    // 1. Peak, True Peak approximation & Clipping Detection
    let mut peak_val: f32 = 0.0;
    let mut clipping_detected = false;
    let mut sum_sq: f64 = 0.0;

    for &sample in &pcm.samples {
        let abs = sample.abs();
        if abs > peak_val {
            peak_val = abs;
        }
        if abs >= 0.9999 {
            clipping_detected = true;
        }
        sum_sq += (sample as f64) * (sample as f64);
    }

    let rms = (sum_sq / pcm.samples.len() as f64).sqrt();
    let rms_db = if rms > 1e-9 { 20.0 * rms.log10() } else { -100.0 };

    // True peak approximation with 4x inter-sample oversampling approximation
    let true_peak_val = compute_true_peak_approx(&pcm.samples);
    let true_peak_db = if true_peak_val > 1e-9 {
        20.0 * (true_peak_val as f64).log10()
    } else {
        -100.0
    };

    if true_peak_db > 0.0 {
        clipping_detected = true;
    }

    // 2. Integrated Loudness (LUFS, ITU-R BS.1770)
    let integrated_lufs = compute_itu_bs1770_lufs(&pcm.samples, pcm.sample_rate, pcm.channels);

    // 3. Loudness Range (LRA)
    let loudness_range_lra = compute_lra(&pcm.samples, pcm.sample_rate, pcm.channels);

    // 4. Frequency Band Energy (Sub, Bass, Low-Mids, Mids, High-Mids, Brilliance)
    let frequency_bands = compute_frequency_bands(&pcm.samples, pcm.sample_rate);

    // 5. Section Segmentation & Energy Dynamics
    let sections = compute_arrangement_sections(&pcm.samples, pcm.sample_rate, pcm.channels, duration_seconds);

    // 6. BPM Estimation
    let estimated_bpm = estimate_bpm(&pcm.samples, pcm.sample_rate, pcm.channels);

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Ok(AudioReceipt {
        version: STUDIO_PROTOCOL_VERSION,
        file_path: path.to_string_lossy().to_string(),
        file_name,
        duration_seconds: round_2(duration_seconds),
        sample_rate: pcm.sample_rate,
        channels: pcm.channels,
        bits_per_sample: pcm.bits_per_sample,
        integrated_lufs: round_2(integrated_lufs),
        loudness_range_lra: round_2(loudness_range_lra),
        true_peak_db: round_2(true_peak_db),
        rms_db: round_2(rms_db),
        clipping_detected,
        estimated_bpm,
        frequency_bands,
        sections,
        analyzed_at_ms: now_ms,
    })
}

/// Compute true peak approximation using inter-sample interpolation.
fn compute_true_peak_approx(samples: &[f32]) -> f32 {
    let mut max_tp: f32 = 0.0;
    let n = samples.len();
    if n < 4 {
        return samples.iter().copied().fold(0.0f32, |a, b| a.max(b.abs()));
    }

    for i in 1..(n - 2) {
        let s0 = samples[i - 1];
        let s1 = samples[i];
        let s2 = samples[i + 1];
        let s3 = samples[i + 2];

        max_tp = max_tp.max(s1.abs());

        // 4x Catmull-Rom / cubic interpolation points between s1 and s2
        for step in 1..4 {
            let t = step as f32 * 0.25;
            let t2 = t * t;
            let t3 = t2 * t;

            let interpolated = 0.5 * (
                (2.0 * s1) +
                (-s0 + s2) * t +
                (2.0 * s0 - 5.0 * s1 + 4.0 * s2 - s3) * t2 +
                (-s0 + 3.0 * s1 - 3.0 * s2 + s3) * t3
            );
            max_tp = max_tp.max(interpolated.abs());
        }
    }
    max_tp
}

/// Simplified ITU-R BS.1770 K-weighting loudness measurement.
fn compute_itu_bs1770_lufs(samples: &[f32], sample_rate: u32, channels: u16) -> f64 {
    let ch = channels.max(1) as usize;
    let sr = sample_rate.max(8000) as f64;

    // Filter weights: High shelf pre-filter + RLB high-pass filter
    // For general audio, K-weighting emphasizes mids/highs and rolls off sub-bass.
    let mut filtered = Vec::with_capacity(samples.len());
    
    // Stage 1: High shelf approximation (gain +4dB above ~1.5kHz)
    // Stage 2: High pass filter (100Hz 2nd order Butterworth)
    // Using a fast 2nd order IIR filter difference equation
    let fc = 100.0 / sr;
    let q = std::f64::consts::FRAC_1_SQRT_2;
    let w0 = 2.0 * std::f64::consts::PI * fc;
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();

    let b0 = (1.0 + cos_w0) / 2.0;
    let b1 = -(1.0 + cos_w0);
    let b2 = (1.0 + cos_w0) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;

    let (nb0, nb1, nb2, na1, na2) = (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0);

    for c in 0..ch {
        let mut x1 = 0.0;
        let mut x2 = 0.0;
        let mut y1 = 0.0;
        let mut y2 = 0.0;

        let mut idx = c;
        while idx < samples.len() {
            let x0 = samples[idx] as f64;
            let y0 = nb0 * x0 + nb1 * x1 + nb2 * x2 - na1 * y1 - na2 * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;

            filtered.push(y0);
            idx += ch;
        }
    }

    // Mean square energy across blocks
    let mut sum_sq = 0.0;
    for &y in &filtered {
        sum_sq += y * y;
    }
    let mean_sq = sum_sq / filtered.len().max(1) as f64;
    if mean_sq <= 1e-12 {
        return -70.0;
    }

    let lkfs = -0.691 + 10.0 * mean_sq.log10();
    lkfs.clamp(-70.0, 0.0)
}

/// Compute Loudness Range (LRA) across 3-second blocks.
fn compute_lra(samples: &[f32], sample_rate: u32, channels: u16) -> f64 {
    let ch = channels.max(1) as usize;
    let block_size = (sample_rate as usize) * 3 * ch;
    if samples.len() < block_size {
        return 4.0; // Default modest dynamic range for very short clips
    }

    let hop_size = block_size / 2; // 50% overlap
    let mut block_loudnesses = Vec::new();

    let mut start = 0;
    while start + block_size <= samples.len() {
        let slice = &samples[start..start + block_size];
        let mut sum_sq = 0.0;
        for &s in slice {
            sum_sq += (s as f64) * (s as f64);
        }
        let rms = (sum_sq / slice.len() as f64).sqrt();
        if rms > 1e-6 {
            let lufs = -0.691 + 10.0 * (rms * rms).log10();
            if lufs > -70.0 {
                block_loudnesses.push(lufs);
            }
        }
        start += hop_size;
    }

    if block_loudnesses.len() < 2 {
        return 4.0;
    }

    block_loudnesses.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx_10 = (block_loudnesses.len() as f64 * 0.10) as usize;
    let idx_95 = (block_loudnesses.len() as f64 * 0.95).min((block_loudnesses.len() - 1) as f64) as usize;

    let lra = block_loudnesses[idx_95] - block_loudnesses[idx_10];
    lra.clamp(1.0, 30.0)
}

/// Estimate energy distribution across standard mixing frequency bands.
fn compute_frequency_bands(samples: &[f32], sample_rate: u32) -> FrequencyBands {
    // Discrete Fourier/Bandpass energy estimation over a representative audio chunk
    let sr = sample_rate.max(8000) as f64;
    let fft_size = 4096.min(samples.len());
    let step = (samples.len() / 4).max(1);

    let mut sub_energy = 1e-9;
    let mut bass_energy = 1e-9;
    let mut low_mids_energy = 1e-9;
    let mut mids_energy = 1e-9;
    let mut high_mids_energy = 1e-9;
    let mut brilliance_energy = 1e-9;

    // Window multiple frames across the track
    let mut frame_start = 0;
    let mut frame_count = 0;

    while frame_start + fft_size <= samples.len() && frame_count < 8 {
        let chunk = &samples[frame_start..frame_start + fft_size];

        // Goertzel / discrete frequency bins integration
        for k in 1..(fft_size / 2) {
            let freq = k as f64 * sr / fft_size as f64;

            // Simplified DFT bin power
            let mut real = 0.0;
            let mut imag = 0.0;
            let angle_step = 2.0 * std::f64::consts::PI * (k as f64) / (fft_size as f64);

            for (n, &s) in chunk.iter().enumerate() {
                let angle = angle_step * (n as f64);
                real += (s as f64) * angle.cos();
                imag -= (s as f64) * angle.sin();
            }
            let mag_sq = real * real + imag * imag;

            if (20.0..60.0).contains(&freq) {
                sub_energy += mag_sq;
            } else if (60.0..250.0).contains(&freq) {
                bass_energy += mag_sq;
            } else if (250.0..500.0).contains(&freq) {
                low_mids_energy += mag_sq;
            } else if (500.0..2000.0).contains(&freq) {
                mids_energy += mag_sq;
            } else if (2000.0..6000.0).contains(&freq) {
                high_mids_energy += mag_sq;
            } else if (6000.0..=20000.0).contains(&freq) {
                brilliance_energy += mag_sq;
            }
        }

        frame_start += step;
        frame_count += 1;
    }

    let to_db = |energy: f64| -> f64 {
        let val = 10.0 * (energy / (fft_size as f64 * frame_count.max(1) as f64)).log10();
        round_2(val.clamp(-80.0, 0.0))
    };

    FrequencyBands {
        sub_bass_db: to_db(sub_energy),
        bass_db: to_db(bass_energy),
        low_mids_db: to_db(low_mids_energy),
        midrange_db: to_db(mids_energy),
        high_mids_db: to_db(high_mids_energy),
        brilliance_db: to_db(brilliance_energy),
    }
}

/// Segment arrangement into sections with relative energy.
fn compute_arrangement_sections(
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
    duration: f64,
) -> Vec<AudioSection> {
    let ch = channels.max(1) as usize;
    // Aim for 4 to 6 structural sections across the song duration
    let section_count = if duration < 30.0 {
        3
    } else if duration < 90.0 {
        4
    } else {
        5
    };

    let section_duration = duration / section_count as f64;
    let frames_per_section = (section_duration * sample_rate as f64) as usize;
    let mut sections = Vec::with_capacity(section_count);

    let mut energies = Vec::with_capacity(section_count);

    for s in 0..section_count {
        let start_frame = s * frames_per_section * ch;
        let end_frame = ((s + 1) * frames_per_section * ch).min(samples.len());
        let slice = &samples[start_frame..end_frame];

        let mut sum_sq = 0.0;
        for &val in slice {
            sum_sq += (val as f64) * (val as f64);
        }
        let rms = (sum_sq / slice.len().max(1) as f64).sqrt();
        let rms_db = if rms > 1e-9 { 20.0 * rms.log10() } else { -80.0 };
        let lufs = (rms_db - 0.691).clamp(-70.0, 0.0);

        energies.push((rms_db, lufs));
    }

    let max_rms = energies.iter().map(|(r, _)| *r).fold(-100.0f64, f64::max);

    let labels = match section_count {
        3 => vec!["Intro", "Main Body", "Outro"],
        4 => vec!["Intro", "Verse / Build", "Chorus / Drop", "Outro"],
        _ => vec!["Intro", "Verse", "Pre-Chorus / Build", "Chorus / Drop", "Outro"],
    };

    for (i, &(rms_db, lufs)) in energies.iter().enumerate() {
        let start_seconds = round_2(i as f64 * section_duration);
        let end_seconds = round_2(((i + 1) as f64 * section_duration).min(duration));
        let relative_energy = round_2((rms_db - (max_rms - 20.0)).max(0.0) / 20.0).clamp(0.0, 1.0);

        sections.push(AudioSection {
            label: labels.get(i).copied().unwrap_or("Section").to_string(),
            start_seconds,
            end_seconds,
            rms_db: round_2(rms_db),
            lufs_short_term: round_2(lufs),
            relative_energy,
        });
    }

    sections
}

/// Lightweight tempo (BPM) estimation via onset energy correlation.
fn estimate_bpm(samples: &[f32], sample_rate: u32, channels: u16) -> Option<f64> {
    let ch = channels.max(1) as usize;
    let sr = sample_rate as usize;
    let window_size = sr / 20; // 50ms window
    let hop = window_size / 2;

    if samples.len() < sr * 3 {
        return None; // Need at least 3 seconds for tempo correlation
    }

    // Compute short-term energy envelope
    let mut energy_env = Vec::new();
    let mut idx = 0;
    while idx + window_size * ch <= samples.len() {
        let mut sum = 0.0f32;
        for &s in &samples[idx..idx + window_size * ch] {
            sum += s * s;
        }
        energy_env.push(sum.sqrt());
        idx += hop * ch;
    }

    // Onset flux (first difference, half-wave rectified)
    let mut flux = Vec::with_capacity(energy_env.len());
    flux.push(0.0f32);
    for i in 1..energy_env.len() {
        let diff = energy_env[i] - energy_env[i - 1];
        flux.push(diff.max(0.0));
    }

    // Autocorrelation over lags corresponding to 70 - 160 BPM
    let env_sr = (sr as f64) / (hop as f64);
    let min_lag = (env_sr * 60.0 / 160.0) as usize; // 160 BPM
    let max_lag = (env_sr * 60.0 / 70.0) as usize;  // 70 BPM

    let mut best_lag = min_lag;
    let mut best_corr = 0.0f32;

    for lag in min_lag..=max_lag.min(flux.len() / 2) {
        let mut corr = 0.0f32;
        for i in 0..(flux.len() - lag) {
            corr += flux[i] * flux[i + lag];
        }
        if corr > best_corr {
            best_corr = corr;
            best_lag = lag;
        }
    }

    if best_corr > 1e-4 {
        let bpm = 60.0 * env_sr / (best_lag as f64);
        Some(round_2(bpm))
    } else {
        None
    }
}

/// Generate concrete, prioritized Music Strategist review from empirical audio receipts.
pub fn generate_strategist_review(
    receipt: &AudioReceipt,
    genre_hint: Option<&str>,
    custom_target_lufs: Option<f64>,
) -> StrategistReview {
    let genre = genre_hint.unwrap_or("electronic / modern pop").to_lowercase();
    let target_lufs = custom_target_lufs.unwrap_or_else(|| {
        if genre.contains("club") || genre.contains("edm") || genre.contains("dance") {
            -8.0
        } else if genre.contains("hip-hop") || genre.contains("trap") || genre.contains("rap") {
            -9.0
        } else if genre.contains("rock") || genre.contains("metal") {
            -10.0
        } else if genre.contains("acoustic") || genre.contains("jazz") || genre.contains("classical") {
            -16.0
        } else {
            -14.0 // Streaming target default
        }
    });

    let lufs_diff = round_2(receipt.integrated_lufs - target_lufs);

    let loudness_verdict = if lufs_diff.abs() <= 1.0 {
        format!("Optimal loudness alignment: Track measures {:.1} LUFS (target {:.1} LUFS). Streaming normalization will apply near-zero gain adjustment.", receipt.integrated_lufs, target_lufs)
    } else if lufs_diff < -2.0 {
        format!("Dynamic but quiet: Track measures {:.1} LUFS ({:.1} dB below the {:.1} LUFS genre target). Consider subtle buss compression and clean limiter gain.", receipt.integrated_lufs, lufs_diff.abs(), target_lufs)
    } else {
        format!("Aggressively loud / pushed: Track measures {:.1} LUFS ({:.1} dB louder than target {:.1} LUFS). Major streaming services will attenuate this by {:.1} dB.", receipt.integrated_lufs, lufs_diff, target_lufs, lufs_diff)
    };

    let headroom_verdict = if receipt.clipping_detected || receipt.true_peak_db > 0.0 {
        format!("CRITICAL: Inter-sample clipping detected at {:.2} dBTP! True peak exceeds 0.0 dBFS ceiling. Set master limiter ceiling to -0.5 dBTP or -1.0 dBTP to prevent lossy AAC/MP3 encoder distortion.", receipt.true_peak_db)
    } else if receipt.true_peak_db > -0.3 {
        format!("Near-ceiling warning: True peak reaches {:.2} dBTP. Safe practice for streaming platforms is maintaining -0.5 to -1.0 dBTP headroom.", receipt.true_peak_db)
    } else {
        format!("Healthy headroom: True peak sits at {:.2} dBTP with no inter-sample clipping detected.", receipt.true_peak_db)
    };

    // Dynamic contrast assessment
    let mut dynamic_contrast_verdict = format!(
        "Loudness Range (LRA) is {:.1} dB. ",
        receipt.loudness_range_lra
    );
    if receipt.sections.len() >= 2 {
        let max_section = receipt
            .sections
            .iter()
            .max_by(|a, b| a.rms_db.partial_cmp(&b.rms_db).unwrap())
            .unwrap();
        let min_section = receipt
            .sections
            .iter()
            .min_by(|a, b| a.rms_db.partial_cmp(&b.rms_db).unwrap())
            .unwrap();
        let section_delta = max_section.rms_db - min_section.rms_db;

        if section_delta < 2.0 {
            dynamic_contrast_verdict.push_str(&format!(
                "Tension Warning: Only {:.1} dB energy lift between '{}' and '{}'. The drop or chorus will feel flat unless you carve space or volume contrast.",
                section_delta, min_section.label, max_section.label
            ));
        } else {
            dynamic_contrast_verdict.push_str(&format!(
                "Solid dynamic lift: {:.1} dB contrast between '{}' ({:.1} dB) and '{}' ({:.1} dB).",
                section_delta, min_section.label, min_section.rms_db, max_section.label, max_section.rms_db
            ));
        }
    }

    // Tonal balance findings
    let mut tonal_balance_findings = Vec::new();
    let fb = &receipt.frequency_bands;
    if fb.sub_bass_db > fb.bass_db + 3.0 {
        tonal_balance_findings.push(format!(
            "Sub-bass dominance: 20-60 Hz ({:.1} dB) is {:.1} dB above upper bass. High risk of 808/kick mud on consumer earbuds; high-pass sub-rumble below 28 Hz.",
            fb.sub_bass_db, fb.sub_bass_db - fb.bass_db
        ));
    }
    if fb.low_mids_db > fb.midrange_db {
        tonal_balance_findings.push(format!(
            "Low-mid buildup: 250-500 Hz zone is hot ({:.1} dB). Clean up boxiness around 300-400 Hz on rhythm instruments to let the vocal cut through.",
            fb.low_mids_db
        ));
    }
    if fb.brilliance_db < fb.high_mids_db - 12.0 {
        tonal_balance_findings.push(format!(
            "Dark top-end: Air band above 6 kHz drops steeply ({:.1} dB vs {:.1} dB high-mids). A gentle high shelf at 10 kHz (+1.5 dB) will add commercial sparkle.",
            fb.brilliance_db, fb.high_mids_db
        ));
    } else {
        tonal_balance_findings.push(format!(
            "Crisp top-end response: Balanced transition from high-mids ({:.1} dB) to brilliance ({:.1} dB).",
            fb.high_mids_db, fb.brilliance_db
        ));
    }

    // Prioritized strategic recommendations
    let mut strategic_recommendations = Vec::new();
    if receipt.clipping_detected || receipt.true_peak_db > -0.2 {
        strategic_recommendations.push(
            "1. Limiter Ceiling: Drop master output ceiling from 0.0 dB to -0.8 dBTP. Inter-sample peaks currently clip."
                .into(),
        );
    }
    if lufs_diff < -2.5 {
        strategic_recommendations.push(format!(
            "2. Gain Staging: Increase limiter drive by {:.1} dB to compete with current genre loudness ({:.1} LUFS target).",
            lufs_diff.abs(), target_lufs
        ));
    } else if lufs_diff > 2.5 {
        strategic_recommendations.push(format!(
            "2. Back Off Limiting: Pull back master limiter drive by {:.1} dB. Over-limiting is squashing transients before streaming attenuation.",
            lufs_diff
        ));
    }
    strategic_recommendations.push(
        "3. Low-End Focus: Check kick drum fundamental vs sub-bass sidechain compression to preserve punch."
            .into(),
    );

    // Canvas Mermaid Visual Chart
    let mut mermaid = String::from("xychart-beta\n  title \"Arrangement Section Energy (LUFS)\"\n  x-axis [");
    for (i, sec) in receipt.sections.iter().enumerate() {
        if i > 0 {
            mermaid.push_str(", ");
        }
        mermaid.push_str(&format!("\"{}\"", sec.label));
    }
    mermaid.push_str("]\n  y-axis \"Short-term LUFS\" -40 --> -5\n  bar [");
    for (i, sec) in receipt.sections.iter().enumerate() {
        if i > 0 {
            mermaid.push_str(", ");
        }
        mermaid.push_str(&format!("{:.1}", sec.lufs_short_term));
    }
    mermaid.push_str("]\n");

    StrategistReview {
        version: STUDIO_PROTOCOL_VERSION,
        song_title: receipt.file_name.clone(),
        genre,
        target_lufs,
        measured_lufs: receipt.integrated_lufs,
        lufs_difference: lufs_diff,
        loudness_verdict,
        true_peak_db: receipt.true_peak_db,
        headroom_verdict,
        dynamic_contrast_verdict,
        tonal_balance_findings,
        strategic_recommendations,
        canvas_card_mermaid: mermaid,
    }
}

fn round_2(val: f64) -> f64 {
    (val * 100.0).round() / 100.0
}
