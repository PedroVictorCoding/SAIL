# Local speaker intelligence — acceptance contract

User workflow: in Settings → Voice, explicitly submit a short mono 16 kHz PCM16
WAV clip. Analyze it into anonymous timed speakers; optionally identify enrolled
voices, verify a selected enrolled person, or consent to enrolling one clearly
single-speaker clip. List and delete profiles without loading a model.

Mind owns the bounded offline pyannote Community-1 worker; Dialogue owns the
owner-authenticated interaction; Arena owns encrypted enrolled-profile storage.
Existing Guardian owner authority gates all audio/profile operations. A voice
match never creates a capability, changes the authenticated owner, or authorizes
an action. No audio is sent to cloud inference or retained in the archive.

Inputs are bounded to 60 seconds, one concurrent analysis, at most 16 speakers
and 2,000 segments. Enrollment requires explicit consent and at least 3 seconds
of single-speaker, non-overlapping detected speech. Unknown and ambiguous matches
remain unknown; similarity is not a calibrated probability. Enrollment persists
only a named, model-bound voice embedding, encrypted using a domain-separated
key derived from the existing owner token. Token rotation requires reenrollment;
existing ciphertext can still be deleted. Backups retain their own lifecycle.

Verification covers typed validation, scores/unknowns, mixed-speaker rejection,
owner denial, cancellation/framing, unavailable worker, persistence/deletion and
the browser workflow. Real-model accuracy/latency requires installed gated assets
and representative consented recordings; fixture output must never be presented
as real diarization. No automatic background listening or changes to existing
live voice latency are in scope. Rust supervision reuses the current Python
speech-worker bridge; removing Python remains an explicit later migration.

## Installation and demo

1. Accept the [Community-1 model conditions](https://huggingface.co/pyannote/speaker-diarization-community-1)
   in your own Hugging Face account. SAIL never accepts terms or starts model downloads automatically. The pipeline model is CC-BY-4.0; pyannote.audio is MIT.
2. Create a separate Python environment on writable runtime storage, install a
   CPU-only PyTorch distribution using its official installer, then install
   `scripts/voice/requirements-speaker.txt` in that environment. Keep model and pip
   caches off the small root filesystem. This optional dependency does not change
   the working VAD/STT/TTS environment.
3. Use the setup downloader after your account has model access. The existing
   `.venv-voice` already includes `huggingface-hub`; this does not require installing
   the speaker/PyTorch runtime just to download weights:

   ```sh
   .venv-voice/bin/python scripts/voice/download_speaker.py --check-access
   .venv-voice/bin/python scripts/voice/download_speaker.py \
     --destination /path/to/writable/models/community-1 --prompt-token
   .venv-voice/bin/python scripts/voice/download_speaker.py \
     --destination /path/to/writable/models/community-1 --verify
   ```

   Replace the destination with a writable directory on runtime/data storage with
   enough free space. The host's `/srv/sail` may require service-user permissions;
   the helper does not use sudo or silently fall back to the nearly full root disk.
   `--prompt-token` reads a read-access token invisibly and does not save it. You
   may instead use an existing `hf auth login` session or `HF_TOKEN`; no token is
   accepted as a command-line argument. Tokens with write access are unnecessary.
   Access denial prints the model's acceptance/request-access link. A script cannot
   approve a gated account or bypass its license conditions.

   The helper pins a full commit SHA, reserves snapshot space plus 512 MiB headroom,
   resumes interrupted staging, checks file sizes and available LFS checksums, and
   writes a SHA-256 manifest. Only a complete verified directory is promoted.
   Existing unmanaged directories are refused. `--revision <full-commit-sha>` pins
   a specific release; `--verify` works offline without importing the Hub SDK.
   Installing the optional speaker runtime remains step 2.

   Configure the SAIL service with the downloaded path and that runtime:

   ```sh
   SAIL_SPEAKER_PYTHON=/path/to/speaker-environment/bin/python
   SAIL_SPEAKER_MODEL_DIR=/path/to/writable/models/community-1
   ```

4. Rebuild/restart SAIL through the normal deployment workflow. Open Settings →
   Voice & Audio → Speaker timelines and enrolled voices. Choose a consented WAV
   clip and **Analyze speakers**. Initially labels remain anonymous/unknown.
5. Choose a single-speaker clip, enter a name, check consent and enroll. Analyze a
   second sample to identify enrolled voices, or use **Verify selected clip** for
   one-to-one comparison against that profile. **Delete voice profile** removes
   future matching, including after a restart.

The API uses the existing owner bearer token: `POST /api/speakers/request` with
`action` = `list`, `analyze`, `enroll`, `verify` or `delete`. Audio is base64
little-endian PCM16 at 16 kHz; input/sample/duration bounds are enforced again in
Rust. Responses contain segments and advisory matches, never voice embeddings.
Enrollment request UUIDs cannot overwrite an existing profile. Deletion increments
a generation to reject in-flight stale enrollments. All responses use `no-store`.
A cancelled browser request may already have committed; refresh the canonical list.

Identification uses cosine similarity >= 0.80 and a top-two margin >= 0.08;
verification compares just the claimed profile at the same threshold. These are
conservative initial operating values, **not calibrated accuracy guarantees**.
Different model asset/library fingerprints cannot match one another. Validate
thresholds with consented held-out recordings and unknown speakers on the target
microphone before relying on labels. Similarity is not a probability.

The Mind worker is CPU-only with two threads, a 3 GiB startup RAM reservation
(estimate, not a measurement), one concurrent request and a 120-second deadline.
Core admission rejects pressure and cancels this background speech lease for live
voice; Mind kills the child on preemption/error and after each successful clip to
release residency. Offline flags and disabled pyannote telemetry are set before
imports. There is no cloud API adapter. The Rust-only target remains a later model
runtime migration; this implementation uses the already established supervised
Python bridge.

## Verification and limits

- `cargo test -p sail-arena -p sail-protocol`
- `cargo test -p sail-dialogue --test speakers_test`
- `cargo test -p sail-dialogue --lib speakers::tests`
- `cargo test -p sail-common --test resource_admission_test`
- `python scripts/voice/speaker_worker_test.py`
- `python scripts/voice/download_speaker_test.py` (fixture-only downloads)
- With the existing Playwright test environment:
  `python scripts/voice/speakers_browser_test.py`
- `cargo check -p sail-dialogue`, `cargo fmt --all --check`, `git diff --check`

The HTTP workflow and browser tests explicitly use fixture inference. They prove
request/state/privacy behavior, not acoustic accuracy. Real Community-1 inference
was **not run** on the development host: pyannote/PyTorch and accepted gated model
assets are absent. No latency, RAM measurement, diarization error rate or speaker
recognition accuracy is claimed. A real consented multi-speaker recording and
held-out enrollment samples remain the acoustic acceptance gate.

Background/live microphone labeling, transcription alignment, Android runtime
validation, automatic person-memory linking and stronger authentication are not
included in this clip-analysis slice. Neither a model-generated name nor a voice
match substitutes for owner authentication or Guardian approval. SQLite deletion
is application-level removal, not forensic erasure of encrypted backups/WALs.

References: [conceptual feature article](https://huggingface.co/blog/pyannote/speaker-diarization-recognition-and-identification),
[local pipeline and offline setup](https://huggingface.co/pyannote/speaker-diarization-community-1),
[upstream output/embedding contract](https://github.com/pyannote/pyannote-audio/blob/main/src/pyannote/audio/pipelines/speaker_diarization.py).
