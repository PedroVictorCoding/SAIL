# SAIL Design System — Ambient Glass, Living Light & State-Driven Motion

**Status:** adopted. The tokens and primitives are implemented in `crates/sail-dialogue/web/tokens.css`; the Reader Library composition (#339, #341) is the reference implementation for a calm, information-dense workspace. Live voice/Orb states still need their own measured slices.
**Authority:** this file governs new or materially revised UI. `docs/SAIL_UI_DESIGN_GUIDE.md` provides component implementation examples. The eight owner-supplied visual references are documented in `docs/design/AMBIENT_GLASS_REFERENCES.md`; artwork must not be copied as production UI.

## Reference implementation

The Reader Library (`crates/sail-dialogue/web/reader.css`, `#library`) is the worked
example to copy from:

- it consumes only shared `--sail-*` tokens for material, typography, radii and focus;
- it composes a navigator, a featured/primary region and a contextual activity column,
  stacking them below 1180px and 700px instead of hiding capabilities;
- illumination is a single static localized tint, with no blur, no new activity reducer,
  no continuous animation and no external artwork;
- metrics (progress, counts, timestamps) come from persisted records, never invented;
- it includes `prefers-reduced-transparency`, `forced-colors` and keyboard-focus handling;
- its Playwright fixture test covers 1440/768/390/360 widths and the empty/error states.

New workspaces follow that material and composition discipline unless a live,
immersive state (Section 3) explicitly justifies more motion.

## Design goal

SAIL should feel like a **living, intelligent material**, not a control dashboard: deep near-black space, subtle translucent glass, soft chromatic light blooming *within* components, and purposeful transitions linked to **verified SAIL state**. The app must remain calm and functional when idle or reading. Bright gradients are reserved for the active, focused region, not applied to every surface.

In order of priority: **truthful state > readability > latency/battery > interaction clarity > visual spectacle**. No standalone cosmetic subsystem, new fake telemetry or duplicated frontend state.

## 1. Design hierarchy

1. **Canvas:** deep neutral charcoal/black; content is largely flush and typography-led.
2. **Glass region:** subtle translucency and edge contrast for contextual sheets, navigators, player overlays and selected information panes; should not create a blurry wall of cards.
3. **Chromatic illumination:** controlled color field from a real active state, task status or selected content. Prefer soft emissive gradients and low-opacity outer glow over neon borders.
4. **Primary action:** tactile glass capsule with clear text/icon and focus indication, not fully transparent mystery buttons.
5. **Live / immersive:** Orb, speech, recording, audio, device status, progress and a few featured analytical cards may employ animated gradient light and softly evolving geometry.
6. **Reading/study:** long text, note editors, calendar time-grid and Arena Learn exercises stay neutral, stable and non-animated. The ambient background must never fight reading.

**Do not copy** a reference's logos, wording, metrics, art, unique illustrations, third-party assets, product layout verbatim or consumer paywall/sales screens.

## 2. Palette / material tokens (starting values — verify in browser/device)

```css
:root {
  --sail-base: #0a0c10;
  --sail-surface: #141820;
  --sail-surface-raised: #1d232c;
  --sail-glass-fill: rgb(22 29 40 / .65);
  --sail-glass-border: rgb(222 238 255 / .13);
  --sail-glass-highlight: rgb(255 255 255 / .12);
  --sail-text: #f2f5fa;
  --sail-text-muted: #aeb8c8;
  --sail-text-subtle: #8592a6;
  --sail-focus-ring: #ffd69f;
  --sail-radius-control: 14px;
  --sail-radius-card: 24px;
  --sail-radius-sheet: 30px;
  --sail-blur-supported: 12px;
  --sail-transition-fast: 140ms;
  --sail-transition-state: 420ms;
  --sail-ease: cubic-bezier(.22, 1, .36, 1);
  --sail-light-a: #78889d; /* injected from verified state */
  --sail-light-b: #384257;
  --sail-light-c: #0e121a;
  --sail-illumination: .15;
}
```

Tokens are semantic and CSS custom properties are the public styling interface. **The example values are starting points, not a license to ignore contrast.** A status light may use strong saturation, but never a saturated full-page gradient by default.

- Glass surface: at most two overlapping translucent layers in an ordinary viewport. Use native `backdrop-filter` only if supported and measured; otherwise use opaque `--sail-surface-raised` with a static gradient and border.
- Blur: minimize large animated backdrop filters. Reuse a pre-rendered radial-gradient plane behind content and animate *opacity / transform* rather than re-rasterizing expensive filter stacks.
- Shadows: broad low-opacity ambient glow for selected/live elements; no thick persistent neon outlines.
- Text contrast: WCAG AA **4.5:1 normal, 3:1 large**, and visible keyboard/focus boundaries **at least 3:1** where required. Decorative color fields are not text backgrounds unless a contrast-proof scrim is present.
- Materials can be flat, glass, or luminous. Don't revive ubiquitous deep neumorphic emboss/inset treatments. An inset slider track is allowed for usability, not as the identity of the new system.

## 3. State-driven chromatic language

**State must come from authenticated, authoritative typed SAIL events**, not a model deciding a color or client guessing model activity. The shell surfaces a single primary ambient state and localized status for each active tool/card.

| Semantic state | Visual intent | Suggested colors | Motion | Claim condition |
| --- | --- | --- | --- | --- |
| `idle` / `ready` | calm, available | graphite + cool blue `#496f9d` | static or barely breathing | connected, no active interaction |
| `listening` | focused and receptive | teal/cyan `#19b5bc`, midnight blue | slow inward breathing / reactive mic amplitude | OS mic capture genuinely active |
| `thinking` / `processing` | computational work | violet/indigo `#8567ff`, blue | gentle directional drift | real active inference/tool task |
| `speaking` | outward delivery | amber/coral `#ff995d`, magenta `#df6c90` | restrained audio-envelope-reactive pulses | PCM actually playing, not synthesis completed |
| `executing` | progressing external action | electric blue `#4e91fc`, turquoise | purposeful traveling light, not fake percentage | authorized tool executing |
| `learning` | active study | purple `#9b7dff`, cyan | soft focus, low motion | active Arena Learn teaching/practice session |
| `recording` / `listen_only` | consented capture | warm red `#f56b66`, muted amber | measured steady pulse + **unambiguous REC indicator** | audio session capture actual and consented |
| `completed` | finished | green `#5ccda4`, gold | one gentle confirmation; then settle | confirmed completed receipt |
| `warning` | attention needed | amber `#f3b05a` | static emphasis, no distracting animation | credible warning event |
| `error` / `disconnected` | blocked/unavailable | desaturated red `#ed7275` / slate | no rhythmic animation; direct diagnostic label | actual error/disconnection |

Color is **never the sole status indicator**: icon, label, speech capture indicator and proper aria-live/status semantics remain required. `thinking` is not an excuse to hallucinate hidden work. `listening` is not permitted when mic permission is denied; `speaking` requires playback acknowledgment; recording must be visible independent of ambient animation.

### Arbitration / priority
- Explicit errors, permission requests and active recording indicators cannot be suppressed by decorative states.
- Localized audio state (speaking/listening) wins shell's ambient visual when currently active; background Forge/Flow tasks appear only in Activity/project surfaces.
- The single global shell ambient state is selected by a typed reducer from **live active session state**, with monotonic sequence/session identity. Stale/out-of-order messages must not reanimate obsolete states.
- A current foreground user action supersedes ambient proactive background work. `completed` is a transient *confirmed* phase followed by ready, not a permanent green shell.
- User-selected quiet mode reduces motion/illumination without changing semantic labels or suppressing privacy indicators.

### Transition principles
- Most focus/hover transitions: 120–200ms. State changes: ~300–650ms ease (duration is a guideline).
- Animated gradients transition through an underlying state vector; no hard RGB flash. Use alpha crossfade between **two** pre-computed gradient layers when interpolating complex gradients.
- Speaking waveform can use smoothed amplitude / envelope events at limited ~15–30fps, where actually available, with graceful reduction to a steady animation. Don't feed raw PCM to CSS.
- Listening uses real VAD/activity indication *only if user has enabled voice activity visualization*; privacy and battery constraints win.
- No autoplaying decorative shader if the app is backgrounded; pause when tab hidden / Android lifecycle suspended.

## 4. Component patterns

### Ambient Orb
Small at rest and only visually prominent for active voice. Color, breathing, glow and pulse are derived from the state table. A disconnected Orb must not indicate connection. Provide static icon/text fallback, reduced-motion mode and screen-reader state.

### Glass panels and contextual cards
Minimal floating glass for playback, system activity, tool inspector, permission reviews and compact reader controls. Prefer flat panes with a single soft radial glow at a corner; chart content must be accurate. Data cards show real timestamps, units and sources; no fake progress percentages.

### Library, reading and study workspaces
Use the #341 composition: a thin navigator, a primary content column, and one contextual
column for saved activity or related material. The shell uses an opaque `--sail-surface`
with one static localized tint; do not stack translucent cards or animate the backdrop
behind text. Progress is percentage-by-text from the persisted position, and activity
lists show real saved timestamps. Reading and exercise text keeps its own neutral,
non-animated theme. Below 1180px the contextual column moves under the content; on phones
all regions stack with full-width actions. The same rules apply to Arena Learn lesson and
practice surfaces: truthful state, stable text, no ambient motion while the learner reads.

### Timelines and daily phases
Use a vertical timeline / anchored events for Life, Pulse, Calendar and Health schedules. The gradient tint can reflect task class or phase when real data exists. **Do not infer medical recommendations from a designer reference.** Calendar remains source-truth from #335.

### Progress and analytics
Small illuminated lines, discrete dots, progress arcs, smooth bar transitions; animate only when values change. Percentage must correspond to a real measurement. Avoid 3D/extruded charts or unreadable colorful labels.

### Mobile
One primary action zone, compact gestures, large controls; preserve Home/Talk/Listen/Learn/Activity as context, not many static tabs. Use GPU-light CSS treatments; Android WebView may lack some effects or suspend animation. Verify safe-area insets, 360px width, touch targets, keyboard overlay, locked-screen modes.

### Navigation and feedback
Clear and semantic; responsive glass sheets only when needed. Selected navigation must have shape/text/position state beyond glow. Never let a glass overlay make buttons inaccessible. Avoid notification flood and pointless looping animations.

## 5. Library shortlist (evaluate; do not install all)

**Default stack**: existing Dioxus components + CSS custom properties + `radial-gradient`/`linear-gradient` + CSS transforms/opacity/transitions + a tiny typed state reducer. Prefer minimal JS.

- **Dioxus 0.7 WebView/CSS**: primary rendering target already chosen by #38. Mobile docs: https://dioxuslabs.com/learn/0.7/guides/platforms/mobile/
- **`dioxus-motion`** (Rust, spring/tween/color interpolation): evaluate compatibility against pinned Dioxus in workspace, test only one component before broad dependency. https://docs.rs/dioxus-motion/latest/dioxus_motion/
- **Motion for JavaScript** (MIT, framework-independent DOM animate and springs): optional for fine-grained transitions; use framework-agnostic `motion`, **not Motion React** in a Dioxus app. Pin/bundle local module, no runtime CDN. https://motion.dev/docs/quick-start
- **dioxus-fx** (Dioxus 0.7 CSS effects): optionally borrow limited accessibility-reviewed surface/entrance primitives; **do not adopt all 185 effects** or create a visual-effects junk drawer. https://github.com/Dodecahedr0x/dioxus-fx
- **CSS only** for blur, glow, glass and most cards; `Canvas 2D` for a single low-fps Orb visualizer if profiling justifies; **no WebGPU mandatory dependency**.
- Advanced shader/gradient libraries (OGL, Three.js, PixiJS) require a separate GPU/compatibility budget review; avoid adding a WebGL canvas per card or competing with client-side Whisper WebGPU. Use only where CSS measurably fails and the exact effect improves usability.

Required ADR compares runtime size, Dioxus interop, license, Android WebView support, reduced-motion compliance, bundle impact, graphics workload, maintenance and resource priority. At most one animation helper dependency in the first slice.

## 6. Performance / accessibility / fallback gates

- `prefers-reduced-motion: reduce`: stop looping animations, transition instantly or briefly, preserve state changes via text/icon.
- `prefers-reduced-transparency` where present or in-app transparency reduction: use opaque surfaces. `forced-colors`: retain borders/focus labels without dependency on gradients/blur.
- Background/low-power mode: gradients static, blur removed, Orb no loop; WebGPU availability never required for ordinary interface.
- Target smooth interaction on actual Web and Android, but measure frame times, long tasks, memory, GPU usage/battery. A stable 30fps low-cost ambient effect is better than jittery 60fps. Never trade SAIL voice responsiveness for a visual.
- Respect OS high contrast, accessible hit areas and screen-reader semantics; avoid high-frequency flashes (>3/sec) or oscillation/vertigo effects.
- Blur/fog never over readable book text (Reader #333), lesson text (Arena Learn #215), private recording controls or evidence citations.
- Feature flag: `ambient_visuals = off | subtle | expressive` with persistent owner choice and reliable static fallback.
- No third-party graphics/font CDN runtime requests in private deployment #334; all assets packaged/served locally and subject to authorized access.

## 7. Implementation contract / verification

1. Audit current SAIL component/tokens/CSS and status event protocol; preserve one authoritative visual state reducer in client UI. Make a crosswalk from former neumorphic tokens and remove obsolete universal shadow classes deliberately.
2. Create shared `AmbientState` enum/state reducer + CSS `data-sail-state` hooks, semantic colors, a single `GlassSurface` primitive, `LuminousAccent`, `AmbientBackdrop`, and optional `OrbReactive`. Avoid new renderers, duplicate telemetry, model-driven HTML/CSS.
3. Implement **one vertical slice**: ready → listening → thinking → speaking → ready, and real disconnected/error/recording semantics on Web and Android. Simulated fixture only in Storybook/demo, never in live UI. The Reader Library (#341) is the completed calm-workspace slice: reuse its tokens, responsive composition and verification rather than inventing a second material system.
4. Add focused component tests and a manual storyboard capturing at least one screenshot for each state and breakpoint (desktop 1440, mobile 360/390). Compare against owner-provided references and `docs/design/AMBIENT_GLASS_REFERENCES.md`.
5. Profile before using advanced shaders; test Web GPU-limited and reduced-motion/forced-colors modes. No production fake numbers or state.
6. Use actual `DESIGN.md` as code-review acceptance. Mark visual QA limitations honestly; do not claim rendered comparison unless performed on target devices.

**Design principle: the interface glows because SAIL is doing something, not to imply that it is.**
