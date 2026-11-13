# Proactive intelligence and reasoning escalation

**Status:** first slice implemented (round 15) · **Owners:** Mind (escalation), Pulse (attention),
Discovery (detection and agenda), Dialogue (wiring and UI) ·
**Related:** [External models](SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md) (owner decision 12),
[Pulse](SAIL_PULSE.md), [Discovery](SAIL_DISCOVERY.md), #102 Attention Economics

SAIL should speak when it has something worth saying, and spend a stronger model's tokens only
when they are likely to change the outcome. This slice adds three things, each placed in the
subsystem that already owns that concern. None of them is a new service or a new tab:

1. **Escalation.** The local model can ask for a stronger model with `escalate_reasoning`.
   Trusted Rust code decides, within the owner's budgets.
2. **Attention.** Everything SAIL notices goes through one scorer in Pulse. The scorer decides
   whether to interrupt, suggest, digest, keep silently or drop.
3. **Initiative.** Discovery runs deterministic detectors and keeps an agenda. A Dialogue timer
   runs them every few minutes, and none of this calls a model.

## 1. Event flow

```text
           ┌─────────── observations (no model calls) ───────────┐
Moodle store ─┐                                                   │
Disk probe ───┼─► Dialogue proactive.rs ─► Discovery detect ─► Signal ─► Pulse attention
Escalation ───┘    (timer 300 s; and on                 │                 │ score − interruption cost
answers             finished escalations)               ▼                 ▼
                                               Discovery agenda     Interrupt │ Suggest │ Digest │ Silent │ Drop
                                               (commitments,            │        │         │
                                                open questions,         ▼        ▼         ▼
                                                revisit on evidence)   inbox   prompt    digest at
                                                                       card    brief     08:00/18:00
                                                                         │
                                         owner feedback: Useful · Not now · Later · Investigate · Dismiss · Mute
                                                                         ▼
                                                           learned (source, kind) preference
```

These stages stay separate:

| Stage | Where | Model? |
|---|---|---|
| Event detection | `sail-discovery/src/detect.rs` | no |
| Relevance scoring | `sail-pulse/src/attention/score.rs` | no |
| Reasoning | local model in conversation; `escalate_reasoning` when justified | local, then external if approved |
| Action planning | agenda items (`sail-discovery/src/agenda.rs`) and existing tools | no |
| Notification | Pulse attention store → web inbox, digest, conversation brief | no |

## 2. Reasoning escalation (Mind)

The code is in `crates/sail-mind/src/escalation/`, and its module doc is the normative spec.

### Configuration

The policy is read from `$SAIL_DATA/mind/escalation.json`. If the file is missing, escalation
is **disabled**. If the file is malformed, escalation stays off and SAIL logs an error; it never
falls back to a silent default.

```json
{
  "enabled": true,
  "providers": [
    {
      "id": "openrouter-sonnet",
      "adapter": "openai_compatible",
      "base_url": "https://openrouter.ai/api/v1",
      "model": "<provider>/<model-name>",
      "api_key_env": "SAIL_OPENROUTER_KEY",
      "kinds": ["coding", "research", "reasoning"],
      "input_price_micros_per_mtok": 3000000,
      "output_price_micros_per_mtok": 15000000
    }
  ],
  "daily_token_cap": 50000,
  "monthly_token_cap": 1000000,
  "daily_cost_cap_micros": 1000000,
  "monthly_cost_cap_micros": 10000000,
  "per_task_max_tokens": 8000,
  "cooldown_secs": 21600,
  "cache_ttl_secs": 604800,
  "route_threshold": 0.45,
  "min_local_attempts": 1,
  "min_local_attempts_coding": 2,
  "allow_voice": false
}
```

- The config names an environment variable, never a key. A missing key makes the provider
  "unavailable"; SAIL never invents an answer.
- Prices are in micro-units of the owner's currency per million tokens. They are only used to
  enforce cost caps before a call is made.
- A cap of `0` means no cap of that kind.

