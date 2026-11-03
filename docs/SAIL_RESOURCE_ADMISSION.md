# Host resource admission and voice priority

This implements a bounded first slice of the master vision's §5 and §13.2. Core owns
scheduling policy, implemented in `sail_common::resources` so daemon subsystems share
one broker without depending on the Core executable. Guardian still owns authority;
runner cgroups and existing sandbox limits still enforce OS boundaries.

## Acceptance contract

When speech starts, the daemon reserves voice priority immediately. Background Mind
and Forge inference is cancelled, running Forge build process groups are terminated,
and new background inference/build/browser claims are refused. Priority lasts across
capture, STT, model output, tool calls and TTS; capture-limit, mute, reset, disconnect,
completion and cancellation release their guards. Already accepted speech requests
retain priority while their worker drains the response, even if the caller disconnects.
No partial Forge model response becomes a patch. Resource denial/preemption produces
an unsuccessful `resource_blocked` solver outcome, preserving the workspace and the
existing blocked-job projection. A retry is explicit; this slice does not schedule
automatic resumption or replay tool effects.

Before execution, the shared broker reads Linux `MemAvailable`, total RAM, swap
used/total, and available logical CPUs. Unknown or inconsistent readings deny work.
It atomically considers other leases, headroom, CPU demand and concurrency limits.
Voice priority never overrides RAM or swap checks. Voice inference has one extra
bounded queue place and waits within the existing queue deadline for preempted work
to release its leases. Failed admission emits the normal typed failure event, without
emitting a model-started event or calling the inference backend.

## Current policy and boundaries

Defaults in `AdmissionPolicy` reserve 2 GiB RAM for the OS/audio, reject new admitted
work above 1 GiB swap usage, leave two logical CPU slots outside admitted work, and
permit one owned heavy model, one browser claim and one Forge build. The policy is a
typed constructor argument, allowing deterministic deployment composition and tests;
this initial slice does not expose policy editing in the frontend.

| Boundary | Admission and lifetime |
| --- | --- |
| Mind inference | 128 MiB incremental allowance and backend peak configured prefill/generation threads; lease includes queued time and is released on completion, error, cancellation or future drop. |
| Owned llama-server | GGUF/projector file sizes + 512 MiB working allowance + 256 KiB per context token before spawn; weights >=1 GiB consume the heavy-model slot. Lease follows the owned child through crashes/restarts/shutdown. |
| Speech worker | Startup allowance: STT 1 GiB, TTS 512 MiB, VAD 128 MiB; lease follows the persistent worker. Audio has separate priority guards, not an inference queue slot. |
| Embeddings/local review | Shared background inference admission covers headers and response bodies; voice cancellation drops the request future. Externally managed server residency is not owned by SAIL. |
| Forge model transport | Background inference lease covers HTTP request, response body and retries; stalled connections/streams poll cancellation every 100 ms and discard partial output. CPU demand is one scheduling slot because the externally configured server's thread allocation is unknown. |
| Forge sandbox command | 1 GiB incremental allowance, one CPU slot, one build slot, and `CARGO_BUILD_JOBS=1`; lease precedes spawn and remains until the child is reaped and streams drained. Voice cancellation kills the process group using the existing timeout cleanup, polled every 20 ms. |
| Browser hook | `ResourceClaim::browser(estimated_peak_bytes)` shares RAM/CPU/concurrency admission. The Bridge browser worker is absent from this default-branch implementation; wiring its launch/close and cancellation to the lease remains required before claiming runtime browser scheduling. |

Startup estimates are explicitly **estimates**, not measured RSS, validated model
profiles or hard limits. Once a model/speech worker answers successfully, its pending
startup reservation is cleared: resident memory is already reflected in the next
`MemAvailable` reading. Its residency lease remains. Loading another owned heavy model
requires unloading the first; this slice does not automatically evict it or kill an
externally managed server. Pending reservations can conservatively count memory
already allocated during startup until the first successful response.

This broker coordinates participants in one daemon process. It measures pressure
from other host processes but cannot control them or coordinate independent SAIL
processes. It is not a cross-process distributed lock, live CPU utilization monitor,
VRAM allocator, disk quota, automatic swap cleanup, or replacement for OS limits.
Cgroup-specific container memory limits, adaptive residency eviction, external reviewers, and full disk/VRAM
admission remain subsequent work. Existing operation timeouts remain in force.

## Observation and verification

`GET /api/mind/status` includes additive optional `resources` state: actual host
measurements or an observation error, RAM/swap thresholds, voice guard count,
admitted inference, owned-heavy-model/speech/browser/build counts, pending RAM and
CPU reservations, rejection count and preemption count. These counts are admitted
leases, not proof that a process completed work; queued inference is included.
Older status payloads without this field still deserialize. No transcript, prompt,
credentials or persistent task content is added to this state.

Run:

```sh
cargo test -p sail-common -p sail-mind --tests
cargo test -p sail-forge --lib --test model_request_test --test worktree_isolation_test -- --test-threads=1
cargo test -p sail-dialogue --lib --test staged_speech_orchestration_test -- --test-threads=1
cargo check -p sail-core
```

Tests exercise measured-host parsing, real Linux observation, low RAM/swap/unknown
telemetry, atomic concurrent launch admission, residency release, voice cancellation,
full-queue priority, aborted caller cleanup, and real loopback HTTP streaming. Model
lifecycle fixtures verify rejection before executing a model binary; Forge fixtures
verify refusal before an HTTP request/build and cancellation without changing source.
These are boundary tests, not a real microphone/speaker or model-quality benchmark.
Target-host audio latency, AMD VRAM behavior and full UI reconnect behavior require
separate hardware verification; no benchmark improvement is claimed here.
