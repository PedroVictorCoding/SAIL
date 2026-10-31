# Using SAIL

## Recommended experience: talk to SAIL on your own computer

Run SAIL on the same computer as your browser and open **http://localhost:8088**. Open **Settings** to choose a local voice and compute tier. Keep one browser tab open for the conversation. This is the simplest everyday setup: no TLS configuration, cloud account, or external inference API is needed.

Speak naturally, pause to finish a turn, and listen to the reply while following the transcript and Canvas. Use text when you want precise wording, attach an image when you want visual reasoning, and interrupt whenever you want to change direction.

## Start a session

Run these commands from the repository root. In the current workspace:

```bash
cd /home/main/SAIL
cargo run -p sail-core
```

Keep the terminal open while using SAIL. If an older SAIL process is already running, stop it with **Ctrl+C in its terminal** before starting the new version.

1. Open **http://localhost:8088** on this computer.
2. Wait for the page to show that it is connected.
3. Open **Settings** and choose **Check local model status**. The first check loads the speech workers and may take longer than subsequent checks.
4. Confirm that the speech models and local LLM report `ready: true`. Image reasoning also requires the LLM's `vision` modality to be enabled.
5. Choose a voice: **English · Heart**, **Português · Dora**, **Português · Alex**, or **Español · Dora**.
6. Click **Start Microphone** and allow microphone access in the browser.
7. Speak your request, then pause for about 3 seconds. Shorter thinking pauses stay in the same turn. Choose Manual turn taking in Settings if you want to decide when to send.

For example:

> Olá SAIL. Responde em português e explica brevemente como funciona o teu pipeline de voz.

The voice selector changes synthesized speech; ask for your preferred response language in the conversation as well. Headphones help prevent the assistant's audio from being picked up by the microphone.

## Everyday controls

| Control | When to use it | What happens |
|---|---|---|
| **Microphone button** | Begin or finish hands-free input | Starts capture; clicking it again flushes the final microphone frame, stops capture, and submits the current turn. An on-screen indicator shows when the microphone is active. |
| **Mute button** | Temporarily stop sending your voice | Pauses sending audio and retains your unfinished recording. Playback continues; the microphone button stops capture and sends the turn. |
| **Interrupt button** | Stop a reply or change direction | Appears during a response and cancels generation and queued playback. |
| **Send turn button** | You have finished speaking and want to send now | Flushes the final microphone frame and submits the entire active recording without waiting for silence. |
| **Settings → Compute profile** | Adjust the CPU budget | Cycles through Balanced, HighPerformance and LowPower. |
| **Settings → Start a new conversation** | Start a different topic without earlier context | Cancels the current reply and clears this session's conversation and Canvas. |
| **Spaces** | Open a subsystem | Dialogue, Canvas and Vision actions are connected; other spaces show Not connected until their runtime route is available. |
| **Settings → Check local model status** | Confirm readiness or investigate a model error | Checks speech worker readiness and local LLM capabilities. |

Speaking while SAIL is replying also triggers automatic interruption once speech is detected. For immediate, predictable cancellation, use **Interrupt**.

### Speak continuously and take thinking pauses

In **Settings → Turn taking**, choose:

- **Automatic:** waits for **3 seconds of quiet** by default. Adjust **Thinking pause** between 0.5 and 10 seconds. Speech resumed before that timeout remains part of the same message.
- **Manual · Send turn:** pauses never submit the recording. Keep speaking and thinking, then press **Send turn**, or click the microphone button to stop and send. Final STT receives the complete recording in one request; partial captions do not trigger replies. Mute can pause input without discarding what you have already said.

The old automatic 30-second split is removed. A turn can now hold up to **3 minutes** (about 5.8 MB of 16 kHz PCM). At that limit capture stops and the recording is **held**, not automatically sent or split. Press **Send turn** to submit it; **Start a new conversation** discards it. The **Recorded** counter shows the buffer duration. Very long recordings may take longer to transcribe, and live partial previews stop after 30 seconds to avoid repeatedly decoding an ever-growing recording; the final transcription still uses all recorded audio.

Settings apply to the current connection and keep already captured audio. A service can set defaults with `SAIL_VOICE_TURN_MODE=manual` (or `automatic`) and `SAIL_VOICE_PAUSE_MS=3000`. Restart the service and refresh the page after updating.

### Choose a hardware tier

| Tier | Recommended use | STT/TTS CPU threads | Partial transcript cadence |
|---|---|---:|---:|
| **Balanced** | Default everyday conversation | 4 | About 1 second |
| **LowPower** | Leave more CPU available for other work | 2 | About 2 seconds |
| **HighPerformance** | Give speech processing a larger CPU budget | 8 | About 0.77 seconds |

