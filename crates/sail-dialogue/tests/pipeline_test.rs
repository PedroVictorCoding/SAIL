use sail_common::types::SubsystemId;
use sail_dialogue::{
    HardwareTierProfile, KernelToolCall, LocalKernelToolBroker, OrbEngine, SileroVad,
    SileroVadConfig, VadEvent, VoiceControl,
};
#[test]
fn vad_requires_contiguous_speech_and_preserves_onset_without_duplication() {
    let config = SileroVadConfig {
        turn_mode: sail_dialogue::TurnMode::Automatic,
        min_speech_duration_ms: 64,
        min_silence_duration_ms: 64,
        speech_pad_ms: 32,
        ..Default::default()
    };
    let mut vad = SileroVad::new(config);
    assert_eq!(vad.process_probability(&[1; 512], 0.).0, VadEvent::Silence);
    assert_eq!(vad.process_probability(&[2; 512], 0.9).0, VadEvent::Silence);
    // Candidate speech interrupted by silence must not accumulate into a turn.
    assert_eq!(vad.process_probability(&[3; 512], 0.).0, VadEvent::Silence);
    assert_eq!(vad.process_probability(&[4; 512], 0.9).0, VadEvent::Silence);
    assert_eq!(
        vad.process_probability(&[5; 512], 0.9).0,
        VadEvent::SpeechStart
    );
    assert_eq!(vad.process_probability(&[0; 512], 0.).0, VadEvent::InSpeech);
    let (event, audio, _) = vad.process_probability(&[0; 512], 0.);
    assert_eq!(event, VadEvent::SpeechEnd);
    let audio = audio.unwrap();
    assert_eq!(audio.len(), 4 * 512);
    assert_eq!(&audio[..512], &[3; 512]);
    assert_eq!(&audio[512..1024], &[4; 512]);
    assert_eq!(&audio[1024..1536], &[5; 512]);
    assert!(vad.snapshot().is_none());
    assert!(vad.finish().is_none());
}
#[test]
fn vad_force_end_and_recording_limit_hold_audio_until_explicit_send() {
    let mut vad = SileroVad::new(SileroVadConfig {
        max_turn_duration_ms: 1000,
        ..Default::default()
    });
    for _ in 0..4 {
        vad.process_probability(&[100; 512], 0.9);
    }
    assert!(vad.finish().is_some());
    assert!(vad.finish().is_none());
    let mut limit_events = 0;
    for _ in 0..100 {
        let (event, audio, telemetry) = vad.process_probability(&[100; 512], 0.9);
        assert!(
            audio.is_none(),
            "buffer limits must never split and submit a turn"
        );
        assert!(telemetry.captured_ms <= 1000);
        if event == VadEvent::CaptureLimitReached {
            limit_events += 1;
        }
    }
    assert_eq!(limit_events, 1);
    assert!(vad.snapshot().is_none());
    assert_eq!(vad.finish().unwrap().len(), 16000);
    assert!(vad.finish().is_none());
}
#[test]
fn fft_separates_frequency_bands_and_respects_nyquist() {
    let mut orb = OrbEngine::new();
    let samples: Vec<_> = (0..1024)
        .map(|i| (12000. * (2. * std::f32::consts::PI * 1000. * i as f32 / 16000.).sin()) as i16)
        .collect();
    let telemetry = orb.compute_telemetry(&samples, 16000);
    assert!(telemetry.spectrum.mid > 0.2);
    assert!(telemetry.spectrum.bass < 0.001);
    assert_eq!(telemetry.spectrum.air, 0.);
    assert!((telemetry.rms_level - 0.259).abs() < 0.01);
    let silence = orb.compute_telemetry(&[0; 512], 16000);
    assert_eq!(silence.rms_level, 0.);
    assert_eq!(silence.spectrum.mid, 0.);
}
#[test]
fn profiles_preserve_wire_rate_and_apply_thread_budget() {
    let mut control = VoiceControl::new();
    for (profile, threads) in [
        (HardwareTierProfile::LowPower, 2),
        (HardwareTierProfile::Balanced, 4),
        (HardwareTierProfile::HighPerformance, 8),
    ] {
        control.set_profile(profile);
        assert_eq!(profile.target_sample_rate(), 16000);
        assert_eq!(profile.thread_count(), threads);
    }
    assert!(control.toggle_mute());
    assert!(!control.toggle_mute());
    assert_eq!(control.advance_turn(), 1);
}
#[test]
fn kernel_tools_validate_real_pixels_and_reject_workspace_escape() {
    let guardian = sail_guardian::GuardianSubsystem::new();
    assert!(
        LocalKernelToolBroker::execute(&KernelToolCall {
            tool: "os:status".into(),
            params: serde_json::json!({})
        })
        .success
    );
    let process_call = KernelToolCall {
        tool: "proc:list".into(),
        params: serde_json::json!({"limit":3}),
    };
    let denied = LocalKernelToolBroker::execute(&process_call);
    assert!(!denied.success);
    assert_eq!(denied.output["error_code"], "permission_denied");
    let approval = guardian.approval();
    let pending = approval.list_pending();
    assert_eq!(pending.len(), 1);
    let token = approval
        .resolve_session(pending[0].session_id, true)
        .unwrap();
    let processes = LocalKernelToolBroker::execute_guarded(
        SubsystemId::Dialogue,
        &process_call,
        Some(token.token_id),
    );
    assert!(processes.success);
    assert!(processes.output["count"].as_u64().unwrap() <= 3);
    for path in ["/etc/passwd", "../../../../etc/passwd"] {
        assert!(
            !LocalKernelToolBroker::execute(&KernelToolCall {
                tool: "fs:query".into(),
                params: serde_json::json!({"path":path})
            })
            .success
        );
    }
    let valid = LocalKernelToolBroker::execute(&KernelToolCall {
        tool: "vision:frame".into(),
        params: serde_json::json!({"width":1,"height":1,"format":"rgb","data":[255,255,255]}),
    });
    assert!(valid.success);
    assert_eq!(valid.output["average_luminance"], 1.0);
    let invalid = LocalKernelToolBroker::execute(&KernelToolCall {
        tool: "vision:frame".into(),
        params: serde_json::json!({"width":100,"height":1,"format":"rgb","data":[255]}),
    });
    assert!(!invalid.success);
    assert!(sail_agents::authorize_voice_operation("shell:exec").is_err());
}
#[test]
fn arena_context_never_crosses_sessions() {
    let arena = sail_arena::ArenaSubsystem::new();
    arena.remember_voice_turn("alice", 1, "Alice likes bicycles".into());
    arena.remember_voice_turn("bob", 1, "Bob likes bicycles".into());
    let context = arena.retrieve_voice_context("alice", "bicycles", 3);
    assert_eq!(context, vec!["Alice likes bicycles"]);
}
#[test]
fn canvas_extracts_mermaid_and_json() {
    let artifacts = sail_canvas::voice_artifacts(
        "Diagram:\n```mermaid\ngraph LR; A-->B\n```\nData:\n```json\n{\"value\":1}\n```",
    );
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0].artifact_type, "mermaid");
    assert_eq!(artifacts[1].artifact_type, "json");
}
#[test]
fn plain_dialogue_never_creates_canvas_artifacts() {
    let artifacts = sail_canvas::voice_artifacts(
        "I have checked the system status. Everything is running smoothly and memory is within limits.",
    );
    assert!(artifacts.is_empty());
}
