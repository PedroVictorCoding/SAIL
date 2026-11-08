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

`sail_common::models::primary_model_path()` is the single resolver used by Core, Mind and
Forge: `SAIL_MODEL_PATH` if set, else the owner's **saved default**, else the primary model.
Weights are looked up in `models/`, `$SAIL_RUNTIME/models` and `<data root>/models`. It never
falls back to another model; missing weights produce an error that names the file to install.

**Default reasoning model.** Settings → Models lists a small catalog
(`sail_common::models::CATALOG`): Qwen3.6-35B-A3B (primary), LFM2.5-24B-A2B Uncensored
(Q4_K_M, text) and LFM3-Omni-2B (Q4_K_M plus its image projector). The choice is saved in
`<data root>/mind/default-model.json` through `PUT /api/mind/models/default` (owner token).
Only installed models can be chosen. It applies the next time SAIL starts: there is still one
resident model, and nothing reloads at runtime. `run.sh` pins the primary model only when
no default is saved. Measure a model with the [model eval](MODEL_EVAL.md) (voice tools, Learn
judge, grounded answers) and Forge's eval before relying on it. On the CPU-only host
(2026-11-07), LFM3-Omni-2B read prompts at ~119 tokens/s and wrote at ~24 tokens/s, against
~48 and ~9.5 for Qwen3.6, and returned well-formed tool calls.

## Thinking and sampling

*Decided 2026-11-08 (LLM task force round 6).*

**What was wrong.** Mind sent `enable_thinking: false` on every request, so no SAIL work ever
reasoned, not even background jobs where nobody is waiting. Forge called llama-server
directly without that switch, and llama-server lets Qwen3 think **without limit** by
default: a one-line arithmetic question used all 200 of its tokens thinking and returned an
empty answer. Forge then read the thinking text (`reasoning_content`) as if it were the
answer. Sampling was llama-server's default for everything except temperature, not what the
model's authors recommend.

**Decision.** Thinking is chosen per request, with a hard budget:

- `Generation::reasoning_budget: Option<u32>` (`sail-protocol`). `None` means no thinking
  and is the default, so existing callers are unchanged. `Some(n)` allows up to `n` tokens of
  thinking, clamped to `MAX_REASONING_BUDGET` (4096).
- `max_tokens` stays the **answer** budget. `sail_common::request_body` adds the thinking
  budget to what it asks llama-server for (`max_tokens + n`), sets
  `reasoning_budget_tokens: n` (llama.cpp forces the end-of-thinking tag at the budget) and
  `reasoning_format: deepseek`, so reasoning arrives in `reasoning_content` and never in the
  answer.
- Mind counts streamed thinking chunks into `Usage::reasoning_tokens` and drops the text: it
  is never returned, streamed to the UI, spoken or stored. The context-window check and the
  output-token check include the thinking budget. The inference deadline grows by the budget
  at 8 tokens/s.
- **Live voice and anything interactive never think.** At about 16 tokens/s, each 1000
  tokens of thinking is about a minute before the first word.
- **Sampling follows the model card**, per family (`request_body::family_for`, from the
  weights file name):
  - Qwen3.x answering directly: top_p 0.8, top_k 20, min_p 0.
  - Qwen3.x thinking: top_p 0.95, top_k 20, min_p 0, and temperature at least 0.6, because
    greedy decoding makes the thinking loop.
  - LFM: min_p 0.15, repeat penalty 1.05, and no thinking mode.
  - Any other model: server defaults, with thinking off.

  The caller still picks the temperature.
- `sail_model_eval` builds its bodies with the same `request_body::chat_body`, so a score
  is a score for what SAIL really sends.

**Which work may think.** Each background job has one row in `request_body::Job`. Its
built-in budget is set only after the model eval shows that thinking helps (see
[Model eval](MODEL_EVAL.md)), and an environment variable overrides it, so the owner can
trial a budget without a rebuild. The value is a number of tokens, or `off`/`0`.

| Job | Caller | Override |
|---|---|---|
| Learn free-recall judge | `arena-learn/agent/judge.rs` | `SAIL_THINK_JUDGE` |
| Course Factory authoring | `factory/author/prompts.rs` → `MindAuthor` | `SAIL_THINK_COURSE_AUTHORING` |
| Moodle extraction | `sail-dialogue/moodle_model.rs` → `MindAuthor` | `SAIL_THINK_MOODLE` |
| Background plans (never interactive ones) | `sail-agents/planning.rs` | `SAIL_THINK_PLANNING` |
| Memory consolidation | `sail-core` proposer | `SAIL_THINK_MEMORY` |
| Forge | `sail-forge/solver.rs` (calls llama-server directly) | `SAIL_FORGE_REASONING_BUDGET` (default 2048) |

Forge streams with the same body builder. Only `delta.content` can become a patch, and
thinking shows in its activity as "thinking (N tokens)", never as text. A turn that thinks
without answering is retried like any other empty reply.

**Output limit.** Mind's `ResourceLimits::max_output_tokens` is 4096, the largest answer
budget a caller asks for (Course Factory lessons). It was 1024 until 2026-11-08, so Mind
rejected every Moodle extraction (2048) as an invalid request. Tests now check that the
authoring and extraction budgets fit. The planner and Arena turns keep their own 1024.

**Not in this round:**

- Sending a hard background job to a stronger external model waits for the guarded external
  provider work (draft PR #262). Moodle and memory data stay local whatever happens.
- A larger KV cache precision (`SAIL_CACHE_TYPE_K=q8_0`) is already configurable. It is
  measured with the eval before the default changes, because it costs VRAM on the RX 5500.

## Host admission and voice priority

Mind shares Core's process-wide resource broker with Forge and speech workers. Owned
model startup and inference require fresh RAM/swap/CPU admission; trusted Dialogue
calls select voice priority and preempt background inference. See
[Host resource admission](SAIL_RESOURCE_ADMISSION.md) for estimates, lease lifetimes,
status fields, verification and remaining browser/VRAM/eviction limitations.

## 7a. GPU offload for MoE models (#34)

For a wedged AMD driver or Vulkan device-loss failure, see
[RX 5500 Vulkan recovery](GPU_RECOVERY.md). Flash Attention disabling is explicit
(`-fa off`); it must not fall back to llama.cpp's automatic mode.

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
