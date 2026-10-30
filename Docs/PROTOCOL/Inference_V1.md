# Local inference protocol v1

The `sail-protocol` crate defines Rust/Serde requests, streaming events, responses,
model capabilities/status, resource limits and typed errors. The machine-readable
[JSON Schema](inference-v1.schema.json) contains each contract under `$defs`, with
`InferenceRequest` as its root. A [request example](../../crates/sail-protocol/examples/inference-request-v1.json)
is checked by the Rust tests. Every request, response, event, error and status
snapshot carries `version: 1`; unsupported request versions are rejected.

Requests reject unknown fields. Response/status consumers may tolerate additive
fields within v1, but must verify `version`. IDs are UUIDs. Image content must be
inline PNG/JPEG base64 in a user message, not a remotely fetched URL. Message roles
are `system`, `user`, `assistant`, and `tool`. A tool observation must match an
outstanding assistant tool-call ID. Tool schemas/calls describe proposals; Mind
never executes tools or accepts model-directed grants or model changes.

`MindService::infer(request, cancellation_token, optional_event_sender)` returns a
typed `InferenceResponse`. The bounded event channel emits `started`, `text_delta`,
`tool_delta`, then `completed`. An accepted request that fails attempts to emit
`failed`; if its consumer has disconnected or its channel is already full, the
returned typed error is authoritative. Validation/queue rejection returns an error
before a stream begins. Dropping the caller's future cancels its I/O and releases
its slot even when no explicit cancellation message can be delivered.

## Routing and authorization boundaries

The daemon registers its existing Gemma GGUF as `gemma4` with `local`, `gemma-4`
and the configured path as aliases. Embedders can register explicit Rust
`InferenceBackend` adapters with unique names/aliases. An omitted model selects the
first configured backend. Unknown names are rejected: there is no automatic model
download, promotion, provider substitution or cloud fallback.

The supplied adapter accepts only loopback HTTP(S) `/v1/chat/completions` endpoints.
Credentials, URL queries/fragments, redirects and proxies are disabled. `localhost`
is pinned to IPv4 loopback rather than delegated to an arbitrary resolver. Images
are checked for base64 size, PNG/JPEG dimensions (up to 4096x4096) and image count
before inference. Semantic vision requires an affirmative runtime capability;
a projector path by itself is not evidence that the running model supports vision.
Tools require the runtime's tools capability. Responses containing an unadvertised
function or a tool call when tool use was disabled are protocol errors.

These boundaries authorize local model access, not cameras, filesystem actions or
other tool execution. Those grants remain the responsibility of Guardian and the
calling tool/session layer. The legacy internal event bus is retained; this change
versions the inference interface rather than migrating every voice/media event.

## Errors and limits

`InferenceError` contains `version`, stable `code`, `message`, and `retryable`.
Codes are listed in the JSON Schema. Important cases:

| Code | Meaning |
|---|---|
| `unsupported_version` / `invalid_request` | Incompatible or malformed request |
| `forbidden_endpoint` | Non-loopback inference or remotely fetched image |
| `unknown_model` / `unsupported_capability` | No registered model or required vision/tool support |
| `resource_limit` | Input/image/context/output budget exceeded |
| `busy` / `queue_timeout` | Bounded queue full or queue wait expired |
| `cancelled` / `deadline_exceeded` | Caller cancelled or inference deadline expired |
| `backend_unavailable` / `backend_protocol` | Load/HTTP failure, malformed/truncated stream or invalid model output |
| `consumer_closed` / `shutting_down` | Event receiver disconnected or service is stopping |

Default limits: one active inference, eight queued, 128KiB textual input and
response bytes, two images up to 4MB each, 1024 maximum output tokens, a 30-second
queue deadline and a 180-second inference deadline. Request output defaults to
384 tokens. Embedders configure `ResourceLimits` when constructing the service.
Limits are hard checks, not latency or memory-throughput promises.

The adapter renders the real chat template through local `/apply-template` and
counts its text with `/tokenize`, reserving requested output headroom. For images,
it tokenizes a placeholder instead of sending image bytes twice, reserving 1280
context tokens per image (managed image maximum 1024 plus formatting reserve).
The native runtime remains the final context-window guard, particularly for
externally configured image models with different token behavior.

SSE decoding tolerates fragmented UTF-8, CRLF and usage-only events. Incomplete
streams and truncated tool calls are failures, never successful fallback answers.
Tool-call count and accumulated text/argument bytes are bounded. The service does
not replay a partially streamed response after a failure; callers may explicitly
retry a new request if appropriate.

## Measurements

`Usage` records native prompt/completion token counts, using null when completion
usage is unavailable; it does not substitute word counts for tokens. Prompt tokens
can use the local tokenizer count when the runtime omits usage. `Timings` separates
queue time, first model text/tool delta, and streamed inference duration. Aggregate
metrics include submitted/completed/failed/cancelled/rejected counts and current
active/queued requests. First-token timing excludes model startup and prompt-budget
validation; total service inference time includes that work. A cold model therefore
can have a much larger total latency than its stream timing.
