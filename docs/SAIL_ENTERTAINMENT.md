# SAIL ENTERTAINMENT — Product and Technical Specification

**Status:** Proposed / evolving — verify implementation against code · **Domain:** Personal & Social · **Roadmap issue:** [#17](https://github.com/PedroVictorCoding/SAIL/issues/17) · **Priority:** P3 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Unify local-first music, video, media discovery and reading continuity.

## 2. Functional scope
- Player adapters.
- media search and playlists.
- reading state #73.
- Audio DNA #72.
- release tracker #74.
- authorized playback..

## 3. Ownership and non-goals
Not a DRM bypass or unauthorized download engine; Bridge handles service adapters and Guardian scopes connected accounts.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Bridge, Dialogue, Arena, Discovery, Guardian, Flow.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Existing Entertainment Space with listening/reading controls and media resume.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 7. MVP vertical slice
Pause/resume one approved player, remember a book location and restore it on another client.

### Acceptance checklist
- [ ] Confirm existing source files, interfaces and dependency versions before changing architecture.
- [ ] Implement the minimal real end-to-end flow above, without mocks presented as production behavior.
- [ ] Test success, offline/unavailable, permission denial, cancellation and restart/reconnection as relevant.
- [ ] Record measured latency and resource use where relevant; avoid fabricated benchmarks or status.
- [ ] Show user-visible success/error through the mapped existing UI and preserve user control.
- [ ] Submit focused tests and a reviewable PR without automatic merge.

## 7a. Implemented: Spotify playback by conversation (2026-11-05)

SAIL plays the owner's own Spotify music as soon as it is asked for, and says what is playing.

> **User:** Play my radiohead playlist.
> **SAIL:** Playing the playlist 'Cabeça de Radio' by Pedro Victor on Pedro's phone.

- **Adapter:** `crates/sail-bridge/src/spotify.rs` calls the Spotify Web API directly. It does search, the owner's playlists (created or followed, up to 200), play, pause/resume/next/previous and now-playing. Sign-in is OAuth authorization code with PKCE, so no client secret is stored. `config.json` and `token.json` live in `<data root>/spotify/` with mode 0600. The scopes are read playback and playlists and control playback; nothing edits the library. `spotify-tui` was evaluated and not used: it hard-codes a `http://localhost` redirect, which new Spotify apps no longer accept, and its last release was in 2021.
- **Tools:** `spotify:find` returns the full name and creator of the best match and up to four alternatives, or the owner's whole playlist list when no name matches, so the model can map "radiohead" to "Cabeça de Radio". `spotify:play` plays a URI. `spotify:control` handles pause/resume/next/previous/status.
- **No confirmation step.** Earlier versions asked "Should we proceed by playing …?" and waited for a yes. That cost a second model round of minutes on this host, so it was removed (2026-11-05):
  - fixed-shape requests ("play my X playlist", "pause", "next song") are answered in code by `crates/sail-dialogue/src/music_fastpath.rs`, which plays the best match at once;
  - in a model turn, a successful `spotify:find` with a best match plays it in the same round (`reason_turn` in `server.rs`), and the model is told only to say what is playing;
  - when no playlist name matches, the model picks one from the list and calls `spotify:play` directly.
  The owner can stop or skip with "pause"/"next" or the Now Playing card.
- **Interface:** a Now Playing card in the Tools Map sidebar has three states: Set up, Connect, and Now Playing/Paused with previous/play-pause/next. Its HTTP routes are under `/api/spotify` (`status` is readable; `setup`, `login`, `control` and `disconnect` need the owner token; `callback` is protected by the single-use PKCE state).
- **Setup:** Set up Spotify → create an app at developer.spotify.com → add the Redirect URI shown (`https://<SAIL address>/api/spotify/callback`) → paste the Client ID → Save and connect. `SAIL_SPOTIFY_CLIENT_ID` and `SAIL_SPOTIFY_REDIRECT_URI` override the saved values.
- **Limits:** playback control needs Spotify Premium and an available Spotify Connect device (phone, desktop app or speaker). SAIL uses the active device, otherwise one whose name contains "SAIL", otherwise the first available. The SAIL host has no sound card, so it cannot be a player itself yet.

## 7a-2. Implemented: one music player for uploads and Spotify (2026-11-06)

`/music` (Entertainment → **Music player**) plays the owner's own uploaded music and Spotify from one
list, one queue and one player bar, all in the browser.

- **Uploads** (`crates/sail-bridge/src/music.rs`, `crates/sail-dialogue/src/music_http.rs`): DRM-free MP3,
  M4A/AAC (ALAC accepted; Chromium may not play it), FLAC, Ogg and WAV up to 300 MB, from purchases such as
  Bandcamp, iTunes Plus, Qobuz or 7digital. The container is recognised from its bytes. FairPlay `.m4p`
  (`M4P` brand) and MP4s with `drms`/`drmi`/`enca` sample entries are refused. Tags, track/disc numbers, year
  and duration come from `lofty` (MIT/Apache); embedded artwork is decoded with 4096px/64MiB limits and
  re-encoded as a JPEG of at most 600px. Storage is `<data root>/music/library.json` and
  `tracks/<id>/{audio.<ext>,cover.jpg}`, atomic and mode 0600; the id is the file's SHA-256, so a
  re-import finds the existing track. Removing a track deletes SAIL's copy only.
- **API** under `/api/music` (owner token, `no-store`): `GET /library`, `POST /import` (raw bytes,
  percent-encoded `X-Filename`), `DELETE /tracks/:id`, `GET /tracks/:id/cover`, `POST /tracks/:id/stream`.
  An `<audio>` element cannot send the owner token, so `POST …/stream` returns a ticket URL
  (`GET /stream/:ticket`): random, bound to one track, valid 12 hours, at most 256 live, revoked when the
  track is removed. Streams support single HTTP Ranges, so seeking works.
- **Spotify in the browser:** the player loads Spotify's Web Playback SDK and registers this page as the
  Spotify Connect device **"SAIL Player"**. SAIL asks Spotify to play exactly one track URI on that device
  (`POST /api/spotify/player/play`), so SAIL's queue, not Spotify's, decides what comes next; a finished
  track is detected from the SDK state and the queue advances. Other routes: `POST /player/token` (an access
  token for the SDK, owner only, never cached), `GET /player/search?q=`, `GET /player/tracks?uri=` (album or
  playlist, first 100), `GET /player/playlists`. The connection now asks for `streaming user-read-email
  user-read-private`; connections made earlier must be reconnected once, and the player offers
  **Reconnect Spotify**. Requires Premium and a browser with Widevine (EME); otherwise the player says so.
  Voice requests ("play my X playlist") still use Spotify Connect, and will pick "SAIL Player" when no other
  device is active.
- **Spotify's February 2026 Development Mode rules** (in force for SAIL's app since 2026-03-09):
  playlist songs come only from `GET /playlists/{id}/items` (the old `/tracks` returns 403), rows hold the
  track in `item`, and only for playlists the owner created or collaborates on. Followed playlists return
  403; the player says so and offers **Play this playlist**, which plays it as a whole on SAIL Player
  (`context_uri`; Spotify decides the order, and next/previous move within it). `/search` accepts at most
  `limit=10` (`SEARCH_LIMIT`). The voice playlist index reads `/items` too (index version 2; older
  indexes are rebuilt), so followed playlists are matched by name only. Spotify answers 503
  intermittently: reads retry once after 0.7 s; changes are never repeated.
