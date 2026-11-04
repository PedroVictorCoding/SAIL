# External provider implementation

Status: first P1 API slice, not the complete Hermes roadmap. No external provider is
enabled automatically in the daemon. The existing local model path stays unchanged.

## Implemented boundary

Mind's `external::ExternalBackend` implements `InferenceBackend` and supports explicit
`infer_authorized` calls. Construction takes a trusted transport, Guardian broker and
vault, quota ledger, resource scheduler, provider/model identity, daily call cap, and
trusted entity-name inventory. An external backend cannot be the first/default backend
in `MindService`.

This slice accepts exactly one user text message, no tools, images, system messages,
history, or retrieved memory. Guardian removes vault secrets, recognizable identifiers
(email, URLs, home paths, IPv4 addresses, numeric identifiers), and supplied entity
names. Hidden formatting controls are refused. Redaction is deliberately conservative:
it can replace ordinary numbers and does not promise to recognize every identifier.
Entity coverage is limited to the inventory supplied by trusted integration code.
The terminal demo has no Arena inventory, and requires owner inspection of its preview.
Nothing restores redacted identifiers into the provider's response in this slice.

Approval binds provider, complete endpoint, model, sanitized request digest, generation
parameters, and request identity. `external:infer` has Critical risk so Autonomous mode
does not silently authorize it. Wildcard learned tool permissions cannot authorize it.
A retry with changed parameters requires a new approval. Direct exact grants remain a
trusted owner-policy operation. Approvals expire under Guardian's existing policy.

Bridge's `external::OpenAiTextConnector` holds an opaque vault credential handle with
exact scope `external:<provider>:infer`. It accepts HTTPS endpoints without embedded
credentials, queries, or fragments, never follows redirects, never sends tools, bounds
response bytes, and refuses tool/function-call responses. HTTP loopback is available
only through the private test constructor. Provider errors never expose response bodies.

## Durable usage and receipts

`UsageLedger::open_default` requires a mounted `SAIL_DATA` root and stores
`mind/external-usage.sqlite3` there. There is no temporary or in-memory fallback.
Explicit-path construction is for tests and trusted embedders, who must supply a
protected directory. SQLite transactions serialize reservations across connections.

Every authorized dispatch reserves a daily slot before network transmission. Slots
count calls, not money or estimated tokens, and reset at UTC midnight. All reservations
consume slots, including rejected and interrupted calls. Request IDs are globally
unique in this ledger; reusing an ID never sends again. HTTP 429 persists the reported
Retry-After cooldown, or one hour when no reset is supplied. No automatic retry occurs.

Receipts contain provider/model, purpose, request hash, sanitized byte count, redaction
operations, timestamps, outcome, and reported token counts. They contain neither prompt
nor reply text. The redaction operation count includes one operation for the vault scan
when it changes the text; it is not a count of individual secret occurrences.

Lifecycle: Reserved → Dispatched → Completed/RateLimited/Failed. Cancellation before
dispatch records Cancelled; interruption, timeout, lost response, malformed response,
or a dropped caller after dispatch records Uncertain. Abrupt process death may leave
Reserved/Dispatched. Every such state retains its slot and blocks replay. Voice preempts
background transport; the task owner must reconcile uncertain calls rather than resend
them as though they definitely failed. Reported model identity prefixes the reply with
the configured provider. No network health or model context-window measurement is claimed.

## Owner-terminal demo

Requires an existing Guardian vault selected by `SAIL_VAULT_DIR`, a mounted data root,
and a valid Bridge credential named `external-<provider>` with exact scope
`external:<provider>:infer`. Credential provisioning uses the existing trusted vault
API; keys must never be passed as process arguments or included in prompts.

```sh
cargo run -p sail-bridge --example external_text -- \
  https://YOUR_PROVIDER/v1/chat/completions PROVIDER MODEL
```

Run from an interactive owner terminal. Enter a one-line prompt, inspect the sanitized
preview and destination, then type `send` to authorize that one request. Any other answer
cancels. The demo uses a ten-call UTC daily cap and prints the answering model and receipt.
It exercises a real HTTP adapter, not a mocked success fallback. It is not a replacement
for authenticated web settings or approval UI.

## Verification

```sh
cargo test -p sail-guardian --lib
cargo test -p sail-mind external --lib
cargo test -p sail-bridge external --lib
cargo check -p sail-bridge --example external_text
cargo clippy -p sail-bridge -p sail-mind -p sail-guardian --lib --examples -- -D warnings
cargo fmt --all -- --check
```

HTTP integration tests bind a local Axum endpoint and exercise the actual vault,
Guardian approval/token broker, scheduler, SQLite ledger, and HTTP connector. Their
provider responses are fixtures; they are not live paid-provider or subscription tests.
The test environment must permit loopback sockets. Tests cover denial, request mutation,
single-use authorization, default-provider rejection, quota/cooldown, redacted transport,
voice interruption, tool-call rejection, oversized responses, and durable restart/dedup.

## Remaining roadmap

- Finish P1: verified CLI sandbox/auth isolation, provider ordering, quota selection,
  Forge escalation after measured local failures, shareable Arena composition and egress
  policy, and authenticated daemon/UI integration. Vendor adapters must pin verified
  versions and documented text-only restrictions. References: [Codex CLI](https://developers.openai.com/codex/cli/reference/),
  [Claude CLI](https://code.claude.com/docs/en/cli-reference), and
  [Gemini headless mode](https://geminicli.com/docs/cli/headless/). Missing or unverified
  adapters must remain unavailable; no vendor credentials were exercised by these tests.
- P2: integrate the owner web-authentication prerequisite before Telegram pairing and
  remote approvals; implement Relay and durable inbound/outbound delivery.
- P3: extend the separately owned Pulse/reminder work after it lands.
- P4: build portable, quarantined, approved skills within Flow.
- P5: integrate profile/review/scanning with Arena after its current learning work lands.

This implementation does not import Hermes code or add a Python runtime. It reuses the
workspace's locked rusqlite 0.37 and regex 1.x dependencies (both MIT-compatible) and
existing HTTP, Guardian, and resource contracts.
