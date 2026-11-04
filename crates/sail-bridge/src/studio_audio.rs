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
    AudioAnalyzeRequest, AudioReceipt, AudioSection, FrequencyBands, STUDIO_PROTOCOL_VERSION,
    StrategistReview,
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

        let audio_path = confine_to_workspace(&request.file_path).map_err(|e| ToolFailure {
            message: e,
            retryable: false,
        })?;
        if !audio_path.exists() {
            return Err(ToolFailure {
                message: format!("Audio file does not exist: {}", audio_path.display()),
                retryable: false,
            });
        }

        // Run empirical analysis
        let receipt =
            analyze_audio_file(&audio_path, request.target_lufs).map_err(|e| ToolFailure {
                message: format!("Failed to analyze audio file: {e}"),
                retryable: false,
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
                return Err(format!(
                    "Unsupported WAV audio format: {audio_format} (only PCM integer and IEEE float supported)"
                ));
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

fn decode_pcm_bytes(
    bytes: &[u8],
    audio_format: u16,
    bits_per_sample: u16,
) -> Result<Vec<f32>, String> {
    match (audio_format, bits_per_sample) {
        (1, 16) => {
            let mut out = Vec::with_capacity(bytes.len() / 2);
            for chunk in bytes.chunks(2).take_while(|chunk| chunk.len() == 2) {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                out.push(sample as f32 / 32768.0);
            }
            Ok(out)
        }
        (1, 24) => {
            let mut out = Vec::with_capacity(bytes.len() / 3);
            for chunk in bytes.chunks(3).take_while(|chunk| chunk.len() == 3) {
                let (b0, b1, b2) = (chunk[0], chunk[1], chunk[2]);
                // sign extend 24-bit to 32-bit
                let sample =
                    i32::from_le_bytes([b0, b1, b2, if (b2 & 0x80) != 0 { 0xFF } else { 0x00 }]);
                out.push(sample as f32 / 8388608.0);
            }
            Ok(out)
        }
        (1, 32) => {
            let mut out = Vec::with_capacity(bytes.len() / 4);
            for chunk in bytes.chunks(4).take_while(|chunk| chunk.len() == 4) {
                let sample = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push(sample as f32 / 2147483648.0);
            }
            Ok(out)
        }
        (3, 32) => {
            let mut out = Vec::with_capacity(bytes.len() / 4);
            for chunk in bytes.chunks(4).take_while(|chunk| chunk.len() == 4) {
                let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push(sample);
            }
            Ok(out)
        }
        _ => Err(format!(
            "Unsupported bit depth: {bits_per_sample} for format {audio_format}"
        )),
    }
}

/// Resolve a model-supplied audio path inside the authorized workspace. Absolute paths
/// outside the root, `..` traversal and symlink escapes are rejected. With no configured
/// `SAIL_TOOL_ROOT` the path is used as given, which keeps library and test callers simple;
/// the conversational route rejects escape attempts before it reaches this tool.
pub fn confine_to_workspace(requested: &str) -> Result<PathBuf, String> {
    let Ok(root) = std::env::var("SAIL_TOOL_ROOT") else {
        return Ok(PathBuf::from(requested));
    };
    let root = PathBuf::from(root)
        .canonicalize()
        .map_err(|e| format!("Invalid SAIL_TOOL_ROOT: {e}"))?;
    let candidate = Path::new(requested);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let resolved = joined
        .canonicalize()
        .map_err(|e| format!("Audio file does not exist: {} ({e})", joined.display()))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "Audio path {} is outside the authorized workspace {}",
            resolved.display(),
            root.display()
        ));
    }
    Ok(resolved)
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
    let rms_db = if rms > 1e-9 {
        20.0 * rms.log10()
    } else {
        -100.0
    };

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

    // 2-3. One K-weighted grid feeds integrated loudness (BS.1770-4) and loudness range
    // (EBU Tech 3342), so both numbers come from the same filtered signal.
    let loudness = LoudnessHops::analyze(&pcm.samples, pcm.sample_rate, pcm.channels);
    let integrated_lufs = loudness.integrated_loudness();
    let loudness_range_lra = loudness.loudness_range();

    // 4. Frequency Band Energy (Sub, Bass, Low-Mids, Mids, High-Mids, Brilliance)
    let frequency_bands = compute_frequency_bands(&pcm.samples, pcm.sample_rate);

    // 5. Section Segmentation & Energy Dynamics
    let sections = compute_arrangement_sections(
        &pcm.samples,
        pcm.sample_rate,
        pcm.channels,
        duration_seconds,
        &loudness,
    );

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

            let interpolated = 0.5
                * ((2.0 * s1)
                    + (-s0 + s2) * t
                    + (2.0 * s0 - 5.0 * s1 + 4.0 * s2 - s3) * t2
                    + (-s0 + 3.0 * s1 - 3.0 * s2 + s3) * t3);
            max_tp = max_tp.max(interpolated.abs());
        }
    }
    max_tp
}

