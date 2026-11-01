# SAIL — Subsystem specification index

Status: design and implementation reference. These docs describe intended behavior, not claims that features have shipped. **Source of truth for project history**: [Roadmap issue #15](https://github.com/PedroVictorCoding/SAIL/issues/15). The roadmap began with 20 subsystems / 6 domains and was extended by Health #104 to **21 product subsystems / 7 domains**. Core is shared infrastructure; Studio is an additional user-facing workspace specification and must be reconciled with the taxonomy before formal counting.

| Domain | Specifications |
|---|---|
| Intelligence & Knowledge | [MIND](SAIL_MIND.md) · [ARENA](SAIL_ARENA.md) · [DISCOVERY](SAIL_DISCOVERY.md) · [LAB](SAIL_LAB.md) |
| Interaction & Presence | [DIALOGUE](SAIL_DIALOGUE.md) · [VISION](SAIL_VISION.md) · [SENSE](SAIL_SENSE.md) · [RELAY](SAIL_RELAY.md) |
| Agency & Creation | [AGENTS](SAIL_AGENTS.md) · [FLOW](SAIL_FLOW.md) · [CANVAS](SAIL_CANVAS.md) · [FORGE](SAIL_FORGE.md) |
| Personal & Social | [LIFE](SAIL_LIFE.md) · [PULSE](SAIL_PULSE.md) · [FRIENDS](SAIL_FRIENDS.md) · [ENTERTAINMENT](SAIL_ENTERTAINMENT.md) |
| Systems & Security | [GUARDIAN](SAIL_GUARDIAN.md) · [BRIDGE](SAIL_BRIDGE.md) · [SENTINEL](SAIL_SENTINEL.md) |
| Infrastructure & Compute | [MESH](SAIL_MESH.md) |
| Health & Physiology | [HEALTH](SAIL_HEALTH.md) |
| Shared foundation | [CORE](SAIL_CORE.md) |
| Cross-cutting experience | [STUDIO](SAIL_STUDIO.md) |

## Design rules
- Specs are per-capability **ownership and acceptance contracts**, not permission to add navigation tabs, new processes or microservices.
- Active client targets: **Android + Web** through the shared Rust/Dioxus UI. Linux host and local inference are primary; no mandatory cloud inference.
- User-impacting implementation requires real end-to-end tests, tested authorization, loading/error/reconnect states, and verifiable backend receipts.
- Integration issue links and priorities are a starting point; defer to current per-issue scope and repo reality.
- Keep ArtCraft Studio document architecture separate from transient Canvas; Guardian is authoritative for all privileged effects.