**Vendor CLIs (#449).** A provider can also be the owner's signed-in Claude Code, Codex or
Gemini CLI, using their subscription instead of an API key:

```json
{ "id": "claude-sub", "adapter": "claude_cli", "model": "", "kinds": ["coding", "reasoning"],
  "command": "/home/<owner>/.local/bin/claude", "cli_unverified_ok": false }
```

- `adapter` is `claude_cli`, `codex_cli` or `gemini_cli`. `command` defaults to `claude`,
  `codex` or `gemini` on `PATH`. An empty `model` uses the CLI's default.
- Each run happens in a fresh empty directory, with a cleared environment (no `SAIL_*`
  variables) and the prompt on stdin. Flags are pinned so the CLI runs non-interactively,
  read-only and without tools.
- Only the CLI's text answer is used. A rate-limit reply blocks that provider until its
  reset time. A CLI that is missing or not signed in is "unavailable", never a fake answer.
- **Version gate.** The flags are verified per CLI version on this host. An unverified
  version is refused unless `cli_unverified_ok` is `true`.

### Policy

The local model calls `escalate_reasoning` with these fields:

- `kind`, `question` and `justification`;
- `expected_benefit` and `token_budget`;
- optionally `importance`, `complexity`, `local_confidence` and `local_attempts`.

The model never sets the origin. Trusted code marks a call made during a live voice turn as
`VoiceTurn`.

1. **Cache and dedup.** The question is fingerprinted as sha256 of its kind and the normalized
   question. An equivalent question answered within `cache_ttl_secs` reuses that answer.
2. **Ordered checks.** The first failure becomes the denial reason, and the model and the owner
   see it verbatim.
   - escalation disabled;
   - a voice turn;
   - the per-task budget;
   - too few local attempts;
   - the daily or monthly token or cost caps;
   - cooldown, or the same question already in flight;
   - a route score below the threshold;
   - no provider with quota.
3. **Adaptive routing.**

   ```text
   base  = 0.35·complexity + 0.25·(1 − local_confidence) + 0.25·importance + 0.15·kind_prior
   score = base · (0.5 + improved_rate)
   ```

   `improved_rate` is a Laplace-smoothed share of past escalations of that kind that the owner,
   or a measurable check, marked as having improved the outcome. A kind whose escalations keep
   not helping falls below the threshold, so tracking outcomes refines routing by itself.

   **Automatic outcomes (#450).** Two checks record outcomes without asking the owner. The
   owner's own rating always wins over a check.
   - `adopted_in_reply`: the turn's final reply is compared with the answer it received. If
     the reply reuses at least 30% of the answer's content words, the answer counts as
     `improved`; below 8%, as `no_change`. Anything in between, or a short answer, is not
     judged.
   - `forge_tests`: an escalation the model tied to a Forge job (`forge_job_id`) counts as
     `improved` once that job reaches review or completes, and as `no_change` if it ends
     blocked.
4. **Egress guard.** Before anything leaves the host, these are replaced with stable
   placeholders, and the reply is re-hydrated locally:
   - e-mail addresses, phone numbers and IP addresses;
   - home paths;
   - long digit runs;
   - known names;
   - credential patterns (Guardian's redactor).

   If redaction fails, nothing is sent. Secrets are removed, not mapped, so they never come
   back.
5. **Asynchronous.** An approved escalation becomes a durable job in
   `$SAIL_DATA/mind/escalation.db`. A typed turn may wait up to 20 s; otherwise SAIL carries on
   with its own answer, and the stronger model's answer arrives in the attention inbox. Pending
   jobs resume after a restart, at most 3 times. After a restart the placeholders stay in the
   answer, because the private values are never persisted.
6. **Observability.** The store logs every decision, denials included, with its route score. It
   also keeps usage rows (tokens and cost per provider, day and month), rate-limit blocks and
   outcomes. All of it is visible at `GET /api/mind/escalation/status`.

External output is untrusted text. The model is told to verify it and to name the answering
model. Nothing in it is executed.

## 3. Attention (Pulse)

The code is in `crates/sail-pulse/src/attention/`, and the module doc is the spec. Scoring is
deterministic:

```text
value = 0.35·max(urgency, deadline_pressure) + 0.25·relevance + 0.20·novelty + 0.20·confidence
score = value · (0.6 + 0.8·learned)          learned ∈ (0,1), per (source, kind), decays over 30 days
cost  = 0.2 normal · 0.6 voice/busy · 0.9 focus · 1.0 quiet hours
```

| Delivery | When | Where it shows |
|---|---|---|
| Interrupt | `score − cost ≥ 0.6`, within 2 per hour, autonomy ≥ Balanced | inbox card at once |
| Suggest | `score ≥ 0.45`, within 6 per day | the "could raise" brief in the next conversation's first two turns |
| Digest | `score ≥ 0.25` | batched at the digest times (08:00 and 18:00 by default), at most 12 items |
| Silent | below that | kept for preparation and the agenda, not shown |
| Drop | duplicate in the cooldown (12 h), muted kind, not novel, or stale | stored for observability only |

Quiet hours (Pulse settings) block interrupts. The only exception is an owner commitment with
urgency ≥ 0.95, such as an exam within two hours. Learning moves a score only within 0.6–1.4×
and never bypasses a budget, so SAIL cannot optimize for engagement.

### Autonomy controls

`POST /api/initiative/settings` takes an `AttentionSettings` value:

| Level | Behaviour |
|---|---|
| `off` | No initiative. Only owner commitments, and only in digests. |
| `quiet` | Prepare silently and digest; never interrupt or bring things up. |
| `balanced` (default) | Interrupt rarely, suggest in conversation, digest the rest. |
| `proactive` | Both thresholds 0.1 lower. Idle-time curiosity is allowed (follow-up). |

`focus_until` ("Don't interrupt until …") raises the interruption cost to 0.9 until that time.
The thresholds, budgets, cooldown and digest times can all be configured.

### Lightweight interactions

`POST /api/initiative/items/:id/feedback` accepts:

- `{"feedback":"useful"}`;
- `{"feedback":"not_now"}`, which moves the item to the digest;
- `{"feedback":"later","until":"<RFC 3339>"}`, which snoozes it (within 60 days);
- `{"feedback":"investigate"}`, which marks it for investigation and opens an agenda investigation. That investigation's match terms come from the title, so new evidence brings it back as a revisit;
- `{"feedback":"dismiss"}`;
- `{"feedback":"mute"}`, which mutes that source and kind.

## 4. Initiative and the agenda (Discovery)

**Detectors.** `sail-discovery/src/detect.rs` holds pure functions of an observation and the
current time. Each stage has its own dedupe key, so a detector never repeats itself.

| Observation | Signals |
|---|---|
| School item from the Moodle store (exam, test, delivery…) | three stages: 14 days (prepare), 3 days (plan), 24 h (now); urgency rises; weight in the grade raises relevance; provenance is the owner's commitment |
| School change from the last 7 days | grade released, deadline moved, announcement, new assignment, event or file; structural noise is ignored |
| Finished escalation | "model X answered …", to the inbox |
| Data disk below 10% (or 3%) free | a system alert |
| New evidence matching an open agenda question | "new evidence on …", marked as a hypothesis |
| GitHub activity, polled through `gh` at most every 15 min (#451) | CI failed on the owner's PR, review requested, issue assigned, PR merged, release on a watched repo (`SAIL_GITHUB_WATCH`); one item per fact (a new commit is a new fact) |
| A Curiosity Engine finding (#453) | "Looked into: …", SAIL's unverified notes with sources, to the digest |

**Agenda.** `sail-discovery/src/agenda.rs` (`$SAIL_DATA/discovery/agenda.db`) holds topics,
investigations, commitments, potential actions and open questions. Each item records its
provenance:

- `user_commitment`;
- `observation`;
- `suggestion`;
- `hypothesis`.

That way SAIL's own ideas are never presented as something the owner said. Items are short
semantic records, not transcripts, and there are at most 200 open ones. When there are too
many, hypotheses are dropped first; commitments are never dropped automatically. Re-detecting
an item does not reopen one the owner closed.

**Conversation brief.** In the first two turns of a conversation, the prompt gains at most 700
characters. It lists commitments due within three days and the top suggestions, each tagged
with its provenance. The model is told the items are optional. This is how SAIL "prepares
conversations" without growing every prompt.

**Curiosity Engine (#453).** "Investigate further" puts an investigation on the agenda. Under
Proactive autonomy, open questions count too. After a tick, one bounded investigation may run
in the background, only when:

- autonomy is Balanced (investigations only) or Proactive (open questions too);
- there is no live voice turn, no quiet hours and no focus;
- the owner has been idle for 20 minutes;
- fewer than 3 runs have happened today (UTC).

How a run works:

- It picks owner-requested investigations first, then whatever was looked into least
  recently. An item is not revisited within 24 hours.
- The local model runs at Background priority for at most 4 steps, 2 000 tokens and 4 minutes.
  The steps are read-only: memory search (Guardian's Dialogue grant) and school search.
- It may ask a stronger model only through the escalation policy, as `Origin::Background`.
- A voice turn cancels it at once.

Every run, including failed and cancelled ones, leaves a budget receipt in
`$SAIL_DATA/discovery/curiosity.db`. A conclusive run produces a `hypothesis` digest item that
starts "SAIL's unverified notes:", so it never reads as the owner's words.
`SAIL_CURIOSITY=0` turns it off.

**Configuration:**

- `SAIL_PROACTIVE=0` turns the loop off.
- `SAIL_PROACTIVE_TICK_SECS` sets the period (60–3600 s, default 300).
- `POST /api/initiative/tick` runs one tick now.

## 5. Resource and safety properties

- **No model polling.** Ticks are deterministic. A model runs only in a conversation, in an
  approved escalation, or in an admitted, receipted Curiosity run.
- **Bounded data.** Background loops are bounded; the stores are pruned (attention after 30
  days, escalation usage after 90) and text fields are capped.
- **Permissions.**
  - The owner token guards every route.
  - External egress needs the owner's policy file, a provider, caps and redaction.
  - The tool goes through Guardian's tool broker as a mutating invocation.
- **Durability.** Attention items, the agenda and escalation jobs survive restarts.

## 6. HTTP

| Route | Returns |
|---|---|
| `GET /api/initiative/inbox?limit=` | attention items not dismissed |
| `GET /api/initiative/deliverable` | interrupts and finished snoozes, marked delivered |
| `POST /api/initiative/items/:id/feedback` | the updated item |
| `GET /api/initiative/digest`, `POST /api/initiative/digest/release` | the last released digest, or a new one |
| `GET/POST /api/initiative/settings` | `AttentionSettings` |
| `GET /api/initiative/stats` | counts by delivery, state and feedback, and the last tick report |
| `GET /api/initiative/agenda` | open agenda items |
| `POST /api/initiative/tick` | a tick report |
| `GET /api/initiative/curiosity` | Curiosity Engine state, limits, why it would not run now, and recent receipts |
| `GET /api/mind/escalation/status` | caps, usage, outcomes and recent decisions |
| `GET /api/mind/escalation/jobs` | recent jobs, with the answering model |
| `POST /api/mind/escalation/jobs/:id/outcome` | records `improved`, `no_change` or `worse` |

## 7. Follow-up work

Delivered in round 16:

- CLI providers (#449);
- automatic outcomes and the usage view under Settings → Models (#450);
- the GitHub detector (#451);
- the Curiosity Engine (#453).

Still tracked as issues:

- the calendar detector, once the calendar (#335) lands (#452);
- the Research Watcher (#454);
- the Reflection Engine (#455);
- spoken and push delivery, and Sense-based busy and focus inference (#456, #102);
- the Conversation Planner (#457);
- the Friction Detector (#84);
- Opportunity Radar (#83).