/// One biquad section (transposed direct form II).
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn state(&self) -> BiquadState {
        BiquadState {
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }
}

impl BiquadState {
    fn step(&mut self, biquad: &Biquad, x0: f64) -> f64 {
        let y0 = biquad.b0 * x0 + biquad.b1 * self.x1 + biquad.b2 * self.x2
            - biquad.a1 * self.y1
            - biquad.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x0;
        self.y2 = self.y1;
        self.y1 = y0;
        y0
    }
}

/// ITU-R BS.1770-4 K-weighting: the high-shelf pre-filter then the RLB high-pass. The
/// standard tabulates 48 kHz coefficients; these are re-derived from its analog prototypes
/// for the file's own sample rate.
struct KWeighting {
    shelf: Biquad,
    high_pass: Biquad,
}

impl KWeighting {
    fn for_sample_rate(sample_rate: u32) -> Self {
        let fs = sample_rate.max(8000) as f64;
        let k_of = |f0: f64| (std::f64::consts::PI * f0 / fs).tan();

        // Stage 1: high shelf, about +4 dB above 1.7 kHz.
        let (f0, gain_db, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = k_of(f0);
        let vh = 10f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        };

        // Stage 2: RLB high pass at about 38 Hz.
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = k_of(f0);
        let a0 = 1.0 + k / q + k * k;
        let high_pass = Biquad {
            b0: 1.0 / a0,
            b1: -2.0 / a0,
            b2: 1.0 / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        };

        Self { shelf, high_pass }
    }

    fn apply(&self, shelf: &mut BiquadState, high_pass: &mut BiquadState, x: f64) -> f64 {
        let y = shelf.step(&self.shelf, x);
        high_pass.step(&self.high_pass, y)
    }
}

/// LUFS for a channel-summed mean square.
fn loudness_of(power: f64) -> f64 {
    if power <= 1e-12 {
        -70.0
    } else {
        -0.691 + 10.0 * power.log10()
    }
}

/// K-weighted sums of squares on a 100 ms grid, per channel. Both the BS.1770 integrated
/// loudness (400 ms blocks) and the EBU Tech 3342 loudness range (3 s windows) are built
/// from it, so the signal is filtered once and never materialized again.
struct LoudnessHops {
    channels: usize,
    hop_frames: usize,
    hops: usize,
    /// `sums[hop * channels + channel]` = squared K-weighted samples in that hop.
    sums: Vec<f64>,
}

impl LoudnessHops {
    fn analyze(samples: &[f32], sample_rate: u32, channels: u16) -> Self {
        let channels = channels.max(1) as usize;
        let sample_rate = sample_rate.max(8000);
        let hop_frames = ((sample_rate as f64) * 0.1).round().max(1.0) as usize;
        let frames = samples.len() / channels;
        let hops = frames / hop_frames;

        let weighting = KWeighting::for_sample_rate(sample_rate);
        let mut states: Vec<(BiquadState, BiquadState)> = (0..channels)
            .map(|_| (weighting.shelf.state(), weighting.high_pass.state()))
            .collect();
        let mut sums = vec![0.0f64; hops * channels];

        for frame in 0..hops * hop_frames {
            let hop = frame / hop_frames;
            for (channel, state) in states.iter_mut().enumerate() {
                let x = samples[frame * channels + channel] as f64;
                let y = weighting.apply(&mut state.0, &mut state.1, x);
                sums[hop * channels + channel] += y * y;
            }
        }

        Self {
            channels,
            hop_frames,
            hops,
            sums,
        }
    }

