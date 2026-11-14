# Talking to SAIL from Telegram

SAIL can answer you in Telegram: text, voice notes, skills, scheduled results and
approvals. This is phase P2 of the [external models, gateway, skills and scheduling plan](SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md).

## Setting it up

1. In Telegram, talk to **@BotFather**, send `/newbot`, and pick a name. BotFather gives
   you a token.
2. In SAIL, open **SAIL Agents › Messaging**, paste the token and press **Connect**. SAIL
   checks it with Telegram and shows the bot's @username. The token is stored only on this
   machine (`$SAIL_DATA/gateway/telegram.token`, readable by your user only) and is never
   shown again.
3. Send your bot a message. It replies with a pairing code. In **Messaging › Pairing
   requests**, check the code and press **Approve** (or type the code you see on your phone).

Nobody else can use your bot: an unknown sender gets a pairing code and nothing else, and
only you can approve it. Codes last one hour; at most three wait at once; after five wrong
codes, approvals are locked for an hour. You can also choose to **ignore** or **decline**
unknown senders. Group chats, channels and other bots are always ignored. Revoke a paired
person at any time.

## Using it

- Each chat is one SAIL conversation, with the same memory rules as the web app, until
  you send `/new`.
- **Voice notes** are transcribed on this machine by SAIL's local Whisper; SAIL first
  shows what it heard. (Needs the system's `libopus`; nothing is sent to a speech
  service.)
- **Commands:** `/new`, `/stop` (ends the running answer), `/status`, `/jobs`
  (scheduled jobs), `/skills`, `/<skill> …` (e.g. `/weekly_report this week`),
  `/approve <id>`, `/deny <id>`, `/help`.
- **Approvals:** when SAIL needs your OK for an action, it sends a card naming the action,
  with **Approve** / **Deny** buttons and an id for `/approve <id>`. Approving gives the
  same single-use permission as the web app's card, and SAIL then carries on. The most
  sensitive actions (critical risk) can't be approved from chat; SAIL tells you to use
  the web app.
- **Scheduled results** that are set to "Show in SAIL" also arrive in your paired chats.

## How delivery works

There is no inbound port: SAIL asks Telegram for new messages (long polling) and remembers
how far it read, so each message is handled once, even across a restart. Replies go through
an outbox: each reply part has a stable key, so it is sent at least once and never twice;
failures are retried with back-off (5 s, 30 s, 2 min, 10 min, then every 30 min) and dropped
after 20 tries or a day. Long replies are split at paragraph boundaries; formatting is sent
as plain text.

## Where things are

| What | Where |
| --- | --- |
| Token, pairing, chats, outbox (all `0600`) | `$SAIL_DATA/gateway/` |
| Bot API types, pairing, commands, outbox, text | `crates/sail-flow/src/gateway/` |
| Service (polling, sessions, approvals, voice) | `crates/sail-dialogue/src/gateway_service.rs`, `server.rs` (`DaemonChat`) |
| Voice notes (OGG/Opus via `libopus.so.0`) | `crates/sail-dialogue/src/ogg_opus.rs` |
| API (`/api/agents/gateway`, owner token) | `crates/sail-dialogue/src/gateway_http.rs` |
| UI | `crates/sail-dialogue/web/gateway.js` |

## Not done

Other platforms, group chats, photos and files, and webhooks.
