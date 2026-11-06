# SAIL UI References — source image inventory

Seven images were supplied by the owner on **2026-11-06** to guide SAIL's active **Ambient Glass / Glow / Gradient / Motion** visual redesign, tracked in [issue #364](https://github.com/PedroVictorCoding/SAIL/issues/364). These are **design references**, not production assets to embed, copy or distribute in the SAIL application.

> **Asset delivery status:** The seven original PNGs were provided in chat, but the current GitHub connector cannot transfer these binary files. This README records exact expected names and SHA-256 checksums. **Do not assume the PNGs have already been committed.** The owner can extract the prepared ZIP archive into the repository root and commit the resulting seven files.

| Expected filename | Reference meaning | SHA-256 |
| --- | --- | --- |
| `01-finance-gradient-mobile.png` | Saturated blue-to-black onboarding, color layering, CTA contrast; ignore paywall and brand | `d3bcf1b60af7d3a278633122c662637d1b728cb069512490055bda80ff886a10` |
| `02-blue-analytics-glow.png` | Deep-blue luminous analytics card, readable data/metric, restrained grid | `6e045e0f08c5dc71598481848b45cff503e7597d57a6cb56d258d7a8b71433d2` |
| `03-alarm-analytics-light-glass.png` | Bright soft green-blue light, subtle transparent cards, sparse clock/dashboard | `203d08ebbbecf43ef96d401bfa687be0db341b0f62d8b85037ae90277097e5d1` |
| `04-health-activity-dotted-ui.png` | Dot-matrix values, focused interactive measurement card, gentle chromatic panels | `1f08c70673c3043b5f135067e3f1985e09a8f9e274833d9b5560303d2de94549` |
| `05-project-progress-dark-cards.png` | Project progress cards with colored upper highlights and honest status | `dbcf0fc8263933a35fdeea292b96b79f76c7472e45174908cd79c8e94335ce91` |
| `06-luminous-feature-panels.png` | Small dark informational cards, vibrant localized soft glow, strong whitespace | `0d0fad4ec80a149ce108eace76a1eb447d97592907c916fbeeeaad1aeaad0070` |
| `07-day-phase-gradient-timeline.png` | Vertical time phases, phase-specific lower-edge glow, compact action tiles | `a99e20742982173cc9976d85d9de52eb70aae944b4adcb1150bdf57765e01793` |

## For coding agents

Read `DESIGN.md`, `docs/SAIL_UI_DESIGN_GUIDE.md`, and issue #364. These screenshots are **not** design specifications by themselves: derive shared semantic tokens, do not trace/copy proprietary UIs, branding, metrics, illustrations or text. The active workspace is Factory/Tools Map, not legacy Classic. Color glows must correspond to **verified** ready/listening/thinking/speaking/recording/executing/etc runtime states.

Each original PNG should live **in this directory** as named above. Do not bundle these inspiration images into shipped application assets or expose them over the private SAIL instance by default. Verify each image SHA-256 before treating it as a matching source.

## Adding the images

From the root of the SAIL repository, extract the owner-provided `SAIL_UI_references_repo_ready.zip` (its paths begin with `assets/UI references/`), then commit the seven PNGs. Do not extract the archive into this folder itself or the paths will nest incorrectly.

```bash
unzip SAIL_UI_references_repo_ready.zip
git add "assets/UI references"
git commit -m "assets: add seven UI design reference images"
```

After commit, update this README by removing the pending-status paragraph and add relative embeds if useful for coding-agent browsing.
