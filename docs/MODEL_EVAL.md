# Model eval

`sail_model_eval` scores the model behind a llama-server endpoint on SAIL's own work. Use it
before changing the resident model ([SAIL Mind: one resident model](SAIL_MIND.md#one-resident-model)).
Run it once for the current model as the baseline, then once for each candidate.

Forge's eval (`sail_forge_eval`) measures coding solve rate. This eval covers what a model
change puts at risk elsewhere: voice tool calls, the Arena Learn judge, answers grounded in
remembered facts, and Moodle extraction.

The eval sends the **same body as Mind** (`sail_common::request_body::chat_body`), so its
thinking switch, sampling and token budgets are what SAIL really uses. The model family comes
from llama-server's `/props` `model_path` (the weights file name), falling back to
`--model-file`, then the served model id. The report header shows the family and whether it
can think.

## Suites

| Suite | Cases | What passes |
|---|---|---|
| `tools` | 32 | Voice system prompt, clock context, and the tool subset that `vlm::selected_tool_definitions` offers for that message (none for capability questions, as in voice). The first call must be an accepted tool, with JSON-object arguments that have the schema's required fields and the expected values (for example, a reminder in 20 minutes is due at `10:20`). The six chat cases must be answered without a tool. |
| `judge` | 20 | Arena Learn's real free-recall judge (`agent::judge::judge`, same prompt, schema and retry) on question/rubric/answer triples with known verdicts: correct, partial and wrong answers, vague answers, one injection attempt and one answer in Portuguese. |
| `grounded` | 16 | A question with a voice memory block (same header and wrapper as production) and tools off. Answerable cases need the remembered value. Unanswerable ones need an explicit "I don't know" and must not guess the trap value. One case has an injected fact. |
| `extract` | 12 | Moodle-style announcements and file snippets in Portuguese and English, run through the real extraction prompt and validator (`sail_moodle::extract::build_prompt`, `parse_reply`). The validated items must contain the expected kind, date and (when stated) time, and nothing carrying a trap date. Cases include a postponed test (`12/11` → `19/11`), a weekday plus date, a Sunday `23h59` deadline, a cancelled class, a quiz window, registration, a presentation, a February date in the next academic year, and English announcements. |

Cases live in `crates/sail-dialogue/src/model_eval.rs`. Grading is deterministic and uses only
the visible `message.content` and `tool_calls`; `reasoning_content` is never graded. A case
whose last request ends at the output limit with no answer is marked `out of budget` and
fails. The abstention check is a phrase list, so read the failures in the `.md` report before
trusting a close result.

The `extract` suite uses the prompt's own `max_output_tokens` (the value `moodle_model.rs`
sends) and the authoring temperature. A unit test checks that this budget fits
`ResourceLimits::default().max_output_tokens`, so an extraction that Mind would reject cannot
be scored as if it ran.

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
tools,judge,grounded,extract`, `--only <id,id>`, `--repeat <n>` (voice samples at temperature
0.4, so repeat to reduce noise), `--reasoning <off|N>` (default `off`), `--model-file <name>`
(weights file when `/props` has no `model_path`), `--out <dir>` (default `.local/model-eval`).
Each run writes `<label>.json` and `<label>.md`.

`--reasoning N` sets a thinking budget of `N` tokens on every case, including the tool, judge
and grounded suites. `0` means off; values above `MAX_REASONING_BUDGET` are clamped by the
protocol. The budget is in the report header and JSON. At local speeds every 1000 thinking
tokens is about a minute, so a thinking run takes much longer.

The report includes llama-server's own prompt and generation speed (median tokens/s), request
p50/p95 wall time, and the RSS of running `llama-server` processes. The **Cost** table adds,
per suite, the median characters of `reasoning_content`, the median wall time and how many
cases ended with `finish_reason: length`. Each case in the JSON records `completion_tokens`,
`reasoning_chars`, the reasoning-token estimate (`completion_tokens_details.reasoning_tokens`
when reported, else `completion_tokens` minus a characters/4 estimate of the answer),
`finish_reason` and `out_of_budget`.

## Comparing thinking off against on

Run the same model twice, once direct and once with a budget, then compare. Mind stays off the
endpoint while the eval runs, so start with the model already served; Claude runs the live
comparison because 8081 is the owner's running SAIL.

```bash
sail_model_eval --label qwen3-direct
sail_model_eval --label qwen3-think-512 --reasoning 512 \
  --compare .local/model-eval/qwen3-direct.json
```

The gate line names both thinking settings. Read the **flips** before trusting a small score
change: the report marks cases that pass→fail and fail→pass so a two-point drop is visible
without diffing both JSON files. Adopt a budget only where a suite gains more than it loses,
and only if the added wall time is acceptable for that background job.

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

## Results: thinking on this host (2026-11-09)

Qwen3.6-35B-A3B Q4_K_M, 16K context, RX 5500 Vulkan, each case run once. "Off" is round 6's
code: the model-card sampling plus the voice grounding and selection fixes.

| Budget | tools | judge | grounded | extract | Request p50 / p95 |
|---|---|---|---|---|---|
| before round 6 (2026-11-07) | 29/32 | 20/20 | 14/16 | n/a | 10.3 s / 27.1 s |
| off | **32/32** | 20/20 | **16/16** | 11/12 | 10.3 s / 26.7 s |
| 512 | not run | 19/20 | 15/16 | 12/12 | 39.5 s / 44.2 s |
| 1024 | not run | 20/20 | 14/16 | 11/12 | 52.2 s / 72.2 s |

How to read it:

- **Thinking doesn't pay here.** It makes each request 4–5× slower and gains nothing a single
  run can tell apart from noise.
- The one extract gain (`x11`, "lab moved to Thursday", Lab vs ScheduleChange) is a labelling
  call, and it flipped back at 1024.
- With thinking on, grounded answers start writing `memory_search` calls as text when tools
  are off (`g10`, `g16`).
- So every `request_body::Job` default stays off. The `SAIL_THINK_*` overrides remain for
  trials.
- The gain in this round came from the voice fixes and sampling: tools +3 and grounded +2.
- Re-measure after a model or hardware change.