- **One engine at a time:** starting an item pauses the other engine. Pause, seek and volume go to whichever
  is current; volume applies to both. Media Session metadata and actions are set for OS media keys.
- **Cross-origin isolation:** SAIL's pages are `COEP: require-corp` (on-device speech needs
  SharedArrayBuffer), which would block Spotify's script and iframe. Only the `/music` document is served
  without isolation, and SAIL embeds it as a `credentialless` iframe (set before `src`), so the app around
  it stays isolated. Browsers without credentialless iframes get an **Open the music player** link instead.
  This is the one documented exception to "no third-party scripts at runtime": `sdk.scdn.co`, loaded only
  by `/music` and only once Spotify is connected with the streaming scope. Spotify album art is shown from
  Spotify's image host, as in the Now Playing card.
- **Interface:** sources (All music, Uploads, Spotify), the owner's Spotify playlists, upload (button or drop),
  search across uploads and Spotify, per-source badges, Play/Shuffle, add to queue, remove upload, an Up next
  panel and a player bar. The ambient backdrop is lit by the current track's cover.
- **Verified:** Rust tests for import (WAV, hand-built MP3 with ID3 + cover, FLAC), DRM refusal, de-duplication,
  ranges, tickets and the Spotify player routes against a fake Web API; a browser test with fixture APIs and a
  fake SDK for the mixed queue, uploads, Spotify states, phone layout and the credentialless embed in an
  isolated page (`scripts/ui/music_player_browser_test.py`).