Higher thread counts do not guarantee lower latency on every machine. These tiers affect speech workers and partial transcription, not the Gemma model's thread setting. Microphone transport stays at 16kHz in every tier. A tier change takes effect on the next speech inference and may reload a worker's model session.

## Useful things to ask

### Have a conversation

Ask a question, then refer to the answer in the same tab:

> Explain the voice pipeline in three sentences.
>
> Which stage decides when I have finished speaking?
>
> Now explain that in Portuguese.

Partial transcripts can change as Whisper hears more audio. The final transcript is the one used for the assistant's turn.

### Inspect the local system

Ask SAIL to use a real local tool rather than estimate an observation:

> Use os_status and tell me the current memory usage.
>
> Use proc_list to show five running processes.
>
> Use fs_query to list the repository's top-level folders.

The assistant presents local tool results as a short summary. Open **Show details** on a result card to inspect the raw payload.

Voice tools currently provide read-only inspection. They do not execute arbitrary shell commands, edit files, or install software. Filesystem inspection is limited to the configured workspace root.

Process inspection, filesystem queries and image tools now require an exact, short-lived owner approval. Configure a securely generated `SAIL_APPROVAL_TOKEN` (at least 32 bytes) in the server's protected environment before startup. In **Settings → Owner approvals**, enter the same token, review the actor, exact resource and expiry, then choose **Approve and retry once**. The token is not saved in browser storage. If owner authentication is not configured, these operations fail closed; ordinary conversation and low-risk OS telemetry continue.

A remembered process/file request can be retried in its original conversation. A new question or another browser connection cannot reuse its grant. The retry returns the local tool result; automatic continuation of the earlier model reasoning is not yet implemented. **Deny / revoke** invalidates the request and any issued token. For image tools, use the Local tools card to keep the selected frame associated with its request.

`SAIL_TOOL_ROOT` sets the permitted data workspace at startup. Symlinks, hardlinked files, special files, cross-mount paths and private stores inside the root are rejected. `SAIL_VAULT_DIR` optionally enables the encrypted credential vault; it must be a private owner-owned directory and an invalid configured vault stops startup. See [Guardian execution](Docs/ARCHITECTURE/Guardian_Execution.md) for deployment and limitations of the separately provisioned command service.


### Ask for a diagram or structured explanation

> Show the voice pipeline as a Mermaid diagram, then briefly explain each stage.
>
> Return a JSON object comparing the three hardware tiers.

SAIL streams its response into the conversation. Fenced Mermaid diagrams and JSON blocks appear in the Canvas alongside it. If a diagram is invalid, the panel shows its source so you can ask SAIL to correct it.

### Discuss an image

1. Choose **＋** in the composer and select a **PNG or JPEG up to 4 MB**, or open **Spaces → Vision** and allow the browser camera permission for a local live preview.
2. Type a specific question, such as “Describe the main elements of this diagram.”
3. Send your question, or choose **Capture frame** in the camera preview.

The image is shown in Canvas and sent to the local Gemma vision service with your question. Camera access begins only after choosing Vision, shows an active indicator and preview, and can be stopped from that preview. An image attachment is cleared after sending; attach it again for a follow-up that requires examining its pixels.

For measured image statistics, select an image and ask SAIL to inspect the frame. This reports dimensions, luminance and edge density. Use a conversational image question for semantic reasoning.

### Use text without the microphone

Type in the message field and click **Send**. This uses the same session, local Gemma inference, speech output and Canvas. Microphone permission is not required. Sending a new message cancels the previous assistant turn.

## Hear the reply before all the text is finished

SAIL starts synthesizing the first complete sentence while Gemma generates the rest. Longer sentences can be committed at a natural clause boundary. Punctuation, questions, quoted endings, common abbreviations and decimal numbers are kept together so speech is not cut at arbitrary model-token boundaries. Kokoro keeps the selected voice and speed, and short pauses preserve the cadence between sentences and clauses.

Kokoro is pre-warmed when the browser connects. A short, single-sentence answer may still finish generating before its first audio is ready; longer answers can overlap generation and playback. This remains local sentence/phrase synthesis, not a neural decoder that speaks unfinished words. Independently synthesized sentences can differ from whole-paragraph prosody, so lower latency does not guarantee identical intonation across the entire answer.

The runner logs the first audio's readiness time as `first_audio_ms`, separately from model completion and total turn time. Use those measurements to distinguish model thinking/prefill time from speech synthesis delay.

## Understand the Orb and session

The Orb indicates **idle**, **listening**, **thinking**, **speaking** or **interrupted**. The meters show audio level, speech confidence and measured VAD timing. VAD inference time and worker round-trip time are separate measurements; neither is the total time needed to produce a reply.