    /// Mean square summed over channels for one hop. BS.1770 channel weights are 1.0 for
    /// the mono/stereo signals this tool accepts.
    fn hop_power(&self, hop: usize) -> f64 {
        (0..self.channels)
            .map(|channel| self.sums[hop * self.channels + channel] / self.hop_frames as f64)
            .sum()
    }

    /// Mean square summed over channels across `start..end` hops.
    fn range_power(&self, start: usize, end: usize) -> f64 {
        if start >= end {
            return 0.0;
        }
        let total: f64 = (start..end).map(|hop| self.hop_power(hop)).sum();
        total / (end - start) as f64
    }

    /// Hop index for an interleaved frame position, clamped to the analyzed range.
    fn hop_at(&self, frame: usize) -> usize {
        (frame / self.hop_frames).min(self.hops)
    }

    /// ITU-R BS.1770-4 integrated loudness: 400 ms blocks with 75% overlap, the absolute
    /// -70 LUFS gate and the relative gate 10 LU below the gated mean.
    fn integrated_loudness(&self) -> f64 {
        const BLOCK_HOPS: usize = 4;
        let mut powers = Vec::new();
        for start in 0..self.hops.saturating_sub(BLOCK_HOPS - 1) {
            let power = self.range_power(start, start + BLOCK_HOPS);
            if loudness_of(power) > -70.0 {
                powers.push(power);
            }
        }
        if powers.is_empty() {
            return -70.0;
        }
        let ungated_mean = powers.iter().sum::<f64>() / powers.len() as f64;
        let relative_gate = loudness_of(ungated_mean) - 10.0;
        let gated: Vec<f64> = powers
            .into_iter()
            .filter(|power| loudness_of(*power) > relative_gate)
            .collect();
        if gated.is_empty() {
            return -70.0;
        }
        loudness_of(gated.iter().sum::<f64>() / gated.len() as f64)
    }

    /// EBU Tech 3342 loudness range from 3 s short-term windows (1 s hop) with the absolute
    /// gate and the 20 LU relative gate. Returns 0.0 when the file cannot produce two gated
    /// windows; callers must then not claim a measured range.
    fn loudness_range(&self) -> f64 {
        const WINDOW_HOPS: usize = 30;
        const STEP_HOPS: usize = 10;
        if self.hops < WINDOW_HOPS {
            return 0.0;
        }
        let mut loudnesses: Vec<f64> = (0..=self.hops - WINDOW_HOPS)
            .step_by(STEP_HOPS)
            .map(|start| loudness_of(self.range_power(start, start + WINDOW_HOPS)))
            .filter(|lufs| *lufs > -70.0)
            .collect();
        if loudnesses.len() < 2 {
            return 0.0;
        }
        let mean_power = loudnesses
            .iter()
            .map(|lufs| 10f64.powf((lufs + 0.691) / 10.0))
            .sum::<f64>()
            / loudnesses.len() as f64;
        let relative_gate = loudness_of(mean_power) - 20.0;
        loudnesses.retain(|lufs| *lufs > relative_gate);
        if loudnesses.len() < 2 {
            return 0.0;
        }
        loudnesses.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let percentile = |p: f64| {
            let index = ((loudnesses.len() - 1) as f64 * p).round() as usize;
            loudnesses[index]
        };
        (percentile(0.95) - percentile(0.10)).max(0.0)
    }
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
    loudness: &LoudnessHops,
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
        let rms_db = if rms > 1e-9 {
            20.0 * rms.log10()
        } else {
            -80.0
        };
        // K-weighted loudness of the section, not an unweighted RMS minus a constant.
        let start_hop = loudness.hop_at(start_frame / ch);
        let end_hop = loudness.hop_at(end_frame / ch).max(start_hop);
        let lufs = loudness_of(loudness.range_power(start_hop, end_hop));

