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
| Library | `/api/reader/*`, `/api/music/library` |
| Tracker | `/api/tracker/*` |

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
  feature/<name>/ Models (DTOs) · Repository · ViewModel · Screen
```

Icons are generated from `assets/source/icons/sail-icons.svg` by
`android/tools/convert_icons.py`, so web and app share one icon set. Colours, hues and the
glow material follow `DESIGN.md` and `crates/sail-dialogue/web/tokens.css`.
