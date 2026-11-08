# SAIL Android app

**Status:** in progress (native client, first round) · **Code:** [`android/`](../android) ·
**Architecture issue:** [#38](https://github.com/PedroVictorCoding/SAIL/issues/38)

The Android app is a thin native client of a SAIL server. It's written in Kotlin with
Jetpack Compose and calls the same HTTP and WebSocket APIs the web interface uses. It has
no inference, memory store, grading or scheduling of its own: the server is authoritative,
and the app shows what the APIs return, including honest empty and unavailable states.

This is a bridge until the shared Rust/Dioxus client in #38 exists. It keeps the same
thin-client rules so that move stays possible.

## Screens

Bottom navigation: **Home**, **Dialogue**, **Arena**, **Workspace**, **Library**. Tracker
opens from Home, and Settings from Home's top bar. The first run opens Settings.

| Screen | Server APIs |
|---|---|
| Home | `/api/briefing/*`, `/api/system/metrics`, `/api/health`, `/api/mind/status`, `/api/learn/next`, `/api/spotify/status` |
| Dialogue | `/ws/voice`, `/api/guardian/*`, `/api/speech/transcribe` |
| Arena | `/api/learn/*` (courses, lessons, diagnostics, reviews, evidence) |
| Workspace | `/api/health`, `/api/mind/*`, `/api/system/metrics`, `/api/plans` |
| Library | `/api/reader/library`, `/api/reader/books/{id}`, `/api/music/library` |
| Reader | `/api/reader/books/{id}`, `…/sections/{n}`, `…/position` (GET, PUT), `/api/reader/speech` (GET, POST) |
| Tracker | `/api/tracker/*` |

### Reader

Library › a book › **Start/Continue reading** (or a section row, or the Continue reading
card) opens the e-reader full-screen, with no tab bar. It shows one section at a time from
the server's plain-text blocks (headings, paragraphs, quotes, lists, preformatted text and
breaks; never book HTML). Tap the right or left third of the page to turn a page, which
continues into the next or previous section at the edges. Tap the middle to show the
controls: back, contents, text settings, previous/next section and progress by text.

- **Position:** the anchor (section, block, UTF-16 offset of the first line at least half
  visible) and the percent (code points before the anchor over the book's total, the
  same measure as the web reader) are saved 650 ms after the page settles, and right away
  when the app goes to the background or the reader closes. Saves send the last
  revision seen. If another device saved since then, SAIL answers 409, and the reader
  stops saving and asks whether to go to the other device's place or keep this one. It
  never overwrites the other place silently.
- **Text settings** are per phone (not sent to SAIL): Night, Paper or Sepia, 14–28 pt,
  serif or sans, and keep the screen on (default on). Changing the size or typeface keeps
  the same place at the top of the page.
- **Read-aloud** (♪ in the top bar) uses SAIL's host voice, the same Kokoro worker and
  route as the web reader. The text is spoken literally, one sentence per request, and
  never reaches a model. Sentences are cut the way the web reader cuts them. Up to two are
  fetched ahead, and a 429 (the voice is reading another sentence) is retried after
  300 ms. The sentence being read is highlighted and kept in view. Scrolling away stops
  following until *Follow* is tapped. Reading continues into the next section. While
  listening, the spoken sentence is the saved position. The bar has previous sentence,
  play/pause, next sentence and stop. Voice and speed (0.7–1.5×) are in **Aa**. If another
  app or a call takes the audio focus, narration pauses. If the host voice is down, the
  reader says so; there is no on-phone fallback voice.
- **Reading from a chosen sentence** is deliberate, so a stray touch can't move the voice:
  - **Long-press** a sentence to mark it. Taps still only turn pages or toggle controls.
  - A long press during a scroll, or within 450 ms after one, is ignored.
  - The mark only underlines the sentence. Nothing moves until **Read from here** (or
    **Listen from here**) is tapped on the card. While a mark is up, any other tap or Back
    only clears it. The mark clears itself after 8 s.
  - After a jump, **Undo** (8 s) returns to the sentence the voice was on, for re-reading
    or for undoing a wrong skip.
- Not in this build: background playback with a media notification (narration keeps
  going with the screen on, which is the reader's default), importing books from the
  phone, and offline reading. Each section loads from SAIL when it's opened.

## Connection and security

- On first run the owner enters the server address (default `https://sail.spinethorns.com`,
  reachable only on the owner's tailnet, #334) and the owner token. The token can be pasted
  or fetched once from `/api/guardian/token`. The app checks the address and the token
  before saving.
- The token is stored with Android Keystore-backed encryption (AndroidX Security), excluded
  from backups and device transfer, and sent as `Authorization: Bearer …` **only to the
  configured SAIL host**. Images from other hosts never get it.
- Release builds allow HTTPS with the system's certificate authorities only. Debug builds
  also allow plain HTTP and trust CAs the owner installs on the phone, so a development SAIL
  with its self-signed `SAIL-local` certificate works once that certificate is installed
  (Settings › Security › Install from storage › CA certificate).
- Over USB, `adb reverse tcp:8088 tcp:8088` lets the phone use `https://localhost:8088`
  (the `SAIL-local` certificate covers `localhost`) without exposing SAIL on the network.
- **Behind Cloudflare Access:** if `sail.spinethorns.com` is published through a Cloudflare
  Tunnel, protect it with an Access application (owner-only policy) and add a *Service Auth*
  policy with a service token. Enter its client ID and secret in Settings; the app sends
  `CF-Access-Client-Id`/`-Secret` to the SAIL host only. Through a tunnel SAIL never hands
  out the owner token (`/api/guardian/token` is refused, PR #410), so paste the token.
- Microphone and camera permissions are asked for at the moment they're used.

## Building

Requirements: JDK 17 and the Android SDK (platform 35, build-tools 35). Gradle 8.11 is
pinned in `android/gradle/wrapper/gradle-wrapper.properties`; the wrapper JAR isn't
committed, so generate it once with a local Gradle:

```sh
cd android
gradle wrapper --gradle-version 8.11.1   # once
./gradlew testDebugUnitTest               # JVM unit tests
./gradlew assembleDebug                   # app/build/outputs/apk/debug/app-debug.apk
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Point `ANDROID_HOME` (or `android/local.properties` → `sdk.dir=`) at the SDK.

## Code layout

```text
android/app/src/main/java/dev/sail/app/
  core/net/       SailApi (HTTP + WebSocket), ApiResult, ServerSettings, Connection
  core/design/    theme, glow material, shared components, icons, safe Markdown
  core/           AppContainer, sailViewModel, UiState
  ui/             SailApp (navigation, bottom bar), Routes
  feature/<name>/ Models (DTOs) · Repository · ViewModel · Screen (reader/ is the e-reader)
```

Icons are generated from `assets/source/icons/sail-icons.svg` by
`android/tools/convert_icons.py`, so web and app share one icon set. Colours, hues and the
glow material follow `DESIGN.md` and `crates/sail-dialogue/web/tokens.css`.
