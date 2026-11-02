# Shared UI design rules

Use neumorphism as SAIL's visual design system. These rules apply to new UI
components and changes to existing UI, regardless of which coding assistant makes
the change. Treat them as acceptance criteria for the requested scope.

Preserve SAIL's dark graphite surfaces and restrained orange accent. Use the
[SAIL UI Design Guide](docs/SAIL_UI_DESIGN_GUIDE.md) for project-specific tokens,
component treatments, and visual direction. The requirements below govern
accessibility, consistency, implementation, and verification; simplify a visual
effect whenever it conflicts with usability or accessibility.

## Visual foundation

- Use a restrained neutral surface palette and one primary accent color.
- Create depth through paired shadows: a light highlight above-left and a soft
  dark shadow below-right. Keep the light source consistent.
- Use raised surfaces for interactive controls and inset surfaces for inputs,
  selected states, and pressed controls, while keeping those states distinguishable.
- Reserve depth for meaningful hierarchy. Do not place every element inside a
  raised card.
- Keep spacing, corner radii, typography, and component proportions consistent.

## Design tokens

- Define reusable tokens for surfaces, text, muted text, accent, borders, radii,
  spacing, and raised/inset shadows.
- Reuse tokens throughout the UI instead of scattering hardcoded styles.
- Adapt tokens to the existing theme and component system.
- Support light and dark themes when the application supports both.

## Accessibility

- Maintain WCAG AA text contrast: at least 4.5:1 for normal text and 3:1 for large
  text. Target at least 3:1 for required control boundaries and focus indicators
  against adjacent surfaces.
- Never rely on shadows or color alone to communicate boundaries, interactivity,
  selection, or errors.
- Provide visible keyboard focus, semantic HTML, accessible labels, and clear
  hover, pressed, selected, disabled, loading, and error states.
- Respect `prefers-reduced-motion`. Keep transitions subtle and short.

## Implementation

- Preserve existing behavior and responsive layouts.
- Prefer shared components and existing dependencies.
- Keep shadows static during continuous animation. Avoid expensive layered blur
  effects.
- Make controls usable on touchscreens and at narrow viewport widths.
- Pair icons with text labels where their meaning is ambiguous.
- Keep product text clear and free of implementation jargon.

## Verification and enforcement

- Check changed components for visual consistency, accessibility, keyboard
  operation, responsive behavior, and rendering cost.
- Fix violations within the requested scope before completing the task.
- Report what was verified and any remaining limitations. Do not claim visual or
  accessibility checks that were not performed.
- If a neumorphic effect reduces usability or accessibility, simplify the effect
  and preserve the functional requirement.
- Apply these rules to the requested change; they do not require an unrelated
  redesign or imply that the existing UI has already passed verification.
