# SAIL MIND — Product and Technical Specification

**Status:** Proposed / evolving — verify implementation against code · **Domain:** Intelligence & Knowledge · **Roadmap issue:** [#3](https://github.com/PedroVictorCoding/SAIL/issues/3) · **Priority:** P0 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Select and run local models across CPU/GPU with one typed interface; balance interactive and background work.

## 2. Functional scope
- Local inference adapters.
- task-aware routing.
- warm/cold residency and memory budgets.
- device discovery.
- context/KV management.
- measured latency/throughput.
- streaming and cancellation..

## 3. Ownership and non-goals
Does not grant tool access, plan agent actions or own conversation history; Guardian owns authority, Agents owns plans, Arena owns memory.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Core resource scheduler, Guardian, Dialogue, Vision, Agents, Forge, Mesh.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Existing Settings → Models and contextual active-model status; no mandatory new dashboard.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 7. MVP vertical slice
Route one text request to a local model, stream tokens, cancel it, report actual backend/VRAM/latency and recover from server unavailability.

### Acceptance checklist
- [ ] Confirm existing source files, interfaces and dependency versions before changing architecture.
- [ ] Implement the minimal real end-to-end flow above, without mocks presented as production behavior.
- [ ] Test success, offline/unavailable, permission denial, cancellation and restart/reconnection as relevant.
- [ ] Record measured latency and resource use where relevant; avoid fabricated benchmarks or status.
- [ ] Show user-visible success/error through the mapped existing UI and preserve user control.
- [ ] Submit focused tests and a reviewable PR without automatic merge.

## One resident model

SAIL keeps **one** large general model loaded, Qwen3.6-35B-A3B (decided 2026-11-03).
Voice, agents, memory and Forge all use it; Mind's scheduler orders their requests and
live voice goes first. A second large model does not fit beside it in the host's RAM,
and swapping to a separate coding model would take the main model away from voice
while Forge runs. Forge therefore never selects a model of its own.

`sail_common::models::primary_model_path()` is the single resolver used by Core, Mind,
Forge and `run.sh`: `SAIL_MODEL_PATH` if set, else the primary model in `models/` or
`$SAIL_RUNTIME/models`. It never falls back to another model; missing weights produce an
error that names the file to install. A different model is a deliberate change of
`SAIL_MODEL_PATH`, measured with Forge's eval before adoption.

## Host admission and voice priority

Mind shares Core's process-wide resource broker with Forge and speech workers. Owned
model startup and inference require fresh RAM/swap/CPU admission; trusted Dialogue
calls select voice priority and preempt background inference. See
[Host resource admission](SAIL_RESOURCE_ADMISSION.md) for estimates, lease lifetimes,
status fields, verification and remaining browser/VRAM/eviction limitations.

## 7a. GPU offload for MoE models (#34)

The host has an AMD RX 5500 with little VRAM. The resident model, Qwen3.6-35B-A3B, is a mixture-of-experts with about 3B parameters active per token. Most of their weights are experts, and only a few run for each token. The useful split is therefore **every layer's attention and KV cache on the GPU, expert weights in system RAM**, not "the first N whole layers on the GPU". The latter fills VRAM with rarely used experts.

| Setting | Meaning |
|---|---|
| `SAIL_MODEL_BACKEND=hybrid` (or `vulkan`) | Offload through llama.cpp's Vulkan backend |
| `SAIL_VULKAN_DEVICE=Vulkan0` | The device llama-server lists for the AMD GPU |
| `SAIL_GPU_LAYERS=99` | All layers on the GPU |
| `SAIL_CPU_MOE=all` | Keep every layer's experts in RAM (`--cpu-moe`) |
| `SAIL_CPU_MOE=N` | Keep the experts of N layers in RAM (`--n-cpu-moe N`); smaller N puts more on the GPU |

`SAIL_CPU_MOE` is rejected on the CPU backend. Unset or `off` means no expert flags, so dense models and existing setups are unchanged. `run.sh --cpu-moe <all|N|off>` sets it too.

**Enable it only after measuring (vision §5).** Run `scripts/mind-gpu-setup.sh` on the host with SAIL stopped. It:
1. checks the GPU, render-node access and the Vulkan driver, and refuses a software device;
2. builds llama.cpp `b11600` with Vulkan into `/srv/sail/llama.cpp-vulkan`;
3. benchmarks each model with `llama-bench`: CPU-only, then `-ngl 99` with a sweep of `--n-cpu-moe` (99, 40, 36, 32, 28, 24);
4. stores the raw JSON in `/srv/sail-data/mind/bench/`.

Only when the best GPU setting beats CPU-only by at least 5% on both prompt processing and generation does it write `/srv/sail-data/mind/gpu.env`. `run.sh` loads that file unless a backend is given explicitly. Otherwise SAIL stays on CPU. Delete the file to go back to CPU. Re-run the script after driver, llama.cpp or model changes.

Not done yet: per-model profiles in the router (`InferenceProfile` has no expert-placement field, so `SAIL_CPU_MOE` applies to every managed model), and VRAM headroom checks while ASR/TTS run.

## 8. Next steps
1. Audit the relevant implementation and the linked roadmap issue; update this spec with confirmed module/file paths and contracts.
2. Build the MVP vertical slice with real tool/model/provider integrations.
3. Add security and regression tests before expanding capability breadth.
4. Record known limitations, operational instructions and migration compatibility.

## 9. Related references
- [Roadmap and subsystem taxonomy](https://github.com/PedroVictorCoding/SAIL/issues/15)
- [Documentation index](README.md)
- [SAIL Studio](SAIL_STUDIO.md) for persistent editable artifacts (distinct from Canvas)
