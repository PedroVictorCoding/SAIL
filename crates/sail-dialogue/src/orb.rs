use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrbState {
    Idle,
    Listening,
    Thinking,
    Speaking,
    Interrupted,
}

impl std::fmt::Display for OrbState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                OrbState::Idle => "idle",
                OrbState::Listening => "listening",
                OrbState::Thinking => "thinking",
                OrbState::Speaking => "speaking",
                OrbState::Interrupted => "interrupted",
            }
        )
    }
}

/// 8-band audio spectrum telemetry matching standard acoustic bands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSpectrum8Band {
    pub sub_bass: f32,   // 20 - 60 Hz
    pub bass: f32,       // 60 - 250 Hz
    pub low_mid: f32,    // 250 - 500 Hz
    pub mid: f32,        // 500 - 2000 Hz
    pub high_mid: f32,   // 2000 - 4000 Hz
    pub presence: f32,   // 4000 - 6000 Hz
    pub brilliance: f32, // 6000 - 12000 Hz
    pub air: f32,        // 12000 - 20000 Hz
}

impl Default for AudioSpectrum8Band {
    fn default() -> Self {
        Self {
            sub_bass: 0.0,
            bass: 0.0,
            low_mid: 0.0,
            mid: 0.0,
            high_mid: 0.0,
            presence: 0.0,
            brilliance: 0.0,
            air: 0.0,
        }
    }
}

/// Real-time Voice Orb visualizer telemetry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbTelemetry {
    pub state: OrbState,
    pub rms_level: f32,               // 0.0 to 1.0 (power level)
    pub wave_phase: f32,              // 0.0 to 2*PI (instantaneous sinusoidal phase)
    pub spectrum: AudioSpectrum8Band, // 8-band audio spectrum analyzer
    pub turn_speaker: String,         // "user" or "assistant"
    pub transcript: String,           // Live transcript text
    pub latency_ms: u64,              // Turn latency
}

pub struct OrbEngine {
    current_state: OrbState,
    current_transcript: String,
    accumulated_phase: f32,
    active_speaker: String,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
}

impl OrbEngine {
    pub fn new() -> Self {
        Self {
            current_state: OrbState::Idle,
            current_transcript: String::new(),
            accumulated_phase: 0.0,
            active_speaker: "none".to_string(),
            fft: rustfft::FftPlanner::new().plan_fft_forward(1024),
        }
    }

    pub fn set_state(&mut self, state: OrbState) {
        self.current_state = state;
    }

    pub fn state(&self) -> OrbState {
        self.current_state
    }

    pub fn set_transcript(&mut self, transcript: impl Into<String>) {
        self.current_transcript = transcript.into();
    }

    pub fn set_speaker(&mut self, speaker: impl Into<String>) {
        self.active_speaker = speaker.into();
    }

    /// Compute real-time Orb visualizer telemetry from incoming audio chunk
    pub fn compute_telemetry(&mut self, samples: &[i16], sample_rate: u32) -> OrbTelemetry {
        // Advance sinusoidal phase based on sample duration
        let duration_secs = samples.len() as f32 / sample_rate.max(1) as f32;
        self.accumulated_phase =
            (self.accumulated_phase + duration_secs * 2.0 * PI * 1.5) % (2.0 * PI);

        if samples.is_empty() {
            return OrbTelemetry {
                state: self.current_state,
                rms_level: 0.0,
                wave_phase: self.accumulated_phase,
                spectrum: AudioSpectrum8Band::default(),
                turn_speaker: self.active_speaker.clone(),
                transcript: self.current_transcript.clone(),
                latency_ms: 0,
            };
        }

        // 1. RMS level calculation
        let mut sum_squares = 0.0f64;
        for &s in samples {
            let v = s as f64 / 32768.0;
            sum_squares += v * v;
        }
        let rms = ((sum_squares / samples.len() as f64).sqrt() as f32).clamp(0.0, 1.0);

        // 2. 8-Band spectral energy calculation
        let spectrum = self.calculate_8_band_spectrum(samples, sample_rate);

        OrbTelemetry {
            state: self.current_state,
            rms_level: rms,
            wave_phase: self.accumulated_phase,
            spectrum,
            turn_speaker: self.active_speaker.clone(),
            transcript: self.current_transcript.clone(),
            latency_ms: 0,
        }
    }

    /// Calculate 8-band audio spectrum from time-domain samples
    fn calculate_8_band_spectrum(&self, samples: &[i16], sample_rate: u32) -> AudioSpectrum8Band {
        let n = samples.len().min(1024);
        if n < 8 {
            return AudioSpectrum8Band::default();
        }
        let boundaries = [20., 60., 250., 500., 2000., 4000., 6000., 12000., 20000.];
        let mut energy = [0.0f32; 8];
        let mut bins = vec![rustfft::num_complex::Complex::new(0., 0.); 1024];
        for (i, sample) in samples[..n].iter().enumerate() {
            let window = 0.5 - 0.5 * (2. * PI * i as f32 / (n - 1) as f32).cos();
            bins[i].re = *sample as f32 / 32768. * window;
        }
        self.fft.process(&mut bins);
        for (k, bin) in bins.iter().enumerate().take(513).skip(1) {
            let hz = k as f32 * sample_rate as f32 / 1024.;
            if let Some(band) = (0..8).find(|&b| hz >= boundaries[b] && hz < boundaries[b + 1]) {
                energy[band] += bin.norm_sqr() * 8. / (n * 1024) as f32;
            }
        }
        let e = energy.map(|v| v.sqrt().clamp(0., 1.));
        AudioSpectrum8Band {
            sub_bass: e[0],
            bass: e[1],
            low_mid: e[2],
            mid: e[3],
            high_mid: e[4],
            presence: e[5],
            brilliance: e[6],
            air: e[7],
        }
    }
}
impl Default for OrbEngine {
    fn default() -> Self {
        Self::new()
    }
}
