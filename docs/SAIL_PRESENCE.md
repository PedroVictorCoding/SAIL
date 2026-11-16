# SAIL Presence: being there when the owner is

**Status:** first slice implemented (round 26) · **Owners:** Pulse (presence estimate, routing),
Dialogue (sockets, hands-free gate, greeting), Discovery (task outcomes) · **Related:**
[Sense](SAIL_SENSE.md) (#12, presence is its first slice), [Proactive](SAIL_PROACTIVE.md),
[Pulse](SAIL_PULSE.md), [Android](SAIL_ANDROID.md)

SAIL already notices things (Discovery), decides whether they matter (Pulse attention) and can
speak. Presence decides **where** the owner is, so that an interruption is spoken on one device
instead of every open tab. It also lets a docked phone listen for SAIL's name, and greets the
owner after an absence.

## 1. What the owner gets

| Behaviour | How |
|---|---|
| **Dock mode** on the phone (Android → Home → Dock mode) | A full-screen Orb, clock, next reminder and running plan. The screen stays on while charging and dims in quiet hours. |
| **Hands-free** (dock, or the desk toggle next to the mic) | "SAIL, what's next today?" starts a turn. Speech that doesn't address SAIL is discarded. After SAIL finishes speaking, one follow-up within 8 s needs no name. |
| **Arrival greeting** after ≥ 45 min away | At most two clauses, from held announcements, the next event due within 12 h, or the project's next step. The text comes from a template, with no model call. |
| **One place per announcement** | Spoken on the active device if it can speak, else shown there. If you're away, it goes to Telegram (opt-in) or is held for the greeting. |
| **Finished work** | Plans and Course Factory jobs that succeed, fail or need you become Pulse signals. Forge jobs are already reported by Discovery. |
| **Settings → Presence** | Hands-free on this device, speak announcements, greet me when I come back, Telegram when away, away threshold, spoken-per-hour cap. |

## 2. Flow

```text
 clients ── POST /api/presence/heartbeat (30 s) ──► PresenceStore ── Arrived/Left ──┐
 (web, dock, phone)   visible · idle s · charging · voice_ready      (broadcast)     │
                                                                                     ▼
 Discovery / Moodle / tasks ─► Pulse attention (score) ─► Interrupt ──► presence loop (15 s)
                                                                          │  route(item, presence)
                         ┌───────────────────┬─────────────────┬──────────┴──────┐
                         ▼                   ▼                 ▼                 ▼
                 Speak on device      Show on device       Telegram        Hold → greeting
                 (announce + speech)  (announce card)      (opt-in)
```

- **Estimate** (`sail_pulse::presence::estimate`, pure). A surface is *active* when it has been seen in the last 90 s, is visible, and was used within `away_after_secs`. The least idle active surface wins; ties go web > dock > phone. With no active surface the place is `asleep` in quiet hours, else `away`.
- **Events** (`PresenceStore`). `Arrived` fires on a change from away/asleep/unknown to an active place, carrying the time since the owner *left*. Quiet hours starting or ending doesn't reset that clock. `Left` fires only when leaving an active place.
- **Route** (`sail_pulse::attention::route`, pure). Only `Interrupt` items are routed. Quiet hours never speak: an urgent override goes to Telegram if allowed, else it's shown or held. At most `max_spoken_per_hour` announcements are spoken.
- **Delivery** (`server/presence_hub.rs`). Routing peeks with `AttentionStore::pending_interrupts` and marks an item delivered only once it reaches a socket or Telegram. A device in the middle of a turn gets the card without speech. Speech carries the socket's epoch, so talking over it stops it like any reply.

## 3. Hands-free

Each voice socket identifies itself with `{"type":"surface","device_id","surface","hands_free"}`. With hands-free on:

1. Partial transcripts are not computed or sent.
2. Each finished utterance goes through `address::addressed` (or the follow-up window) **before** any transcript is emitted, logged, observed or sent to a model. Unaddressed speech ends with `{"type":"not_addressed"}`, with no text.
3. The follow-up window (8 s) opens when the client reports `playback_finished` for a fresh answer, not when generation ends, so SAIL's own voice can't count as a follow-up. It is used once.
4. A bare "SAIL" gets "Yes?" and opens the window after it is played.

**Measured on the host (2026-11-16, host STT worker, Kokoro speech as input):**

- **Cost.** About 4.2 CPU-seconds per utterance (≈ 0.85 s wall on ~5 cores). A 0.8 s clip that the model hallucinated cost 9 CPU-s. In a room of continuous talk this competes with the language model, so hands-free is opt-in per device and meant for the dock.
- **Recognition.** The STT writes "SAIL" as **"sale"** in most addressed phrasings, so the homophone is accepted with tighter placement (round 26 R14). A bare "SAIL." on its own can come back as noise; say the name and the request together.
- **Language.** Portuguese requests do not transcribe with the current STT model. The Portuguese wake names are kept for when a multilingual model is configured.

## 4. Privacy and control

- Presence uses metadata only: visibility, seconds since the last input event, foreground, charging, and the time of addressed turns. There is no keystroke content, camera, location or audio level. Heartbeats live in memory. Only `PresenceSettings` is written (`$SAIL_DATA/pulse/presence.json`).
- Android listens only while the Dock screen is in the foreground, with a visible mic indicator and a **Stop listening** control. The web stops the mic when the tab is hidden.
- Every announcement has **Why?** (the assessment's reasons) and feedback: Useful, Not now, Don't tell me about this kind.

## 5. Interfaces

| | |
|---|---|
| `POST /api/presence/heartbeat` | `Heartbeat` → `{"state": PresenceState}` (owner token; 4 KiB) |
| `GET /api/presence` | `PresenceState` |
| `GET`/`POST /api/presence/settings` | `PresenceSettings` (echoed after clamping: away 60–7200 s, greeting 300–86 400 s, spoken 0–6/h) |
| ws client → server | `surface` |
| ws server → client | `announce`, `presence_greeting` (`items: [{id, title}]`), `presence_state`, `hands_free` (`off`/`listening`/`follow_up`), `not_addressed` |

## 6. Known limits and next steps

- `initiative.js` still polls `/api/initiative/deliverable`, which consumes items. When a desk tab is visible it can take an item before the presence loop routes it, and the item then appears as an initiative card without speech. Routing should become the only consumer.
- The greeting is English. `greeting::compose` supports `pt_pt`, but there is no voice-language setting to drive it yet.
- Out of scope this round: routines (morning / heading out / goodnight), undo instead of confirm, a shared "current focus" for "this", on-device keyword spotting, background phone notifications, speaker verification, owner-editable wake names, host keyboard/mouse idle.
