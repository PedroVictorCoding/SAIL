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