Each browser connection has its own session. Keep the same tab and connection for follow-up questions. Reloading the page, reconnecting after a disconnect, or opening another tab creates a new session. Voice history is kept in memory, is bounded, and is cleared when its session disconnects or resets. It is not a permanent personal memory store.

After the initial package and model downloads, the voice pipeline runs locally: **Silero → Whisper → Session → Gemma → Kokoro → Orb**. Speech models run in local Python workers managed by Mind; Rust coordinates the session, tools, streaming and controls.

## First-time setup on another installation

The current workspace already has the speech environment and downloaded weights. On a fresh installation, prepare:

- A Rust toolchain compatible with the workspace and Python 3.11 with virtual-environment support.
- A compatible local `llama-server` binary.
- The Gemma weights at `models/gemma4-v2-Q4_K_M.gguf`. The speech setup script does not download these main LLM weights.

From the repository root, run:

```bash
./scripts/voice/setup.sh
```

This installs speech dependencies into `.venv-voice` and downloads Silero, Whisper, Kokoro and the Gemma vision projector into `models/voice`. Internet access is needed for setup. Missing runtime models produce errors rather than being downloaded automatically.

The default llama-server binary is `/home/main/llama.cpp/build/bin/llama-server`. For a different installation:

```bash
SAIL_LLAMA_SERVER=/absolute/path/to/llama-server cargo run -p sail-core
```

Mind starts its managed model service on `127.0.0.1:8081`. To use an existing compatible local server instead:

```bash
SAIL_LLM_URL=http://127.0.0.1:8080/v1/chat/completions cargo run -p sail-core
```

That server must support streamed chat and tool calls, and must load a compatible vision projector for image questions. Remote inference endpoints are not supported by the voice path. Full configuration is documented in the [voice architecture guide](Docs/ARCHITECTURE/Realtime_Voice_Pipeline.md).

## Optional: use a phone or another computer on your LAN

Use this setup only when the browser is on a different device. On the SAIL computer itself, continue using **http://localhost:8088** without TLS.

A browser opened at a LAN IP needs trusted HTTPS for microphone capture. In addition, SAIL refuses a plain HTTP listener on a non-loopback address.

For the example server address `192.168.1.101`:

```bash
# Generate once, or regenerate if the server IP changes or the certificate expires.
./scripts/voice/tls.sh 192.168.1.101
```

Trust the public certificate `.local/tls/voice-cert.pem` on the client device using its operating system or browser certificate settings. Keep `.local/tls/voice-key.pem` on the server. If you already have a trusted certificate covering the server address, use its certificate and key instead.

Stop the existing SAIL runtime, then start HTTPS:

```bash
SAIL_VOICE_BIND=0.0.0.0:8088 \
SAIL_TLS_CERT=.local/tls/voice-cert.pem \
SAIL_TLS_KEY=.local/tls/voice-key.pem \
cargo run -p sail-core
```

Open **https://192.168.1.101:8088** on the other device, then follow the same session steps as above. Replace the example IP with the actual server address. Simply bypassing a certificate warning may not enable microphone access.

The generated certificate lasts 365 days. Regenerating it also replaces its key, so clients must trust the replacement certificate. This listener has no user login; use it only on a trusted network and do not forward it to the public internet.

## Follow what the runner is doing

The runner prints detailed diagnostics by default and appends the same events to **`.local/runner.log`**. Each voice session and assistant turn has an ID, so you can follow a request through context retrieval, Gemma inference, tool authorization/execution, speech synthesis, Canvas delivery and cancellation. Logs include prompts, transcripts, bounded tool arguments/results, worker startup/output, timings and errors. Audio and image payloads are omitted.

```bash
# Watch the log while SAIL is running in another terminal.
tail -f .local/runner.log
```

For every model text delta and processed audio frame, enable trace output:

```bash
RUST_LOG=warn,sail_core=debug,sail_common=debug,sail_dialogue=trace,sail_mind=trace \
cargo run -p sail-core
```

Keep your usual TLS variables if starting in HTTPS mode. `RUST_LOG` overrides the default logging filter; use `RUST_LOG=info` for quieter output. When SAIL starts llama-server itself, its native output is forwarded into runner diagnostics and also appended to `.local/llama-server.log`. An already-running model server retains its own process output.

A question such as “What tools do you have?” is answered without executing host tools. A denied or failed query switches the model to a final explanation. Repeated identical tool calls reuse the prior result and do not produce another execution or duplicate result card. After three tool rounds, a final model pass runs with tools disabled. That pass uses plain observations instead of native tool-call history. A streaming filter holds possible control-token prefixes before display/playback, so ordinary prose can start immediately. If the model still returns control syntax, SAIL discards pending unspoken text and presents a factual runtime summary of the existing results.
## Inspect local model status

