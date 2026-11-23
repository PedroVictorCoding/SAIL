# SAIL MESH — Product and Technical Specification

**Status:** MVP implemented (inference offload to paired worker PCs) — verify against code · **Domain:** Infrastructure & Compute · **Roadmap issue:** [#13](https://github.com/PedroVictorCoding/SAIL/issues/13) · **Priority:** P4 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Schedule future model inference across trusted local workers without undermining offline-first defaults.

## 2. Functional scope
- Node discovery.
- secure pairing.
- capability negotiation.
- scheduling and health.
- transport observability.
- fallback to host.
- bounded data movement..

## 3. Ownership and non-goals
Not session continuity: Relay handles UI sessions; Mind selects models; Guardian controls network/device trust.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Mind, Core scheduler, Relay, Guardian, Bridge.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Existing Settings → Compute and connected worker status; no mandatory new app.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 7. Implemented: inference offload to worker PCs

A **worker** is another PC on the owner's network (Windows or Linux) with its own
llama-server. The host sends it whole inference requests; it never splits one model
across machines.

| Piece | Code |
| --- | --- |
| Contracts (`MeshHello`, `MeshStatus`, `MeshMode`, …) | `crates/sail-protocol/src/mesh.rs` |
| Worker binary, pairing code, identity, pinned TLS, host pairing file | `crates/sail-mesh` (portable; no `libc` or Linux-only crates) |
| Pool, health checks, routing policy, owner operations, `sail-core mesh` CLI | `crates/sail-mind/src/mesh.rs` |
| Routing and fallback inside Mind | `MindService::infer_with_priority` / `run_remote` in `crates/sail-mind/src/service.rs` |
| llama-server client reused for workers | `LocalGemmaBackend::remote` in `crates/sail-mind/src/local.rs` |
| LAN discovery (announce only; never pairs, never carries the secret) | `crates/sail-mesh/src/discovery.rs`, `sail-core mesh discover` |
| Owner HTTP API `/api/mind/mesh` | `crates/sail-dialogue/src/mesh_http.rs` |
| Settings → Models "Worker PCs" (status, pair, turn off, forget) | `crates/sail-dialogue/web/mesh.js`, `mesh.css`; `scripts/ui/mesh_browser_test.py` |
| End-to-end test (real worker over TLS, fake llama-servers) | `crates/sail-mind/tests/mesh_e2e.rs` |

### Trust and transport

- `sail-mesh-worker init` creates a self-signed certificate, a random worker id and a
  256-bit secret in `%APPDATA%\sail-mesh` (Windows) or `~/.config/sail-mesh` (files 0600).
- `sail-mesh-worker serve` prints a **pairing code**
  `sailmesh://<host>:<port>/?name=…&fp=<SHA-256 of the certificate>&key=<secret>`.
  It contains the secret, so treat it like a password.
- On the host, `sail-core mesh pair '<code>'` (or the Settings → Models UI) connects to the
  running worker, **pins its certificate** by fingerprint and proves the secret before
  saving `<data root>/mesh/workers.json` (0600). Pairing is always an explicit owner step;
  nothing pairs automatically.
- Every request is HTTPS to the pinned certificate with `Authorization: Bearer <secret>`.
  Proxies and redirects are never followed. Prompts never cross the network in clear text.
- The worker forwards only `GET /health`, `GET /props`, `POST /apply-template`,
  `POST /tokenize` and `POST /v1/chat/completions` to its configured llama-server, plus
  its own `GET /mesh/v1/hello`. It is not a shell, file or generic proxy. `/props` loses
  the model's directory before it leaves the worker. At most `--max-parallel` generations
  run at once (more get 503). Only `Content-Type` crosses in either direction (never the
  host's `Authorization` to llama-server). After 20 failed authentications in 60 s an
  address is refused for 60 s.
- `serve` announces `{name, port, certificate fingerprint}` on UDP 7711 every 5 s
  (`--no-announce` turns it off). `sail-core mesh discover` lists what it hears; pairing
  still needs the code.

### Routing policy

| Setting | Default | Meaning |
| --- | --- | --- |
| `SAIL_MESH` | `offload` | `offload`: eligible work goes to a free worker first. `overflow`: only while the host model is busy. `off`: never. |
| `SAIL_MESH_PRIORITIES` | `background` | Which work may leave the host. Add `interactive` for chat turns. **Voice never leaves the host.** |
| `--serves` at pairing | any | Limit a worker to some host model names/aliases. |

- Only `Ready` workers with a free slot are used; images only go to workers whose model
  has vision, tool calls only to workers whose template supports tools.
- **Fallback:** if a worker fails before anything was delivered to the caller (down,
  busy, wrong model, context too small), the request runs on the host and is counted as
  `fell_back`. The worker is skipped for 20 s. If output already streamed, the error is
  returned: SAIL never splices two models' answers or invents a success.
- **Cancellation:** cancelling closes the TLS stream; the worker drops its llama-server
  request, which stops generating.
- **Health:** every 15 s the host calls `hello` on each worker and re-reads the pairing
  file, so CLI changes reach a running SAIL without a restart.
- Status: `GET /api/mind/status` → `mesh`, `sail-core mind-status`, `sail-core mesh list`.

### Sharing the PC with its owner (resource caps and stepping aside)

A worker PC is someone's own computer, so the worker keeps out of the way.

**Hard caps** (for the llama-server that `serve --llama-server` starts):

| Flag | Default | Effect |
| --- | --- | --- |
| `--threads N` | half the PC's threads | llama-server `--threads`/`--threads-batch` |
| `--priority P` | `below-normal` | `low`, `below-normal` or `normal`; Windows priority class, `nice` on Linux, so the owner's programs always win the CPU |
| `--gpu-layers N` | 99 | fewer layers leave VRAM for other programs (0 = CPU only) |
| `--ctx N` | 16384 | smaller context uses less memory |
| `--max-parallel N` | 1 | generations at once |

**Stepping aside.** Every 5 s the worker measures how the PC is used by *other* programs
(its own llama-server is left out): CPU, free memory, the process list, NVIDIA GPU load when
the driver's `nvml.dll`/`libnvidia-ml` is present, and on Windows whether a full-screen game
is in front.

- **Gaming or making music: full rest.** A full-screen Direct3D game, other programs using
  ≥ 50 % GPU for two readings, music software (Ableton Live, FL Studio, REAPER, Cubase,
  Nuendo, Studio One, Bitwig, Pro Tools, Reason, Ardour, LMMS, Renoise, Cakewalk, Waveform,
  Audition, Maschine, Serato, rekordbox, Traktor) or a program named with `--pause-for`
  makes the worker refuse all work **and stop its llama-server** to free the GPU, CPU and
  memory. A running generation is cut and the host handles the next one. When the program
  closes, the worker waits one backoff before loading the model again.
- **Busy PC: exponential backoff.** Other programs using ≥ 50 % CPU (`--busy-cpu`) or
  ≥ 20 % GPU (`--busy-gpu`), less than 2 GiB free memory (`--min-free-ram`) or a full-screen
  window, for two readings in a row, pause new work for 1 min. If the PC is still busy when
  the pause ends, the next pause doubles (2, 4, 8 … up to `--max-backoff`, 30 min). Running
  work finishes. Ten calm minutes reset the pause to 1 min.
- `--no-rest` turns this off. With `--upstream` (a llama-server you run yourself) the worker
  still refuses work but cannot stop that server.

The host sees a resting worker as **Busy** (`hello.busy`: reason, `full`, seconds until the
next check), skips it, and shows "Resting — Stepped aside: …" in Settings → Models. A worker
that started resting between two health checks answers generations with **429** and
`Retry-After`; the host runs that request itself without counting a failure (503 still
means the worker's llama-server is broken).

### Finding and pairing a worker

`sail-core mesh discover` lists workers announcing on the LAN (the host must receive UDP
7711). Run `sail-mesh-worker pairing-code` on the worker to show its code again.

### Setting up a Windows worker

1. Install Rust (`rustup`, MSVC toolchain), clone this repository and run
   `cargo build --release -p sail-mesh`. It builds only the worker and its portable
   dependencies. The binary is `target\release\sail-mesh-worker.exe`.
2. Get a llama.cpp release for your GPU (CUDA, Vulkan or CPU) and a GGUF model. The same
   model family as the host keeps answers consistent; a larger one is fine for background work.
3. `sail-mesh-worker init --name desktop`
4. `sail-mesh-worker serve --llama-server C:\llama\llama-server.exe --model C:\models\model.gguf --gpu-layers 99`
   (or `--upstream http://127.0.0.1:8080` for a llama-server you already run with `--jinja`).
   Allow `sail-mesh-worker` through Windows Firewall on private networks (TCP 7710).
5. On the host: `sail-core mesh pair '<code printed by the worker>'`, then
   `sail-core mesh test desktop`.

Linux workers use the same steps with the Linux binaries.

### Acceptance checklist
- [x] Confirm existing source files, interfaces and dependency versions before changing architecture.
- [x] Pair one trusted worker, route a permitted inference request and fall back locally on disconnect (`mesh_e2e.rs`).
- [x] Test success, unavailable, wrong secret/certificate, cancellation and fallback rules.
- [x] Live check with the real binaries on the host (loopback worker in front of the running
  llama-server): pair, list, test and a forged fingerprint refused (2026-11-14).
- [ ] Record measured latency against a real worker PC (needs the owner's second machine).
- [x] Resource caps and stepping aside: policy tests (`rest.rs`), resting worker refused
  with 429 (`worker_http.rs`), host skips it without a failure (`mesh_e2e.rs`), live run
  with `--pause-for` stopping and restarting llama-server after the backoff (2026-11-14).
- [ ] Full-screen game detection and NVML GPU readings on the owner's Windows PC.
- [x] Show worker status and pairing in Settings → Models.
- [x] Focused tests and a reviewable PR without automatic merge.

## Compute

Pooled compute lets a paired worker PC lend its GPU and RAM to the **host's** own large model (for example `gpt-oss-120b-MXFP4`) using `llama.cpp` RPC (`ggml-rpc-server`). Unlike whole-request offload, the host owns the model, tokenizer, sampling, and KV cache, while the PC computes the model's expert tensors placed on it via `--rpc` and `-ot`/`--override-tensor`. Weights stay resident on the PC (cached under `--cache-dir` via `LLAMA_CACHE`), never re-sent per token.

### Lending window and `pause` / `play` / `auto`

- **Overnight window:** lending is automatic during the PC's local daily window (`--window`, default `01:00–08:00`; `start > end` wraps midnight, `start == end` means always).
- **Commands:**
  - `sail-mesh-worker pause` — stops lending immediately, closes all tunnels, stops `ggml-rpc-server`, and resumes every hibernated app within 5 s.
  - `sail-mesh-worker play [--for D]` — lends immediately outside the window (e.g. `--for 3h`, or indefinitely until changed). Games, `MUSIC_APPS`, and busy-PC rest rules still apply and move the worker to `Resting`.
  - `sail-mesh-worker auto` — returns to the scheduled overnight window.
  - `sail-mesh-worker status` — prints the current compute state in one plain-words line.
- From SAIL, `POST /api/mind/mesh/workers/:name/lend` forwards `LendMode` (`auto`, `play`, `pause`) to the worker's `POST /mesh/v1/lend` over the existing pinned-TLS channel.

### Hibernation and guarantees

When entering `Lending`, the worker can suspend and trim owner-listed desktop apps to free RAM (`sail-mesh-worker hibernate add <exe>`, `remove`, `list`, `resume-all`). Nothing is hibernated unless explicitly listed.

- **Journal before suspend:** every target PID and its process start time are written and `fsync`ed to `hibernate.json` *before* suspension, and removed only *after* resume succeeds.
- **Startup recovery:** on start (including after a crash or reboot), the worker resumes everything in `hibernate.json` before lending or anything else.
- **Guaranteed resume:** `pause`, `stop`, Ctrl-C/SIGTERM, leaving `Lending`, or a hibernated process receiving foreground focus (`owner_returned`) resumes all journaled apps.
- **Watchdog & PID reuse:** a process suspended past the window end + 15 min is resumed automatically; a journaled PID whose start time no longer matches is skipped rather than resumed blindly.
- **Windows vs. Unix:** Windows uses `NtSuspendProcess`/`NtResumeProcess` and `EmptyWorkingSet` (refusing unless free commit ≥ working sets to trim + 2 GiB); Unix uses `SIGSTOP`/`SIGCONT`.
- **Never-list:** the worker never hibernates:
  - system and session processes (Windows: `System`, `smss`, `csrss`, `wininit`, `winlogon`, `services`, `lsass`, `svchost`, `dwm`, `explorer`, `fontdrvhost`, `sihost`, `ctfmon`, `audiodg`, `MsMpEng`; Linux: PID 1, session leader, kernel threads);
  - the worker itself or its children;
  - any `MUSIC_APPS` entry;
  - anything with a full-screen window;
  - anything not on the owner's list.

### Tunnel (no new firewall port)

`ggml-rpc-server` has no authentication, so it binds **`127.0.0.1` only** on the PC and is never exposed on the LAN. The host reaches it solely through the worker's existing pinned-TLS + bearer channel:

- `GET /mesh/v1/compute` with `Upgrade: sail-rpc/1` switches to `101` and bridges raw bytes to `127.0.0.1:<rpc_port>` **only while in `Lending`** (otherwise `429` with `Retry-After`).
- Leaving `Lending` closes all open tunnel connections within 5 s.
- On the host, `HostTunnel::bind` listens on `127.0.0.1:0` and passes that loopback address to `llama-server --rpc`. No new firewall port is opened.

### Host caps and admission

The resident model (Qwen3.6-35B-A3B) stays loaded for voice and chat. The heavy lane's host share is hard-capped:

- **RAM cap:** `SAIL_HEAVY_HOST_RAM_GIB` (default `6` GiB).
- **VRAM cap:** `SAIL_HEAVY_HOST_VRAM_GIB` (default `2.5` GiB; `0` disables host GPU offload).
- **Admission:** `admission::admit` refuses to start the heavy model if the host's free RAM minus the planned host share would drop below **4 GiB** (`HEADROOM_BYTES`, reserved for voice + Qwen3.6). Qwen3.6 is never unloaded or restarted by the heavy lane.

### Night Shift routing

- Night Shift errands default to `use_heavy: true` ("Use the big model when the PC is lending") and `fallback_to_host: false` ("If the PC isn't lending, use Qwen instead").
- When `use_heavy` is on and the heavy lane is not running, the errand stays `Queued` (the UI shows **"Waiting for the PC to lend"**) and does not silently fall back to Qwen3.6 unless `fallback_to_host` is enabled.
- If a `Pooled` attempt loses the heavy lane mid-run, it requeues with reason `heavy_unavailable`.

### Measured speed

—

### Setting up the PC

On the Windows worker PC (no admin rights and no new firewall port needed):

1. Install the pinned `ggml-rpc-server` (`b12014`, Windows Vulkan x64) into `%LOCALAPPDATA%\SAIL\mesh\rpc\`:

   ```powershell
   pwsh -File scripts\mesh\install-rpc-server.ps1
   ```

   The script verifies the zip's SHA-256 (`e3712df80ab21c81051f43ac6edd9f4c8faefb8f7f760d971a578a78d6bbbdac`) and extracts only `ggml-rpc-server.exe` and its `ggml*.dll` / `libomp.dll` runtime files.
2. Start the worker with `--compute` and an HDD `--cache-dir` (using the command printed by the script, e.g. `D:\sail-cache`):

   ```powershell
   sail-mesh-worker serve --compute "%LOCALAPPDATA%\SAIL\mesh\rpc\ggml-rpc-server.exe" --cache-dir D:\sail-cache
   ```

## 8. Next steps
1. First Windows build and run on the owner's PC (not verifiable on the host: `ring` needs MSVC tools).
2. Measure real offload latency and throughput with the owner's PC; tune `max_parallel`.
3. Later: route non-inference jobs (embeddings, speech) only with their own contracts; llama.cpp RPC
   (one model split across machines) stays out of scope until measured worthwhile.

## 9. Related references
- [Roadmap and subsystem taxonomy](https://github.com/PedroVictorCoding/SAIL/issues/15)
- [Documentation index](README.md)
- [SAIL Studio](SAIL_STUDIO.md) for persistent editable artifacts (distinct from Canvas)