- **Not verified here:** real Spotify playback (needs Widevine and Premium, which the test browser lacks),
  real Ogg/M4A files, and Android WebView. The sidebar Now Playing card shows Spotify (including SAIL Player)
  but not uploads yet.

## 7b. Implemented: SAIL Reader, first slice (2026-11-06, issue #333)

A local book library at `/reader` with a paginated reader and on-device read-aloud whose pages turn
when the narration reaches them. **Reading and read-aloud never call Mind, Agents or any language model.**

- **Formats:** DRM-free EPUB 2/3 and UTF-8 `.txt`, up to 50 MB. DRM or encrypted EPUBs (`rights.xml`, or
  `encryption.xml` with anything other than font obfuscation) are refused with a plain message. PDF is later.
- **Import safety** (`crates/sail-bridge/src/reader/zip.rs`): a strict ZIP reader on `flate2`. It refuses ZIP64,
  encryption, absolute or `..` paths, backslashes, duplicates, more than 4,000 entries, entries over 32 MB,
  more than 256 MB unpacked and ratios over 200×. Every CRC is checked, and inflation stops at the declared size.
- **Canonical text** (`reader/epub.rs`, `reader/mod.rs`): each spine document is flattened once into plain-text
  blocks (`h1`–`h6`, `p`, `quote`, `li`, `pre`, `hr`; `\n` marks a line break). No book HTML reaches the browser,
  and the client renders with `textContent`. Scripts, styles, media, forms, hidden elements and footnote bodies or
  markers are dropped, so footnotes don't interrupt narration. Drop-cap images keep their letter. The TOC comes
  from the EPUB 3 nav document, else the NCX, else the headings. Entries point to `(section, block)`, because
  chapters often share one file. Text files split at "Chapter …/Part …" lines, else every 150 paragraphs.
- **Storage:** `<data root>/reader/library.json` (metadata and positions) and `books/<id>/{content.json,source.*}`,
  all written atomically with mode 0600. The id comes from the SHA-256 of the file, so a re-import finds the
  existing book. Removing a book deletes SAIL's copy only; the owner's original file is never touched.
- **Positions** are text anchors `{section, block, char}` (char = UTF-16 offset), never page numbers. Saves use
  revisions (`expected_revision`); a stale save gets 409 with the current position, and the reader offers
  "Resume there / Keep here". The same anchor reopens correctly on any layout (spread or phone, any font size).
- **API** (`crates/sail-dialogue/src/reader_http.rs`, owner token on every route, `no-store`), under `/api/reader`:
  `GET /library`, `POST /import` (raw bytes plus `X-Filename`), `GET|DELETE /books/:id`,
  `GET /books/:id/sections/:index`, `GET|PUT /books/:id/position`, `GET|POST /speech`.
- **Web reader** (`web/reader.{html,js,css}`): library with Continue Reading, typographic covers, search, import
  (button or drag-and-drop) and removal. The reader uses CSS columns, two-page spreads when wide and one column
  on phones. Pages turn by edge tap, arrow keys or swipe. It also has a TOC drawer and font size, spacing, width
  and theme settings.
- **Read-aloud** (`web/reader-audio.js`, `client-speech.js` `loadTts()`, worker role `tts`): the existing Kokoro
  (q8, WebAssembly, served from `/voice-assets`) is loaded **without** VAD or Whisper. A worker Dialogue already
  loaded is reused, so one device never runs two copies. Text is split into sentences, and also at page
  boundaries, with 1–3 segments synthesized ahead and a Web Audio clock. A page turns when a segment *starts
  playing*, never when it is synthesized, and never while paused. Paging by hand suspends following until
  **Follow narration** is pressed. Seeking, skipping or changing the voice bumps a generation counter, so late
  audio is dropped. Narration continues across sections and ends with "Finished". The current sentence is
  highlighted; no word-level timing is claimed.
- **Host voice (default when available, 2026-11-06):** `POST /api/reader/speech` with `{text, voice, speed}` returns
  16-bit PCM from the host's Kokoro worker, the one Dialogue uses (native ONNX, 4 threads). It speaks 1–600
  characters literally, with one of the 54 host voices, at 0.7–1.5×. Measured here, the host needs 0.31 s per
  second of audio, against about 3 s for Kokoro in the browser: narration went from 5–25 s pauses at most
  sentences to none in 70 s of playback.
  - It calls `VoiceModelServices::synthesize_reading`, which never opens a voice turn, so narration doesn't
    cancel background inference or builds. Book text is not logged.
  - Only one reader request is served at a time (a second gets 429 and the reader retries), so conversation
    speech waits for at most one sentence.
  - The controller has "Voice runs on: SAIL host / This device" and a speed menu (0.8–1.5×; host only, since the
    browser's speed isn't verified). If this device is slower than real time for two sentences in a row, the
    reader switches to the host and says so. If the host fails, it falls back to this device.
