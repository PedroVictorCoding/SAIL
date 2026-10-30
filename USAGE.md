# Using SAIL

## Recommended experience: talk to SAIL on your own computer

Run SAIL on the same computer as your browser and open **http://localhost:8088**. Start with the **Balanced** tier, select your preferred speech voice, and keep one browser tab open for the conversation. This is the simplest everyday setup: no TLS configuration, cloud account, or external inference API is needed.

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
3. Click **Check local models**. The first check loads the speech workers and may take longer than subsequent checks.
4. Confirm that the speech models and local LLM report `ready: true`. Image reasoning also requires the LLM's `vision` modality to be enabled.
5. Choose a voice: **English · Heart**, **Português · Dora**, **Português · Alex**, or **Español · Dora**.
6. Click **Start Microphone** and allow microphone access in the browser.
7. Speak a short request, then pause. SAIL detects the end of your turn automatically.

For example:

> Olá SAIL. Responde em português e explica brevemente como funciona o teu pipeline de voz.

The voice selector changes synthesized speech; ask for your preferred response language in the conversation as well. Headphones help prevent the assistant's audio from being picked up by the microphone.

## Everyday controls

| Control | When to use it | What happens |
|---|---|---|
| **Start / Stop Microphone** | Begin or finish hands-free input | Starts capture or releases the microphone. Stopping capture does not cancel an existing reply. |
| **Mute: ON / OFF** | Temporarily stop sending your voice | Gates microphone input while leaving assistant playback available. Use **Stop Microphone** to release the device. |
| **Interrupt** | Stop a reply or change direction | Cancels the current assistant turn and stops active and queued playback. |
| **End turn** | You have finished speaking but SAIL is still listening | Finalizes an active speech turn without waiting for the silence timeout. |
| **Tier: Balanced** | Adjust the CPU budget | Cycles through Balanced, HighPerformance and LowPower. |
| **New conversation** | Start a different topic without earlier context | Cancels the current reply and clears this session's conversation, Canvas and voice memory. |
| **Check local models** | Confirm readiness or investigate a model error | Shows speech worker readiness and LLM capabilities in the companion panel. |

Speaking while SAIL is replying also triggers automatic interruption once speech is detected. For immediate, predictable cancellation, use **Interrupt**.

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

The **Kernel: os:status**, **Kernel: proc:list** and **Kernel: fs:query** buttons run those inspections directly and display their results. A direct tool button does not request a spoken explanation; ask in the conversation if you want SAIL to interpret the result.

Voice tools currently provide read-only inspection. They do not execute arbitrary shell commands, edit files, or install software. Filesystem inspection is limited to the configured workspace root.

### Ask for a diagram or structured explanation

> Show the voice pipeline as a Mermaid diagram, then briefly explain each stage.
>
> Return a JSON object comparing the three hardware tiers.

SAIL streams its response into the conversation. Fenced Mermaid diagrams and JSON blocks appear in the Canvas alongside it. If a diagram is invalid, the panel shows its source so you can ask SAIL to correct it.

### Discuss an image

1. Choose a **PNG or JPEG up to 4 MB** using the file input.
2. Type a specific question, such as “Describe the main elements of this diagram.”
3. Click **Send**.

The image is shown in the panel and sent to the local Gemma vision service with your question. An image attachment is cleared after sending; attach it again for a follow-up that requires examining its pixels.

For measured image statistics, select an image and click **Kernel: vision:frame**. This reports dimensions, luminance and edge density. Use a conversational image question for semantic reasoning.

### Use text without the microphone

Type in the message field and click **Send**. This uses the same session, local Gemma inference, speech output and Canvas. Microphone permission is not required. Sending a new message cancels the previous assistant turn.

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
