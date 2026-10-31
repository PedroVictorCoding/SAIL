use sail_dialogue::{
    RealtimeVoicePipeline, SileroVad, SileroVadConfig, TurnDetectionSettings, TurnMode, VadEvent,
};
#[test]
fn a_thinking_pause_and_resumed_speech_are_one_automatic_turn() {
    let mut vad = SileroVad::new(SileroVadConfig {
        turn_mode: TurnMode::Automatic,
        min_silence_duration_ms: 3000,
        ..Default::default()
    });
    for _ in 0..8 {
        assert!(vad.process_probability(&[100; 512], 0.9).1.is_none());
    }
    // Over two seconds of thinking must not send anything to the assistant.
    for _ in 0..64 {
        let (event, audio, _) = vad.process_probability(&[0; 512], 0.);
        assert_ne!(event, VadEvent::SpeechEnd);
        assert!(audio.is_none());
    }
    for _ in 0..8 {
        let (event, audio, _) = vad.process_probability(&[200; 512], 0.9);
        assert_eq!(event, VadEvent::InSpeech);
        assert!(audio.is_none());
    }
    let mut completed = Vec::new();
    for _ in 0..100 {
        if let Some(audio) = vad.process_probability(&[0; 512], 0.).1 {
            completed.push(audio);
        }
    }
    assert_eq!(completed.len(), 1);
    let audio = &completed[0];
    assert_eq!(&audio[..8 * 512], &vec![100; 8 * 512]);
    assert_eq!(&audio[72 * 512..80 * 512], &vec![200; 8 * 512]);
    assert!(vad.finish().is_none());
}
#[test]
fn manual_mode_keeps_long_pauses_and_sends_once_on_finish() {
    let mut vad = SileroVad::new(SileroVadConfig {
        turn_mode: TurnMode::Manual,
        ..Default::default()
    });
    for _ in 0..8 {
        vad.process_probability(&[100; 512], 0.9);
    }
    for _ in 0..400 {
        assert!(vad.process_probability(&[0; 512], 0.).1.is_none());
    }
    for _ in 0..8 {
        let (event, audio, _) = vad.process_probability(&[200; 512], 0.9);
        assert_eq!(event, VadEvent::InSpeech);
        assert!(audio.is_none());
    }
    let audio = vad.finish().unwrap();
    assert_eq!(audio.len(), 416 * 512);
    assert_eq!(&audio[..8 * 512], &vec![100; 8 * 512]);
    assert_eq!(&audio[audio.len() - 8 * 512..], &vec![200; 8 * 512]);
    assert!(vad.finish().is_none());
}
#[test]
fn continuous_speech_longer_than_thirty_seconds_is_not_split() {
    let mut vad = SileroVad::new(SileroVadConfig {
        turn_mode: TurnMode::Manual,
        ..Default::default()
    });
    for _ in 0..1100 {
        let (event, audio, _) = vad.process_probability(&[100; 512], 0.9);
        assert_ne!(event, VadEvent::SpeechEnd);
        assert_ne!(event, VadEvent::CaptureLimitReached);
        assert!(audio.is_none());
    }
    // The full recording survives even when expensive partial previews stop.
    assert!(vad.snapshot().is_none());
    assert_eq!(vad.finish().unwrap().len(), 1100 * 512);
}
#[test]
fn settings_changes_preserve_buffer_and_invalid_settings_leave_control_unchanged() {
    let mut vad = SileroVad::new(SileroVadConfig {
        turn_mode: TurnMode::Automatic,
        ..Default::default()
    });
    for _ in 0..8 {
        vad.process_probability(&[100; 512], 0.9);
    }
    vad.set_turn_detection(TurnDetectionSettings {
        mode: TurnMode::Manual,
        pause_ms: 3000,
    });
    for _ in 0..100 {
        assert!(vad.process_probability(&[0; 512], 0.).1.is_none());
    }
    assert_eq!(
        vad.finish()
            .unwrap()
            .iter()
            .filter(|sample| **sample == 100)
            .count(),
        8 * 512
    );
    let mut pipe = RealtimeVoicePipeline::new();
    let previous = pipe.control_state().turn_detection;
    assert!(
        pipe.set_turn_detection(TurnDetectionSettings {
            mode: TurnMode::Manual,
            pause_ms: 0
        })
        .is_err()
    );
    assert_eq!(pipe.control_state().turn_detection, previous);
    pipe.set_turn_detection(TurnDetectionSettings {
        mode: TurnMode::Manual,
        pause_ms: 10000,
    })
    .unwrap();
    assert_eq!(pipe.control_state().turn_detection.mode, TurnMode::Manual);
}
