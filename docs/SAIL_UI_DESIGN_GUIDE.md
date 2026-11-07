# SAIL UI Design Guide — Ambient Glass / Living Light (2026-11)

**Authoritative design requirements:** [DESIGN.md](../DESIGN.md). This guide replaces the previous dark tactile neumorphism aesthetic. Maintains the project identity: restrained, purposeful, legible and low-clutter.

## Visual references and exact interpretation
See [Ambient Glass References](design/AMBIENT_GLASS_REFERENCES.md) for the eight owner-supplied inspiration images, their ordered filenames, intended use and implementation constraints. The images are owner-supplied inspiration only; code and branded assets must not be traced/copied from them.

### New shared primitives
- `AmbientBackdrop`: a near-black canvas with one (sometimes two) radial chromatic light layers, masked to the currently active region; never underneath dense reading text without an opaque scrim.
- `GlassSurface`: semitransparent with thin bright edge and optional measured low-radius backdrop blur; opaque gradient fallback.
- `LuminousCard`: dark card with subtle soft color near an edge and a real status/capability, no dummy values; used sparingly for actionable data.
- `OrbReactive`: slow amplitude-sensitive breathing from observed speech/capture activity, accessible textual/status mirror and no active animation in reduced-motion.
- `StatusCapsule`: rounded signal strip, real event label and one state-specific accent; e.g. READY, LISTENING, SPEAKING, RECORDING, WORKING, OFFLINE.
- `TimelineSurface`: quiet rule, actual scheduled events and state tint for Life/Pulse, not a bright gradient chart for its own sake.
- `ProgressMeter`: constrained luminous fill only for verified numeric progress, supports unknown/indeterminate without fake percent.
- `GlassAction`: legible solid/transparent hybrid CTA with focus ring and tactile press, not invisible low-contrast ghost button.

## Typography and composition
- Main information uses restrained sans-serif scale with deliberate contrast and breathing room; oversized headings only for destination identity, not every widget.
- Main reading/learning/editor surfaces flat and opaque enough for extended sessions.
- Primary information should be visible without hunting among decorative cards; use progressive disclosure rather than giant card grids.
- The base backdrop is `#0a0c10`, text `#f2f5fa`, subtle surfaces `#141820`; color semantic mapping comes from DESIGN.md. References are colorful but should not make every screen a full rainbow.
- No constant animated glows across the entire dashboard; one primary active luminous surface, smaller dim secondary signals.

## CSS implementation pattern (illustrative, not a drop-in full theme)
```css
.sail-glass {
  position: relative;
  isolation: isolate;
  background: linear-gradient(145deg, rgb(31 41 57 / .82), rgb(11 16 25 / .90));
  border: 1px solid rgb(222 238 255 / .13);
  border-radius: var(--sail-radius-card, 24px);
  box-shadow: 0 16px 48px rgb(0 0 0 / .22), inset 0 1px rgb(255 255 255 / .09);
}
@supports (backdrop-filter: blur(12px)) {
  .sail-glass[data-blur="allowed"] { backdrop-filter: blur(var(--sail-blur-supported, 12px)); }
}
.sail-light {
  pointer-events: none; position: absolute; inset: -18%;
  background: radial-gradient(ellipse at 70% 80%, var(--sail-light-a) 0, transparent 60%);
  opacity: var(--sail-illumination, .15);
  filter: blur(28px);
  transform: translate3d(0, 0, 0);
  transition: opacity var(--sail-transition-state, 420ms) ease,
              transform var(--sail-transition-state, 420ms) ease;
}
@media (prefers-reduced-motion: reduce) {
  .sail-light, .sail-glass { animation: none !important; transition-duration: 0ms !important; }
}
@media (forced-colors: active) {
  .sail-glass { background: Canvas; border: 1px solid CanvasText; }
  .sail-light { display: none; }
}
```
Prefer precomputed gradients with opacity cross-fading instead of animating several filter layers. `backdrop-filter` is optional. The reference aesthetic is an accent, not an excuse for slow pages.

## State integration
`crates/sail-dialogue/web/ambient-state.js` is the only writer of `data-sail-state` (on `<html>`) and of `--sail-light-a/b/c`. It reduces real events (socket, voice phases with turn epoch, mic capture and denial, consented listening context, Guardian approvals, foreground tool receipts, Arena Learn session) into one of the canonical states in [DESIGN.md §3](../DESIGN.md#3-state-driven-chromatic-language): `idle`, `listening`, `thinking`, `speaking`, `executing`, `learning`, `recording`, `approval_required`, `completed`, `warning`, `error`, `disconnected`. There are no aliases. Views read `data-sail-state` / `--sail-light-*` or call `SailAmbient.subscribe(view => …)`; they never set colours themselves. It also publishes `data-sail-recording`, `data-sail-approval` and `data-sail-connected`, which stay visible whatever the shell colour is. Tests: `node --test scripts/ui/*.test.js`.
- Inbound mic frame/capture means listening; pending mic request means awaiting permission, not listening.
- Audio PCM playback clock means speaking; synthesis buffered without playback means buffering/processing, not speaking.
- Real `Mind` inference/task state means thinking; no arbitrary spinner when idle.
- Recording stays marked until microphone/recording subsystem confirms stopped.
- Prioritize foreground voice state over background work; other statuses persist in Activity.

## Library selection / maintenance
**Phase 1: CSS/Dioxus alone.** Phase 2 (only after measured need): assess `dioxus-motion` for Rust springs or `motion` vanilla JS for DOM effects. `dioxus-fx` selective CSS effects may be convenient but should not dictate UI. Every optional package needs pinned version/license, WebView smoke test and bundle diff; avoid runtime CDN. See DESIGN.md's researched library links.

## QA per feature
- Web + Android narrow/wide comparisons, screenshot references and real accessibility contrast tests.
- Real state transition trace idle → listening → thinking → speaking → idle; off/permission denied/recording/error states.
- Reduced-motion, reduced-transparency and forced-color fallback.
- Real CPU/GPU frame time and battery profiling; no client model slowdown or dropped speech.
- Focus rings, accessible labels, noncolor indicators, usable forms and input feedback.
- No fake numbers, no social/leaderboard UI copied from owner image references.

## Workspace navigation and runtime settings

Dialogue session/audio diagnostics belong in Settings → Voice & Audio; local models
and the resource governor in Models & Inference; system telemetry in Developer &
Telemetry; security status and safety findings in Guardian Security. Keep the original
live elements mounted there and show their sections without nested card tab selectors.
The corresponding legacy subsystem links open the relevant Settings category.

While You Were Away replaces Discover in the sidebar and Spaces selector. It is a
separate return-briefing destination, not a Home map-view selector. On phones, where
the sidebar is hidden, a labeled header shortcut keeps it reachable. Closing the
briefing returns Home; reopening it still shows the prepared briefing.
