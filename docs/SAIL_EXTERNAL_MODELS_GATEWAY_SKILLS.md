# External models, messaging gateway, skills and scheduling

**Status:** decided design; P1 escalation core (policy, ledger, redaction, OpenAI-compatible adapter, `escalate_reasoning`) implemented — see [Proactive intelligence](SAIL_PROACTIVE.md); P4 skill loader, index, `skill:view` and sandboxed Hermes plugins implemented — see [Hermes plugins and skills](SAIL_HERMES_PLUGINS.md) · **Decided:** 2026-11-03 by the owner ·
**Vision:** [decisions 12–15](SAIL_MASTER_VISION.md#decisions-requiring-explicit-migration-rather-than-assumptions) ·
**Reference:** [Hermes Agent](https://github.com/NousResearch/hermes-agent) (MIT) and the
[Agent Skills specification](https://agentskills.io/specification)

SAIL borrows four capabilities from Hermes Agent: using the owner's model subscriptions,
a messaging gateway, self-improving skills in the open skills format, and scheduled
work. It keeps what makes SAIL different: local-first operation, authority enforced by
Guardian at the OS/adapter boundary, memory that is validated knowledge rather than
stored transcripts, and receipts instead of claims. Each borrowed feature is placed in
the subsystem that owns it; none becomes a separate service or tab.

## 1. Owner decisions

| # | Decision |
| --- | --- |
| 12 | **External models by escalation.** SAIL may use the owner's ChatGPT (Codex CLI), Claude (Claude Code), Gemini (Gemini CLI) subscriptions and API keys (OpenRouter or any OpenAI-compatible endpoint). Work starts on the local model, voice, coding and research included. A task escalates to an external model only after the local attempts fail the task's measurable checks a set number of times, and only to a provider with quota left. The answering model is always shown. The owner can also ask for a provider explicitly. |
| 13 | **Messaging gateway, Telegram first.** The owner can talk to SAIL from a chat app. Other platforms follow behind the same adapter interface. |
| 14 | **Skills use the Agent Skills format** (`SKILL.md` folders), so skills move between SAIL, Hermes and other compatible agents. A skill never grants permission. |
| 15 | **Scoped, redacted memory for external models.** Only memory the owner marked shareable, at `Normal` sensitivity, may go to an external model; names and identifiers are replaced with placeholders; health, people and private items never leave. Every external request has an egress receipt. |

These amend vision invariant 6 (local ownership) and the §0.1 compute rule: external
inference is now an authorized integration, but never the default path, never silent, and
never a carrier for private memory.

## 2. Providers and escalation (Mind)

### 2.1 Providers

Each provider is an `InferenceBackend` beside the resident local model:

| Kind | How SAIL calls it | Credentials |
| --- | --- | --- |
| Local | Mind's llama-server (unchanged) | none |
| Codex (ChatGPT) | `codex exec` non-interactive, JSON output | the CLI's own sign-in (`~/.codex`) |
| Claude (Claude Code) | `claude -p` non-interactive, JSON output | the CLI's own sign-in |
| Gemini | Gemini CLI non-interactive mode | the CLI's own sign-in |
| API key | OpenAI-compatible `/v1/chat/completions` (OpenRouter, OpenAI, Anthropic-compatible gateways) | Guardian vault |

Subscriptions are used **only through the vendor's own CLI**, in its documented
non-interactive mode. SAIL never extracts or reuses their tokens, so it stays within each
vendor's terms. Each provider is opt-in and can be disabled.

The vendor CLIs are agents with their own file and shell tools. SAIL uses them as **text
models only**:

- They run in Guardian's sandbox, in an empty temporary directory, with network allowed
  but no access to the repository, SAIL data or other credentials.
- Their own tools are disabled through their documented permission flags. Exact flags are
  verified per CLI version when the adapter is built; a CLI whose tools cannot be disabled
  is not offered.
- Their output is untrusted text. Anything SAIL then does with it goes through the normal
  typed tool path and Guardian.

### 2.2 Quota

A durable usage ledger, like Forge's review ledger, records each external call:
provider, purpose, time, outcome and reported usage. A provider is **out of quota** when
its CLI or API reports a rate or usage limit (until the reset time it reports, or a
conservative back-off if none), or when it reaches an owner-set daily cap. Out-of-quota
providers are skipped, never retried in a loop. Forge's one-Codex-review-per-issue rule is
unchanged and separate.

### 2.3 Escalation policy

Every escalatable task kind declares its checks, the number of local attempts, and an
ordered provider list:

| Task | Measurable checks | Default local attempts |
| --- | --- | --- |
| Forge coding | Issue tests, deterministic gates, mutation gate | 2 jobs |
| Research | Sources found and cited, question answered, verification pass | 2 |
| Agent tool plans | Plan validates, steps succeed, verification pass | 2 |
| Voice and dialogue turns | none: never escalated automatically (latency, privacy) | — |

The local model always runs first. Only after the checks fail the set number of times
does Mind pick the first provider with quota. The attempt history, the reason for
escalating and the answering model are recorded and shown (UI, gateway reply, PR body).
"Ask Claude: …" or picking a model escalates immediately. Escalated work is background
work: admitted by the scheduler, paused by live voice.

## 3. Egress guard: memory and external models (Guardian + Arena)

1. **Purpose.** `MemoryPurpose::External { provider }`. Guardian maps it to a grant with
   `max_sensitivity: Normal` and only the scopes the owner marked shareable. Health,
   Friends, and anything `Private`/`Sensitive` are never in it.
2. **Composition.** The Context Composer selects the smallest relevant set under that
   grant, as for local tasks.
3. **Redaction.** Before sending, names of known entities (people and places from Arena),
   e-mail addresses, phone numbers, account and card numbers, IP addresses, file paths
   under the home directory and anything the vault redactor recognizes are replaced with
   stable placeholders (`[PERSON_1]`, `[EMAIL_1]`). The reply is re-hydrated locally. This
   applies to the owner's prompt text too; secrets are always removed.
4. **Receipt.** Each external request writes an egress receipt: provider, purpose, the IDs
   and hashes of the memory items included, redaction counts, byte counts, and the
   outcome. No prompt text is stored. The owner can see what left the machine and when.
5. **Fail closed.** If the grant, composition or redaction fails, nothing is sent.

## 4. Messaging gateway (Relay, authorized by Guardian)

- **Process.** One gateway task inside the SAIL daemon with a platform adapter per
  channel. Telegram uses the Bot API with long polling, so no inbound port is opened.
- **Default deny.** An unknown sender gets a one-time pairing code (8 characters from an
  unambiguous alphabet, valid 1 hour, at most 3 pending, 1 request per sender per
  10 minutes, lockout after 5 failed approvals). The owner approves it in the SAIL web UI.
  Approved senders are kept in Guardian and can be revoked there. Unpaired senders can be
  ignored or declined instead.
- **Sessions.** One chat is one SAIL conversation until `/new`. Its context follows SAIL's
  memory rules: bounded turn context, no permanent transcript, learning only with consent.
- **Commands.** `/new`, `/stop`, `/status`, `/model`, `/approve`, `/deny`, `/skills`,
  `/<skill>`, `/jobs`.
- **Approvals over chat.** A Guardian approval becomes a chat card naming the exact action.
  `/approve` mints the same single-use, exact-request approval as the web UI. It times out
  to deny. The `Sensitive` class (destructive or irreversible actions) is never approvable
  from chat; it needs the web UI.
- **Voice notes** are transcribed by SAIL's local Whisper, never by an external service.
- **Delivery** goes through Continuity's outbox: at-least-once with dedup, so a restart
  neither loses nor duplicates a reply. The bot token is stored in the Guardian vault.

## 5. Skills (Flow / Skillsmith)

- **Format.** [Agent Skills](https://agentskills.io/specification): a folder named after
  the skill with `SKILL.md` (YAML frontmatter `name`, `description`, optional `license`,
  `compatibility`, `metadata`, `allowed-tools`) and optional `scripts/`, `references/` and
  `assets/`. Validated on load against the spec's rules. Hermes skills import unchanged.
- **Where.** `$SAIL_DATA/skills/<name>/`, versioned, with history kept for rollback.
- **Progressive disclosure.** The model's prompt carries only the index (name and
  description). A `skill:view` tool loads the body, then individual resource files on
  request.
- **Permissions.** `allowed-tools` only *requests* tools. Guardian still decides, and a
  skill never widens a grant. Scripts run only through the sandboxed runner, never on the
  host.
- **Self-improvement.** After a task that took several steps, recovered from errors or was
  corrected by the owner, a background review on the local model proposes a skill or a
  patch. Proposals are **staged until the owner approves them** (Hermes makes this
  optional; SAIL makes it the default). Skills can be pinned, diffed, reverted and
  deleted.
- **Imports** from URLs or other agents are quarantined, hashed, scanned, and enabled only
  after approval.
- **Use.** `/<skill>` from the gateway or the web UI, or the model chooses one from the
  index.

## 6. Scheduling (Pulse, durable in Continuity)

Covers everything Hermes cron does, with SAIL's durability and attention rules:

- **When:** `in 30m`, `every 2h`, `daily at 7am`, `weekdays at 9am`, cron expressions and
  ISO times, each with a time zone. Event triggers too: a Forge job finishing, a Discovery
  finding, a file change in a granted folder.
- **What:** an agent prompt, a skill, a script with no model call (sandboxed runner), or a
  Flow workflow.
- **Delivery:** the originating chat, the web UI, a named gateway chat, several targets,
  or silent (result kept only).
- **Attention:** Pulse's quiet hours, snooze and "not now" apply to delivery.
- **Resources:** jobs are background work, admitted by the scheduler and paused by voice;
  model use follows the escalation policy.
- **Durability:** each run is a Continuity task with a lease. After downtime it catches up
  within a grace window. Committed side effects are not replayed.
- **Safety:** a scheduled run cannot create scheduled jobs unless the owner allows it. Its
  approvals default to deny, since nobody is present. Repeated failures notify the owner.
  A misconfigured job (missing provider, unreachable target) is blocked before any model
  call.

## 7. Memory: what SAIL takes from Hermes

Taken:

- **A small always-in-context profile.** A bounded core-facts block (about 1,400
  characters), built from pinned and high-salience claims and frozen for the session so
  the prompt cache holds. It replaces nothing: retrieval still supplies task facts.
- **Optional owner review of memory changes** (`memory write approval`): model-proposed
  change sets wait in a queue the owner can approve or reject, in addition to the
  deterministic validator.
- **Injection and exfiltration scanning** of candidate memory (invisible Unicode,
  instructions aimed at the model, exfiltration URLs) before validation.
- **Background memory review on a cheaper model:** the local model, in idle windows.
- **No false saves.** Hermes warns that its model can claim it saved something without
  doing so. SAIL replies "remembered" only from the commit receipt.

Not taken: storing raw sessions for full-text search, and agent-curated free-text memory.
Both conflict with [§4 of the vision](SAIL_MASTER_VISION.md#4-memory-the-most-important-architectural-correction).

## 8. Delivery plan

Each phase is one vertical slice with its own PR and tests.

| Phase | Scope | Accepted when |
| --- | --- | --- |
| P1 | Providers (Codex, Claude, Gemini CLIs and an OpenAI-compatible key adapter), quota ledger, egress guard, escalation for Forge | A Forge job that fails its local attempts is retried on an external model with quota. The PR names the model. The egress receipt shows only shareable, redacted context. An out-of-quota provider is skipped. A missing CLI means "unavailable", never a fake answer. |
| P2 | Telegram gateway: pairing, sessions, commands, approvals, local transcription of voice notes, outbox delivery | An unpaired sender gets a code and nothing else. After approval in the web UI, a text and a voice note get answers. `/approve` executes exactly one pending action. A restart neither loses nor duplicates replies. **Done (round 22):** pairing with owner approval and lockout, one conversation per chat, `/new /stop /status /jobs /skills /<skill> /approve /deny`, approval cards (never for critical risk), voice notes through local Whisper, outbox with dedup and back-off, scheduled results to Telegram ([details](SAIL_TELEGRAM_GATEWAY.md)). |
| P3 | Scheduler: schedule grammar, job store on Continuity, delivery targets, quiet hours, catch-up | "Every weekday at 9" runs once per day across a restart, delivers to Telegram, respects quiet hours, and pauses for voice. **Done (round 21):** grammar with time zones and DST, job store with once-per-slot starts and crash recovery, 2-hour catch-up, quiet hours hold delivery, no starts during voice, model-proposed jobs need approval, Schedule section ([details](SAIL_SCHEDULE.md)). Plugin hooks and per-plugin Python environments also landed ([details](SAIL_HERMES_PLUGINS.md#hooks)). **Open:** Telegram delivery (with P2), event triggers, runs as Continuity tasks. |
| P4 | Skills: loader and validator, index and `skill:view`, `/<skill>`, staged self-created skills, import with quarantine | A Hermes skill imports and runs. A self-proposed skill appears only after approval. `allowed-tools` cannot widen a grant. **Done:** loader, validator, index, `skill:view`, Hermes skills and plugins with hash-bound approval and a bubblewrap sandbox, the Market (install from Hermes' public skill and plugin catalogs with quarantine, scan and owner install) and staged self-proposed skills ([details](SAIL_HERMES_PLUGINS.md)). `/<skill>` from Telegram landed in round 22. |
| P5 | Memory: core profile block, review queue, injection scan | The profile block stays under budget and is frozen per session. A queued change commits only after approval. An injected instruction in a candidate is rejected. **Done (round 22):** a ≤1,400-character profile block from the owner's active normal-sensitivity claims (pins first), frozen per conversation; an injection/exfiltration/hidden-Unicode/secret scan on every proposed change set; an optional review queue (Memory › Review) whose saves still pass Arena's validator. |

**Next:** [Token economy](SAIL_TOKEN_ECONOMY.md) (proposed) narrows the providers to Claude and Gemini, reserves subscription capacity for the owner, sends compact briefs instead of whole tasks, moves Forge review off Codex, and turns escalations into cached answers, skills and distillation data.

Open risks: CLI output formats and permission flags change between versions (pin and
verify per adapter); vendor quotas are not exposed uniformly (rely on reported limits and
owner caps); redaction cannot be perfect (scope and sensitivity rules are the primary
protection, redaction the second line).
