# SAIL Calendar: one place for what's actually scheduled

**Status:** first slice implemented (round 28; issues #335 phase A, #452) · **Owners:** Life
(the `sail-calendar` crate: store, ICS, recurrence, feeds), Dialogue (routes, voice tools,
briefing and greeting), Discovery (prepare and change signals) · **Related:**
[Life](SAIL_LIFE.md), [Proactive](SAIL_PROACTIVE.md), [Presence](SAIL_PRESENCE.md),
[Android](SAIL_ANDROID.md)

SAIL keeps its own calendar and can subscribe to yours. You paste a calendar's ICS address,
and SAIL shows those events next to its own in Month, Week, Day and Agenda views, on the web
and on the phone. The greeting mentions what starts soon ("Algorithms lecture starts in 40
minutes"). Voice can read a day or add an event. Events you mark **Needs preparation** go
on the agenda 48 hours ahead. When a subscribed event moves or is cancelled, SAIL tells you.
None of this calls a model.

## 1. Connecting a calendar

Calendar → **Sources** → **Add a calendar feed**: give a name and paste the address.

| Calendar | Where to find the address |
|---|---|
| Google | Settings → the calendar → *Integrate calendar* → **Secret address in iCal format** |
| Outlook / Microsoft 365 | Settings → Calendar → Shared calendars → **Publish a calendar** → ICS link |
| Moodle | Calendar → **Export calendar** → *Get calendar URL* |
| Anything else | Any `https://` or `webcal://` address that serves an `.ics` file |

SAIL fetches it once while you wait. If that works, the source shows its event count and
"synced just now". Feeds refresh every 30 minutes by default (15–1440, per source), one at a
time, and never during a voice turn. **Sync now** works at most once a minute.

Feeds are **read-only**. You can tick *Needs preparation* on a feed event, and that's all.
Editing in your real calendar happens in round 29 (OAuth and CalDAV).

`.ics` files can be imported into **SAIL calendar**. The preview shows new events,
duplicates (same UID and recurrence id) and skipped entries, and nothing is written until
you confirm. **Export** gives an `.ics` file of any sources and range.

## 2. Privacy and safety

- **A feed address is a password.** Google's secret address reads your whole calendar.
  SAIL stores addresses only in `<data>/calendar/feeds.json` (mode 0600). The API, logs,
  errors and the model see only `https://host/…`.
- **Fetching is fenced:**
  - `https://` only (`webcal://` is rewritten to it); at most 3 redirects, each checked;
  - 5 MiB, 20 s, and the body must be a calendar;
  - loopback, private, link-local, CGNAT, unique-local, multicast and metadata addresses are
    refused. The connection itself resolves through the same guard, so a host that changes
    its DNS answer after the check still can't reach the LAN;
  - no proxy.
- **Event text is data.** Titles, notes and locations from feeds reach the model only as
  tool output marked as calendar text, and clients render them as text, never HTML. Links
  in notes are never opened.

## 3. What it feeds

| Where | What |
|---|---|
| Arrival greeting / return briefing | Timed events starting in the next 12 h, as "… starts in 40 minutes" (`briefing.rs`, `greeting.rs`) |
| Voice | `calendar:agenda` (today / tomorrow / a date, ≤ 8 lines), `calendar:add` (SAIL calendar only; no end = 1 h) |
| Agenda + Discovery (#452) | A prepared event within 48 h → a "Prepare: …" commitment whose title words are match terms, so related Moodle news revisits it. A feed event moved or cancelled within the next 14 days → "Moved: Physics lab → Thu 14:00" / "Cancelled: …" |
| Web | Calendar page (nav ▸ Calendar): Month / Week / Day / Agenda, quick add, detail, sources, import |
| Android | Calendar screen (month strip + agenda, day view, add/edit sheet) and a **Next up** card on Home |
| Socket | `{"type":"calendar_changed","revision":N}` after any change the clients can see |

"Moved" also covers the usual way Google moves one lecture of a weekly series: the feed
gains an override (`RECURRENCE-ID`) for that instance. "Cancelled" also covers an event
that simply disappears from the feed, which is what Google and Outlook do when one is deleted.

## 4. Data model

Code: `crates/sail-calendar` (contracts in `src/contracts.rs`; the HTTP contract is in
`crates/sail-dialogue/src/calendar_http.rs`).

- Timed events are UTC instants plus their IANA zone. All-day events are dates with an
  **exclusive** end (RFC 5545). Recurrences expand only inside the requested window, so a
  weekly 09:00 Lisbon event stays at 09:00 across DST.
- Local events have ids `loc_…`. Feed events are `fe_` + a hash of source, UID and
  recurrence id, so a refresh keeps the same ids, and *Needs preparation* (stored apart)
  survives refreshes and moves.
- A refresh compares events by content. Google rewrites `DTSTAMP` on every export, and a
  refresh where nothing else changed writes nothing and doesn't make clients refetch. Real
  changes go in one SQLite transaction.
- Limits: 20 sources, 20 000 events per feed, 400-day windows, 5 000 occurrences per
  response, 5 000 instances per series.

## 5. Known limits (round 29 and later)

- No Google/Microsoft sign-in, no CalDAV, no writes to external calendars.
- Recurring local events can be deleted one occurrence at a time, but edits apply to the
  whole series.
- `RDATE` (extra one-off dates on a series) isn't kept. It's rare in Google and Outlook exports.
- A vanished *recurring* series is not announced as cancelled (there is no single instance
  to name).
- The web add-event form reads times in the browser's zone. That matches the default
  (Europe/Lisbon) on the owner's devices.
