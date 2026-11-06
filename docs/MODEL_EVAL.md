# Model eval

`sail_model_eval` scores the model behind a llama-server endpoint on SAIL's own work. Use it
before changing the resident model ([SAIL Mind: one resident model](SAIL_MIND.md#one-resident-model)).
Run it once for the current model as the baseline, then once for each candidate.

Forge's eval (`sail_forge_eval`) measures coding solve rate. This eval covers what a model
change puts at risk elsewhere: voice tool calls, the Arena Learn judge, and answers grounded
in remembered facts.

## Suites

| Suite | Cases | What passes |
|---|---|---|
| `tools` | 32 | Voice system prompt, clock context, and the tool subset that `vlm::selected_tool_definitions` offers for that message (none for capability questions, as in voice). The first call must be an accepted tool, with JSON-object arguments that have the schema's required fields and the expected values (for example, a reminder in 20 minutes is due at `10:20`). The six chat cases must be answered without a tool. |
| `judge` | 20 | Arena Learn's real free-recall judge (`agent::judge::judge`, same prompt, schema and retry) on question/rubric/answer triples with known verdicts: correct, partial and wrong answers, vague answers, one injection attempt and one answer in Portuguese. |
| `grounded` | 16 | A question with a voice memory block (same header and wrapper as production) and tools off. Answerable cases need the remembered value. Unanswerable ones need an explicit "I don't know" and must not guess the trap value. One case has an injected fact. |

Cases live in `crates/sail-dialogue/src/model_eval.rs`. Grading is deterministic. The
abstention check is a phrase list, so read the failures in the `.md` report before trusting
a close result.

A tool case whose expected tool was not in the offered subset is marked `not offered`. That
is a gap in tool selection, not a model fault, and it fails for every model alike.

## Running

The eval calls the endpoint directly (OpenAI chat API, the same body Mind sends), so host
admission and model lifecycle do not affect the score. It shares the model with SAIL while
it runs, so run it when voice is idle.

```bash
CARGO_TARGET_DIR=/srv/sail/cargo-target cargo build -p sail-dialogue --bin sail_model_eval
/srv/sail/cargo-target/debug/sail_model_eval --label baseline     # current model
```

Options: `--url` (default `SAIL_LLM_URL`, else `http://127.0.0.1:8081`), `--suites
tools,judge,grounded`, `--only <id,id>`, `--repeat <n>` (voice samples at temperature 0.4, so
repeat to reduce noise), `--out <dir>` (default `.local/model-eval`). Each run writes
`<label>.json` and `<label>.md`.

The report includes llama-server's own prompt and generation speed (median tokens/s), request
p50/p95 wall time, and the RSS of running `llama-server` processes.

## Comparing a candidate

Only one large model fits in RAM, so stop SAIL's model server first, then start the candidate
on the same port with the same context size:

```bash
/home/main/llama.cpp/build-vulkan/bin/llama-server -m /srv/sail-data/models/<candidate>.gguf \
  --jinja --host 127.0.0.1 --port 8081 --ctx-size 16384 --threads 5 --parallel 1
/srv/sail/cargo-target/debug/sail_model_eval --label <candidate> \
  --compare .local/model-eval/baseline.json
```

**Adoption gate:** a candidate is *adoptable* when no suite scores more than 5 points below the
baseline. The report also shows the RSS difference. A candidate that passes the gate and frees
enough RAM for a larger context (or faster prefill) is worth a trial as `SAIL_MODEL_PATH`; one
that fails stays out, whatever its size. Then run Forge's eval too, since Forge uses the same
model.

Run both models with the same `--ctx-size`. Mind may launch with a smaller window when RAM is
tight (it logs "scaled context window down"), and a smaller window changes speed and which
requests fit.
