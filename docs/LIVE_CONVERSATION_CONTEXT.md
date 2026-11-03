# Live conversation context

In Settings → Voice & Audio, choose **Enable listening context**, with everyone’s
consent, then start the microphone using the existing microphone control. Owner
transport authentication is required to enable it. It is off on every connection.
SAIL uses its existing local VAD and STT; no pyannote, speaker model, Hugging Face
account, voiceprint, new model download or cloud service is required.

Speech boundaries update temporary context without generating an answer, running
agent tools or speaking. Type a follow-up question, such as “What date did we agree
on?”, to use recent observations. Ordinary voice replies resume after disabling
listening context; disabling also clears the observations. Microphone permission
and the existing recording indicator remain in effect. This does not start hidden
microphone capture. Partial and final transcriptions are still visible.

Dialogue owns this session-only context. At most 32 segments and 8,000 UTF-8 text
bytes are kept, with a ten-minute per-segment expiry checked on access. Oldest
segments are evicted first. No speaker identity, overlap detection or diarization
is claimed; all voices remain unknown and transcription can be wrong. Simultaneous
speech may be mistranscribed. Expired bytes are lazily removed on the next context
operation; disabling, muting, reset and disconnect explicitly discard the buffer.
There is no cross-session recovery or durable ambient transcript store. An explicit
question and its answer follow the existing conversation/learning policy; an answer
may quote the observations. Disable learning if those answers should not persist.

Enabling/disabling flushes buffered audio, resets VAD and cancels in-flight turns.
Muting clears context and invalidates pending transcription. A generation guards
against late results repopulating cleared context. The context is supplied as
quoted untrusted background, never as a direct user request, verified owner speech,
or authorization. Guardian still controls tool effects requested by explicit turns.
Existing speech-worker scheduling protects live voice; missing STT reports an error
and does not fabricate an observation. No extra model runs per observation. Capture pauses while an explicit typed answer
is running so background speech cannot interrupt that answer or record its TTS.

This replaces PR #198’s uploaded-clip diarization/enrollment workflow and removes
its model downloader, optional pyannote dependencies and voiceprint persistence.
The existing supervised Python STT runtime remains; a Rust speech migration is
separate. Persisting useful knowledge from ambient speech is deliberately deferred
to Arena’s validated knowledge pipeline rather than automatically archiving it.

## Validation

- `cargo check -p sail-dialogue --offline`
- `cargo test -p sail-dialogue --lib live_context::tests`
- `cargo test -p sail-dialogue --lib live_conversation_tests -- --test-threads=1`
- `node --test crates/sail-dialogue/tests/voice_learning_ui_test.cjs crates/sail-dialogue/tests/live_context_ui_test.cjs`
- `python scripts/voice/live_context_browser_test.py` (existing Playwright environment)
- `cargo test -p sail-dialogue --test pipeline_test --test patient_turns --test staged_speech_orchestration_test --offline`
- `cargo fmt --all --check` and `git diff --check`

The production-turn test uses explicitly labelled fixture STT through the actual
speech worker bridge and a deterministic fixture host probe (production keeps the
Core shared admission broker). It also exercises real loopback WebSocket owner
authentication, reset and disabling, and decoder failure. It verifies observation
events, bounded context and absence
of assistant/tool/archive activity, not acoustic accuracy. Real microphone/STT
accuracy, end-to-end answer quality and Android capture still need target-device
validation; no accuracy, latency or model-resource measurements are claimed.
