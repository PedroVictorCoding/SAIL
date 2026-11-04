# SAIL DIALOGUE — Product and Technical Specification

**Status:** Proposed / evolving — verify implementation against code · **Domain:** Interaction & Presence · **Roadmap issue:** [#1](https://github.com/PedroVictorCoding/SAIL/issues/1) · **Priority:** P1 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Deliver continuous local low-latency voice conversation with robust interruption and multilingual speech.

## 2. Functional scope
- VAD.
- STT.
- multi-stage acknowledgments.
- streamed TTS.
- barge-in.
- speaker verification optional.
- PT-PT/English voice routing.
- audio focus and session lifecycle..

## 3. Ownership and non-goals
Does not itself select model hardware or create permanent user memories; Mind runs models, Arena stores memory and Guardian gates microphones.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Mind, Core audio bus, Relay, Vision, Arena, Guardian; concurrent tools #42.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Persistent Orb and voice-session card; explicit mic and recording state, language and listening/cancel controls.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 6a. On-device speech (decided 2026-11-03)

The speech chain (Silero VAD, Whisper base, Kokoro) can run in the user's browser, so the
host's CPU stays with the one resident language model ([Mind](SAIL_MIND.md#one-resident-model)).
It is the **same models** as the host workers, not a separate on-device assistant: the
conversation still goes to the host model. The host workers remain the fallback.

**Install.** `python3 scripts/voice/download_web.py` puts transformers.js 3.5.1 (bundles
ONNX Runtime Web), kokoro-js 1.2.1 and the ONNX models in `$SAIL_RUNTIME/models/voice/web`
(about 0.7 GB, every file pinned by revision and SHA-256). The server serves only the files
listed in its `manifest.json`, under `/voice-assets/`, refusing symlinks
(`crates/sail-dialogue/src/web_assets.rs`). The browser never contacts the internet: the
libraries' Hugging Face and CDN URLs are mapped to these files, and other requests to
those hosts are refused. Pages are cross-origin isolated so WebAssembly can use threads.

**Measured, per device.** On load the browser (`web/client-speech.js`, models in Web
Workers) speaks a short phrase with Kokoro and transcribes it back with Whisper. Each side
moves to the device only if it ran at most 0.5× real time there; otherwise it stays on
the host, and Settings → Voice & Audio → *Where Speech Runs* says which and why, with the
measured numbers. The user can choose *SAIL host* to keep everything on the host.
Uses WebGPU when the browser offers it, else WebAssembly.

**Protocol** (`/ws/voice`, all additive):

| Client → server | Meaning |
| --- | --- |
| `client_speech {stt, tts, engine}` | What this browser runs. Unreported parts stay on the host. |
| `speech_start` | Browser VAD heard speech: voice priority (preempts background work) and barge-in, as the host VAD's onset. |
| `voice_text {text}` | The transcribed turn. Learns only under the consent in force when capture began. |
| `speech_cancel` | VAD misfire or discarded capture: releases voice priority. |

With client TTS the server sends `speech_text {text, pause_ms, fragment}` in order instead
of `audio_chunk`; the browser synthesizes and plays it through the same playback queue, so
interruption and the Orb behave the same. With client STT no PCM leaves the device.

**Limits.** kokoro-js 1.2.1 speaks English only: with the Portuguese or Spanish voices,
speech output stays on the host. transformers.js Whisper does not identify the language by
itself, so the browser runs Whisper's language identification first, as faster-whisper
does on the host. Echo during playback is handled by the browser's echo cancellation, as
with host capture.

**Licenses.** transformers.js and kokoro-js: Apache-2.0. ONNX Runtime Web: MIT. Whisper
base (OpenAI weights, onnx-community export): Apache-2.0 card / MIT upstream. Kokoro-82M:
Apache-2.0. Silero VAD: MIT. Downloaded at setup, not vendored in the repository.

## 7. MVP vertical slice
One complete local voice turn with measured STT/first-audio latency, barge-in, no mic access when disabled and PT-PT/English text playback.

### Acceptance checklist
- [ ] Confirm existing source files, interfaces and dependency versions before changing architecture.
- [ ] Implement the minimal real end-to-end flow above, without mocks presented as production behavior.
- [ ] Test success, offline/unavailable, permission denial, cancellation and restart/reconnection as relevant.
- [ ] Record measured latency and resource use where relevant; avoid fabricated benchmarks or status.
- [ ] Show user-visible success/error through the mapped existing UI and preserve user control.
- [ ] Submit focused tests and a reviewable PR without automatic merge.

## 8. Next steps
1. Audit the relevant implementation and the linked roadmap issue; update this spec with confirmed module/file paths and contracts.
2. Build the MVP vertical slice with real tool/model/provider integrations.
3. Add security and regression tests before expanding capability breadth.
4. Record known limitations, operational instructions and migration compatibility.

## 9. Related references
- [Roadmap and subsystem taxonomy](https://github.com/PedroVictorCoding/SAIL/issues/15)
- [Documentation index](README.md)
- [SAIL Studio](SAIL_STUDIO.md) for persistent editable artifacts (distinct from Canvas)

## Live conversation context

Voice settings provide opt-in, owner-authenticated listening without replies using
the existing local VAD/STT. A bounded session-only buffer supplies anonymous speech
as untrusted background to explicit typed questions. No pyannote or voiceprints are
required. See [live conversation context](LIVE_CONVERSATION_CONTEXT.md) for consent,
clearing/expiry, usage, verification and limitations.

## In-browser terminal implementation

The existing terminal card uses locally served MIT-licensed xterm.js 5.5.0 and
FitAddon 0.10.0. The shell owns prompts, echo, editing, history, completion and
interrupt handling; the browser forwards keystrokes instead of keeping a second
command buffer. xterm handles cursor positioning, alternate screens, ANSI colors,
selection, paste and incremental UTF-8 decoding. Clear clears local scrollback
without injecting a command into a running application.

`/ws/terminal` carries JSON control messages and raw binary PTY output (a change
from JSON `output` messages; clients must consume binary frames). Exit follows
drained output and includes the shell's exit code. An intentional exit stays
closed until Restart; transport reconnection starts a **new** shell and does not
resume processes. Unix uses a PTY; the existing pipe fallback does not provide
TTY job control or full interactive-program compatibility. Session restart and
disconnection hang up the shell and foreground process group.

This improves the existing owner terminal; it does not grant agents shell access
or implement the broader Guardian-scoped tool execution contract above.