        energies.push((rms_db, lufs));
    }

    let max_rms = energies.iter().map(|(r, _)| *r).fold(-100.0f64, f64::max);

    let labels = match section_count {
        3 => vec!["Intro", "Main Body", "Outro"],
        4 => vec!["Intro", "Verse / Build", "Chorus / Drop", "Outro"],
        _ => vec![
            "Intro",
            "Verse",
            "Pre-Chorus / Build",
            "Chorus / Drop",
            "Outro",
        ],
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
    let max_lag = (env_sr * 60.0 / 70.0) as usize; // 70 BPM

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
    let genre = genre_hint
        .unwrap_or("electronic / modern pop")
        .to_lowercase();
    let target_lufs = custom_target_lufs.unwrap_or_else(|| {
        if genre.contains("club") || genre.contains("edm") || genre.contains("dance") {
            -8.0
        } else if genre.contains("hip-hop") || genre.contains("trap") || genre.contains("rap") {
            -9.0
        } else if genre.contains("rock") || genre.contains("metal") {
            -10.0
        } else if genre.contains("acoustic")
            || genre.contains("jazz")
            || genre.contains("classical")
        {
            -16.0
        } else {
            -14.0 // Streaming target default
        }
    });

    let lufs_diff = round_2(receipt.integrated_lufs - target_lufs);

    let loudness_verdict = if lufs_diff.abs() <= 1.0 {
        format!(
            "Optimal loudness alignment: Track measures {:.1} LUFS (target {:.1} LUFS). Streaming normalization will apply near-zero gain adjustment.",
            receipt.integrated_lufs, target_lufs
        )
    } else if lufs_diff < -2.0 {
        format!(
            "Dynamic but quiet: Track measures {:.1} LUFS ({:.1} dB below the {:.1} LUFS genre target). Consider subtle buss compression and clean limiter gain.",
            receipt.integrated_lufs,
            lufs_diff.abs(),
            target_lufs
        )
    } else {
        format!(
            "Aggressively loud / pushed: Track measures {:.1} LUFS ({:.1} dB louder than target {:.1} LUFS). Major streaming services will attenuate this by {:.1} dB.",
            receipt.integrated_lufs, lufs_diff, target_lufs, lufs_diff
        )
    };

    let headroom_verdict = if receipt.clipping_detected || receipt.true_peak_db > 0.0 {
        format!(
            "CRITICAL: Inter-sample clipping detected at {:.2} dBTP! True peak exceeds 0.0 dBFS ceiling. Set master limiter ceiling to -0.5 dBTP or -1.0 dBTP to prevent lossy AAC/MP3 encoder distortion.",
            receipt.true_peak_db
        )
    } else if receipt.true_peak_db > -0.3 {
        format!(
            "Near-ceiling warning: True peak reaches {:.2} dBTP. Safe practice for streaming platforms is maintaining -0.5 to -1.0 dBTP headroom.",
            receipt.true_peak_db
        )
    } else {
        format!(
            "Healthy headroom: True peak sits at {:.2} dBTP with no inter-sample clipping detected.",
            receipt.true_peak_db
        )
    };

    // Dynamic contrast assessment
    let mut dynamic_contrast_verdict = if receipt.loudness_range_lra > 0.0 {
        format!(
            "Loudness Range (LRA) is {:.1} LU (EBU Tech 3342). ",
            receipt.loudness_range_lra
        )
    } else {
        "Loudness Range (LRA) not measurable: the file has too little gated 3 s material. "
            .to_string()
    };
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
    let mut mermaid =
        String::from("xychart-beta\n  title \"Arrangement Section Energy (LUFS)\"\n  x-axis [");
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
