# Dialogue prompt processing

Dialogue uses a compact shared system prompt (`sail-dialogue/src/prompt.rs`) and
the existing selected tool schemas. The full duplicate prose tool catalog is no
longer sent every turn; the summary still lets capability questions be answered
without running tools. Selected tools, model weights, sampling, output budgets,
Guardian execution checks, and memory retrieval are unchanged.

Clock/timezone and opt-in ambient observations accompany the newest user message
inside the existing labelled, untrusted context block. They no longer mutate the
system prompt. Ambient capture consent, expiry, and exclusion from durable memory
still apply. Tool selection reads the user's own words outside that block.
Approval requirements, denied-query handling, evidence-based success reporting,
memory retrieval guidance, and ambiguous-reminder previews remain explicit.

This keeps the shared prefix stable across clock/ambient changes. Cache reuse
still depends on the actual tool schemas, history and model-server configuration;
it is not guaranteed across different turns or clients. No persistent prompt cache
or new conversation store is introduced.

## Local measurement

The 2026-11-05 comparison used the same Qwen3.6-35B-A3B Q4_K_M model,
llama.cpp b12014-aa588a346, hybrid attention offload with CPU experts, f16 caches,
Flash Attention off, and 256-token batches. Both variants contained the same
fixed UTC clock/timezone and identical selected tool schemas. The installed
`/apply-template` and `/tokenize` endpoints measured these full rendered prompts:

| Probe | Before | After | Tokens removed |
|---|---:|---:|---:|
| Short arithmetic answer | 2278 | 1655 | 623 |
| Workspace listing request | 2432 | 1809 | 623 |
| Ambiguous reminder | 2628 | 2005 | 623 |
| Capability question (no tools) | 1100 | 477 | 623 |

These are prompt-processing savings, not an increase in decode tokens/second or
proof of equal quality on every task. At the previously measured ~42 prompt
tokens/second, 623 uncached tokens represent approximately 15 seconds of work.
Real latency also includes paging, queueing, retrieval, decoding and speech.

In the cold arithmetic pair, both variants answered `102`. Backend prompt time
fell from 54.18 to 44.80 seconds (17% less); both generated four tokens at about
14 tokens/s. End-to-end HTTP time was 64.77 seconds before and 88.31 seconds after:
other requests were using the shared single model, so queueing masked the prompt
improvement. This run is evidence of reduced backend work, not a claim that total
user-visible latency fell by 17%. Initial measurements also showed paging after
the idle period; no hardware or sampling settings were changed for this patch.

The candidate workspace probe selected `fs_query` with `path: "."`. The ambiguous
“remind me later” probe asked for a concrete time instead of creating an
unconfirmed reminder. It also unnecessarily asked for reminder text already in
the request; the smaller prompt is not claimed to eliminate conversational
redundancy. These are limited model-behavior probes, not a general quality score
or end-to-end execution tests of the proposed actions.
The capability probe described the available domains without tool calls; it
reached the benchmark's 160-token output budget. Production output budgets are
unchanged by this patch.

## Reproduce

Save the previous complete system prompt, including a fixed reminder clock and
timezone, as a local text file. Then, from the repository root:

```bash
cargo run -q -p sail-dialogue --example voice_prompt_cases -- \
  /tmp/baseline-system.txt > /tmp/voice-prompt-cases.json
.venv-voice/bin/python scripts/voice/prefill_bench.py /tmp/voice-prompt-cases.json
```

Use an existing healthy model server at 127.0.0.1:8081; an alternate endpoint can
be passed as the second Python argument. The example uses a fixed Europe/Lisbon
clock at 2026-11-05T12:00:00Z. Match those facts in the baseline. The benchmark
disables prompt-cache reuse per request, compares before/after arithmetic, and
checks candidate workspace, reminder and capability behavior. Tool calls are
returned for inspection and are never executed by this script. It prints JSON
timings, usage and responses. Run while the host is otherwise idle; inspect the
answers and tool arguments as well as speed. The benchmark does not validate
physical microphone/playback or sustained GPU stability.