- **Verified live (2026-11-06)** with three Gutenberg EPUBs and one Gutenberg TXT, the real API and real Kokoro in
  headless Chromium:
  - pages turned automatically exactly at the next spread's first sentence;
  - the position persisted across a reload, a font change and a phone layout;
  - there were no off-origin requests and no non-reader API calls.
- **Limits:** with the host voice, reading aloud needs a connection to SAIL; offline it uses this device's voice,
  which pauses between sentences on slow devices (and the reader says so). Background or lock-screen playback and Android WebView are untested. Bookmarks, highlights, notes,
  collections, cover images, in-book search and Relay handoff are the next phases of #333.

### Library composition and online book details

The Library follows the ambient-glass guide (#339): a compact book navigator, a
cover-led selected book, measured progress, chapter/collection counts, shelf and
saved reading activity. Sidebar selection updates the featured book; Continue
Reading opens it. Below 980px the activity moves beneath the shelf, and below
700px the regions stack. `--sail-*` tokens supply the shared material and focus.
Reading surfaces remain opaque, and the reader's theme and narration are independent.
The Library uses static gradients, without blur, continuous animation or new JS dependencies.

**Find cover & details** opens a searchable match picker. Search explicitly sends
only the entered title, optional author and optional ISBN to Open Library. It does not
send book text, source paths, owner tokens or reading history. ISBN-10/13 values are
compacted and checksum-validated before leaving the host. Open Library remains the
only catalog provider; SAIL does not scrape retailer or social-reading pages and adds
no third-party API key. Opening the Library, importing, reading and searching the
local shelf never initiate a catalog search. The owner previews a returned match,
edits a local draft and explicitly saves it; imported metadata, text and position
anchors stay intact.
Selected online title/author appear in the Library, while original fields remain
available for lookup and removal restores them. **About this book** progressively
discloses the bounded description, source link and **Remove online details** action.
The work-level first-publication year and cover may represent a different edition;
no exact edition or page-count match is claimed. Reading time, streaks, badges and
social rankings are not fabricated.

Bridge owns the adapter (`reader/catalog.rs`), using the documented
[Search](https://openlibrary.org/dev/docs/api/search) and
[Covers](https://openlibrary.org/dev/docs/api/covers) APIs plus the selected work's
JSON description. Search first asks the exact title/author fields; when those only
return box sets, study guides or a generic series title, it merges a ranked general
query and prefers the closer owned work. The picker shows subtitle (when present),
authors, first year, a publisher and language, and accepts an ISBN-only lookup. Cover
selection uses `cover_i` first and a validated edition OLID as a fallback.
Production URLs are fixed HTTPS hosts, with a narrow allowlist for Open Library's
verified Internet Archive cover-storage redirect chain. Requests
have 4s connect/12s total timeouts, are serialized with at least 1.1s between starts,
and are bounded to 512KiB JSON / 2MiB cover responses. Images are decoded with
4096px/64MiB limits, resized to at most 600×900 without upscaling, and re-encoded as
JPEG in a blocking worker. The existing image dependency is reused. Details and
artwork are best effort; missing covers retain a typographic fallback.

Confirmed details persist in `<data root>/reader/library.json`, and validated cover
files are private atomic writes in `books/<id>/cover-<uuid>.jpg`. The index is updated
only after the new cover is written; replaced/removed covers are cleaned up. Old
libraries deserialize without metadata. Cached details and covers work offline and
are removed with the book. No third-party image URL, CDN or API key reaches the
browser. Thumbnails use one authenticated same-origin fetch per cover version and
shared session object URLs, loaded only near the viewport and revoked when obsolete.
Open Library attribution links remain available; cached art is for the owner's
private Library, not a public artwork redistribution feature.

Additional owner-authenticated, `no-store` API routes under `/api/reader`:

- `POST /books/:id/metadata/search` — `{title, author, isbn?}`, returns at most five
  matches. Any one of title/author/ISBN may be supplied; ISBN is exact.
- `POST /books/:id/metadata/preview` — `{work_key}`, accepts a recent server-held search
  match, downloads its details/validated cover into a volatile preview, and returns
  `{preview_id, metadata}`. It never writes the Library or its persistent cover.
- `GET /books/:id/metadata/preview/:preview_id/cover` — owner-authenticated preview JPEG.
- `POST /books/:id/metadata` — `{preview_id, edits, use_cover}`. Only **Save details**
  persists the draft and the exact previewed cover, without redownloading. `edits`
  contains title, authors, first_publish_year, subjects and description. A null
  preview_id edits already-saved details and their cached cover entirely offline.
  Provider work/cover IDs cannot be edited or substituted by the client.
- `DELETE /books/:id/metadata` — removes online details and cached cover.
- `GET /books/:id/cover` — serves the cached JPEG; no network fetch on this route.

Search/preview bodies are limited to 4KiB; reviewed edits to 16KiB, invalid input is rejected, and concurrent
catalog operations return 429. Failed services and empty results have explicit UI
feedback; the lookup sheet can be closed or retried without affecting reading.

**Preview and edit workflow:** Search → **Preview & edit** → inspect the actual cover
and editable fields → **Save details**. Before saving, Back, Close and Escape discard the unsaved
client draft. Existing saved details open directly in the same editor; **Find another
match** returns to lookup. The cover checkbox can omit the artwork. Missing or failed
preview images offer explicit text and permit saving details without a cover, rather
than silently selecting unseen artwork. Failed saves preserve the draft for retry.
Locally changed values are marked `user_edited` and attributed as edited locally.

Preview images use authenticated same-origin blob URLs, revoked when leaving a draft.
The server holds at most 16 book previews (one per book, at most 2MiB each); tokens
are valid for ten minutes, bound to their book and pruned on cache use. Expired or
replaced previews return 410 and require previewing again. Search matches remain
bounded to 16 books/five results and expire separately. Old clients that attempt a
direct `{work_key}` save must reload: selection now requires a reviewed preview or
an existing saved metadata record. Original imported files, metadata and reading
positions remain intact. Edits validate title (300 characters), up to 8 authors
(160 each), year −9999…9999, up to 4 subjects (80 each), and description (1600).

Validation commands:

- `cargo test -p sail-bridge reader:: --lib`
- `cargo test -p sail-dialogue reader_http::tests --lib`
- `node crates/sail-dialogue/tests/reader_ui_test.cjs`
- `node crates/sail-dialogue/tests/library_browser_test.cjs` (existing Playwright,
  or `PLAYWRIGHT_MODULE` selecting its module path). Intercepted fixtures cover
  metadata search/confirmation/caching/fallback/removal, search, keyboard, resume,
  import/removal, empty/error states, four widths, reduced motion and forced colors.
- Explicit opt-in real service smoke test:
  `cargo test -p sail-dialogue reader_http::tests::live_open_library_lookup_roundtrip --lib -- --ignored`
  searches *The Time Machine* by H. G. Wells, confirms a match, validates a downloaded
  raster, reopens its local library and checks authenticated cover delivery. Network
  unavailability or catalog changes may fail it; it is excluded from normal CI.

Desktop/mobile browser screenshots have been reviewed, including a real downloaded
cover used only in the local preview. Actual Android/WebView, battery profiling and
assistive-technology device testing remain separate checks.

### Entertainment workspace

The main left sidebar opens **Entertainment** in the same center workspace as Tracker,
with a thin **Reader / Music player / Movies** bar. Reader opens by default; Music player
and Movies identify themselves as later updates. The existing Spotify adapter is unchanged.
The Reader frame stays mounted across tab changes, retaining the open book. Switching
away pauses narration and checkpoints the position; returning requires explicit playback.
The standalone `/reader` link remains available.

## 8. Next steps
1. Audit the relevant implementation and the linked roadmap issue; update this spec with confirmed module/file paths and contracts.
2. Build the MVP vertical slice with real tool/model/provider integrations.
3. Add security and regression tests before expanding capability breadth.
4. Record known limitations, operational instructions and migration compatibility.

## 9. Related references
- [Roadmap and subsystem taxonomy](https://github.com/PedroVictorCoding/SAIL/issues/15)
- [Documentation index](README.md)
- [SAIL Studio](SAIL_STUDIO.md) for persistent editable artifacts (distinct from Canvas)

## 10. External product research reference — Taranify (2026-11-02)

**Product reference:** [Taranify](https://www.taranify.com/) · [How it works](https://www.taranify.com/how-it-works) · [About](https://www.taranify.com/about).

**Source status:** Public-facing product website/documentation, **not a verified open-source code repository or licensed software SDK**. Do not cite the site as available source code or copy its implementation, branding, media assets, proprietary recommendation logic, or visual designs. If code is later published, independently verify repository ownership, version, license, integration surface and permission before adding it as a code dependency. This reference should be used to research *product interaction patterns* and define SAIL's original implementation.

### Publicly observable concept

Taranify offers entertainment discovery informed by a *momentary self-report color-choice quiz* and optional mood-related questions, covering Spotify playlists, movies, Netflix shows, books and food. Its home page also describes a group movie-picking flow: share a short join code, have participants answer the quiz and seek a recommendation acceptable to everyone.

Its own [methodology page](https://www.taranify.com/how-it-works) expressly cautions that entertainment mood research **does not validate its particular color quiz as a psychological assessment**. Do not reproduce Taranify's marketing claims about scientific precision or mood detection as established fact. For SAIL, color choice is a playful *preference elicitation* interaction, **never mental-health assessment, emotion inference certainty or physiological measurement**.

### SAIL Entertainment feature specification: Moment Discovery

- **Entry point:** Entertainment Space → **“Find something for now”**. Compact category selector: Music / Film & series / Books; optional Food recommendations should be delegated to Life rather than silently enlarging Entertainment's scope.
- **Mood direction first:** ask whether the user wants something matching, lifting, calming, energizing or contrasting their current mood. Allow a direct text description, optional playful paired-color preference cards, or **Skip**. No mandatory quiz.
- **Short, revisable signal:** retain an ephemeral `MomentPreference { modality, selected_signals, desired_effect, optional_context, timestamp, consent }`, not a fabricated “detected emotional state”. Show a human-editable summary before using it.
- **Ranking:** combine explicit mood direction, format, availability, duration, language, novelty, previous likes/dislikes (only if permissioned) and diversity; provide concise “why this” explanations. On weak data, degrade to a transparent recommendation based on stated preferences instead of claiming calibrated AI predictions.
- **Interaction:** three to five focused recommendation cards, regenerate, filter, save, dislike, and “not now”; keep source/provider attribution and actionable playback/deep links only where licensed and technically available. Don't promise an accessible Netflix/Spotify catalog or playback without the relevant authorized adapters.
- **Group recommendation (optional later milestone):** local/consented session with explicit participant join, one-time short code, time-limited identity, per-participant preference summaries, weighted fairness/least-misery balancing and a clear explanation of compromise; no inferred psychological dossiers or automatic account linking.
- **Feedback and learning:** store explicit feedback separately from transient mood inputs. Use Discover/Arena only for consented durable preferences; ephemeral mood should expire. Offer anonymous/private mode with no history dependency.
- **Voice:** Dialogue may ask “Do you want something mellow, or something to change your energy?” and present the same results; no microphone-derived emotion recognition without a separate explicit opt-in design.
- **UI design:** retain SAIL minimal dark/glass UI; use simple accessible color-choice cards and brief single-step flow only if beneficial. Color choice must never be the sole channel for a decision (screen readers, color-vision accessibility). Keep mobile and desktop functionally equivalent.
- **Research/evaluation:** A/B-test optional color choice versus direct mood text versus no mood prompt using measured click-through, satisfaction and repeat use; assess group fairness and diversity. Neither color selection nor engagement metrics should be claimed to measure psychological wellbeing.

### Implementation / boundary mapping

- **Entertainment:** recommendation orchestration and actionable media cards; existing Audio DNA #72 and release tracker #74 may become candidate features only after their own contracts are implemented.
- **Discovery:** optional surfacing of new eligible recommendations, respecting attention budget and quiet hours.
- **Bridge + Guardian:** provider access, local metadata import and auditable permissioned Spotify/other integration; no scraping around DRM, login gates or access restrictions.
- **Arena:** consented cross-session tastes and provenance; no mandatory media-history tracking.
- **Mind:** bounded local ranking/explanations; deterministic filter before generative reranking.
- **Friends/Relay:** group invitations and transient shared decision context if user explicitly enables group mode.
- **Life:** food recommendations if ever adopted.

**MVP acceptance (after player MVP):** Without any external account, user selects Music, optionally states desired mood, receives grounded recommendations from an available local/public catalog, sees provenance and can regenerate; user can skip/color-mode toggle and clear all transient answers. Separate integration tests for provider unavailable, user opt-out, missing catalog, inaccessible colors, and mobile navigation. Do not claim that Taranify's private source or model has been incorporated.
