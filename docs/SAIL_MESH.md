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
| Owner HTTP API `/api/mind/mesh` | `crates/sail-dialogue/src/mesh_http.rs` |
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
  run at once (more get 503).

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
- [ ] Record measured latency against a real worker PC (needs the owner's second machine).
- [ ] Show worker status and pairing in Settings → Models (round 23 UI slice).
- [x] Focused tests and a reviewable PR without automatic merge.

## 8. Next steps
1. LAN discovery (announce only; pairing still needs the code), round 23.
2. Settings → Models "Worker PCs" UI, round 23.
3. Measure real offload latency and throughput with the owner's PC; tune `max_parallel`.
4. Later: route non-inference jobs (embeddings, speech) only with their own contracts; llama.cpp RPC
   (one model split across machines) stays out of scope until measured worthwhile.

## 9. Related references
- [Roadmap and subsystem taxonomy](https://github.com/PedroVictorCoding/SAIL/issues/15)
- [Documentation index](README.md)
- [SAIL Studio](SAIL_STUDIO.md) for persistent editable artifacts (distinct from Canvas)
