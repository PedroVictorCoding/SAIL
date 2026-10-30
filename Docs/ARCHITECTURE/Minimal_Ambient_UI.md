# Minimal Ambient UI

The Dialogue server serves the SAIL Home/Dialogue shell from `crates/sail-dialogue/web` and the first-party icon sprite from `assets/source/icons/sail-icons.svg`. Its CSS tokens, floating controls, responsive Spaces sheet and locally drawn Orb form the shared v1 visual language. The idle view stays sparse; subsystem navigation expands only after Spaces is opened.

The existing voice WebSocket remains the source of truth for session state. Microphone, mute, interrupt, tier, voice, text and image analysis actions call the existing session controls. The Orb listens to the server's telemetry and playback stream. Camera/image analysis is only started by a user choosing a local PNG/JPEG and sending a request. A disconnected local server is shown as disconnected. Subsystems without a runtime route are marked “Not connected”. Tool outputs show a brief summary with raw payload inside an explicit details disclosure.

## Assets

All UI artwork is rendered locally. The background and Orb use CSS/Canvas2D, the SVG icon set is original, and no concept-art pixels, remote image host or hosted renderer is used. The asset manifest pins each logical asset to a source and budget. From the repository root:

- `cargo xtask assets build` generates `assets/generated/asset-index.json` with source hashes.
- `cargo xtask assets verify` checks IDs, metadata, paths, budgets and hash drift against that index.
- `cargo xtask assets preview` prints the deterministic asset index without writing files.
- `cargo xtask assets clean` removes generated copies and the generated index; `build` recreates them.

The Orb uses a Canvas2D fallback and honors reduced-motion preferences. `window.sailUi.setOrbTelemetry({state, rms, phase, spectrum})` supplies fixed local telemetry for screenshot tooling and freezes the animation until `resumeOrbAnimation()`; it never changes the displayed connection state. The responsive layout supports 320px widths, keyboard navigation, Escape-to-close, focus return, screen-reader labels, and safe-area insets. The small telemetry inspector remains collapsed unless a developer opens it. Model routing preferences and Guardian approval sheets still require their backend UI contracts; spaces without runtime routes are clearly marked unavailable.