With the SAIL daemon running, use `cargo run -p sail-core --bin saild -- mind-status` to inspect real model capabilities, process ownership, limits and inference metrics. Use the same `SAIL_VOICE_BIND`/TLS variables as your daemon, or set `SAIL_MIND_STATUS_URL` to its loopback `/api/mind/status` endpoint. Model loading is reported separately from readiness. See the [Mind guide](Docs/ARCHITECTURE/Local_Inference.md) for CPU/Vulkan configuration and typed errors.

## Troubleshooting

| Symptom | What to check |
|---|---|
| Microphone unavailable on `http://192.168.…` | Use `http://localhost:8088` on the server computer, or trusted HTTPS from another device. |
| Microphone permission denied | Allow microphone access for the site in browser settings, then retry **Start Microphone**. |
| Port 8088 already in use or the page still looks old | Stop the earlier SAIL runtime, restart from this checkout, and reload the page. |
| A speech model reports `ready: false` | Run `./scripts/voice/setup.sh` from the repository root and inspect the displayed worker error. |
| The local LLM is unavailable | Check the Gemma file and llama-server binary, or the configured `SAIL_LLM_URL`. Startup details are in `.local/llama-server.log` when SAIL starts the model server. |
| Text works but image reasoning fails | Use **Check local models** and confirm the LLM's vision modality is enabled. An existing text-only model server needs a compatible projector. |
| There is text but no audible reply | Check system volume/output, interact with **Send** or **Start Microphone** to enable browser audio, and inspect any TTS error. |
| Replies are slow | Let models finish loading, close competing CPU-heavy work, and compare Balanced with HighPerformance. Gemma generation and speech synthesis add latency beyond VAD. |
| SAIL appears to hear itself | Use headphones, reduce speaker volume, or mute microphone input while listening. |
| A file query is denied | Request a path inside the workspace. Set `SAIL_TOOL_ROOT` at startup if a different inspection root is intended. |

To verify real speech inference independently of the browser:

```bash
.venv-voice/bin/python scripts/voice/smoke.py
```

This generates speech, detects it with Silero, transcribes it with Whisper, and saves a reviewable sample at `.local/voice-smoke.wav`.

## End your session

Use **Stop Microphone** when you no longer want capture. Use **Interrupt** if you also want the current reply to stop. Close the tab when finished, then press **Ctrl+C** in the SAIL terminal to shut down the runtime. A llama-server that SAIL started is stopped with it; a model server that was already running remains independently managed.

### Capture a picture or screenshot during conversation

Say or type “Take a screenshot and explain this error” for the screen, or
“Take a camera picture and tell me what you see” for the camera. SAIL pauses
reasoning until the browser captures an image. Click the capture button to open
the browser permission picker. Screen capture lets you choose a screen, window,
or tab; camera capture takes one frame. Both require HTTPS or localhost and
browser support. Capture stops immediately after the frame is obtained.

“Capture an image and tell me what you see” uses the previous user message to
resolve screen/camera context when possible; otherwise SAIL asks which source
you mean. Permission denial or cancellation never submits an imageless request.
The original request and captured image are sent together to the local model.
Existing uploaded images are analyzed directly without requesting device capture.


### Authorize and continue a tool operation

Configure `SAIL_APPROVAL_TOKEN` (at least 32 bytes) in the protected daemon service
configuration. When a process query or command needs approval, open Settings →
Owner approvals, enter that owner token, and review the exact arguments, actor,
risk and task correlation. **Approve and continue** resumes the waiting request;
you do not need to repeat the question. Deny/revoke or Interrupt cancels it.
Approvals expire after 60 seconds. No operation starts using a grant that expired
while waiting for an execution slot. The owner token stays in page memory.

Ask “What time is it?”, “List three processes”, or “Inspect the workspace root”.
Time and OS telemetry need no elevated grant; other observations are explicitly
approved. The result returns to the model for its answer and appears on Canvas.
Large outputs expose **Open full output** instead of expanding model context.
They are available only within the originating session/turn and expire.

For command requests, an administrator must install and validate the dedicated
runner service described in `Docs/ARCHITECTURE/Guardian_Execution.md` and
`deploy/runner/`. Configure the daemon's `SAIL_RUNNER_SOCKET` and
`SAIL_RUNNER_UID` to match that service. A request such as “Run /bin/echo with
argument hello in the sandbox” requires exact owner approval and runs without
network or host workspace mounts. Missing service/configuration produces an
explicit failure; it never executes the command on the daemon host. This flow
does not provide host administration or reviewed filesystem artifact export.
