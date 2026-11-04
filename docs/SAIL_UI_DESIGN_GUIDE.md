# SAIL UI Design Guide
## Direction: dark tactile neumorphism

### Visual intent

SAIL should feel like a carefully manufactured physical control panel: dark graphite surfaces, softly raised controls, recessed inputs, precise typography, and restrained orange illumination.

Use reference images 3 and 4 as the primary direction. Borrow the dashboard organisation of images 1 and 2 when useful, while retaining the darker material treatment.

### Material and lighting

- Use graphite backgrounds with subtly lighter panels and controls.
- Maintain one consistent light source from the upper left.
- Raised surfaces receive a faint upper-left highlight and a soft lower-right shadow.
- Recessed surfaces use inner shadows with the same light direction.
- Use subtle gradients to suggest curvature.
- Edge highlights should be visible at normal viewing size without resembling bright outlines.
- Reserve strong glow for active controls and meaningful status.

Avoid making every container appear raised. Depth should communicate interaction and hierarchy.

### Initial design tokens

These are starting values to validate against an approved reference screen.

| Token | Value |
|---|---|
| Page background | `#1C2023` |
| Panel surface | `#24292D` |
| Control surface | `#292F33` |
| Recessed surface | `#191D20` |
| Primary text | `#F0F2F3` |
| Secondary text | `#B5BEC5` |
| Accent | `#FF8A32` |
| Focus indicator | `#FFC184` |
| Spacing scale | `4, 8, 12, 16, 24, 32, 48px` |
| Control radius | `12px` |
| Card radius | `20px` |
| Large panel radius | `28px` |

Check contrast for actual text/background combinations. Small orange controls may need dark text or icons.

### Depth system

Use four named surface treatments:

1. **Base:** page background; no shadow.
2. **Panel:** subtle elevation for grouped content.
3. **Raised control:** stronger, compact depth for clickable elements.
4. **Inset:** recessed fields, tracks, and selected segments.

Create shared tokens for these treatments. Do not invent a different shadow for each component.

### Sculpted surfaces: blending, raised and inset relief

The intended effect is **surface continuity**: a button, field or selection appears
formed from the same material as its immediate background. The control remains
recognisable, while its colour stays close to that surrounding surface.

- **Flush:** the resting surface flows into its parent with little or no shadow.
  Use for quiet content and secondary areas, with clear affordances for actions.
- **Raised / embossed / convex:** the control gently protrudes from the surface.
  Pair an upper-left outer highlight with a lower-right outer shadow; a subtle
  surface gradient may suggest curvature.
- **Inset / debossed / concave:** the control is pressed into the surface. Use an
  upper-left inner shadow and a lower-right inner highlight. This is the inverse
  edge response under the same light source, not a change of lighting direction.

These describe the existing Base/Panel/Raised control/Inset treatments, not a new
set of components or a separate theme. Derive the fill from the immediate parent
surface and use shared surface, raised-shadow and inset-shadow tokens. Maintain
SAIL's graphite palette and restrained orange accent; do not copy a reference's
colours wholesale.

Relief should be shallow and soft, with enough separation to read at normal size.
Avoid making controls look like detached floating cards, outlining every element,
adding glossy bevels, or using glow as the main depth cue. Text and dense content
stay relatively flat.

**State mapping:** use raised relief for a resting primary button, a small
highlight change on hover, and reduced elevation or the shared inset treatment
when pressed. A latched selection needs an additional persistent indicator,
position, text or icon plus appropriate semantics (for example, `aria-pressed`
on a toggle button). Do not use inset depth alone to mean both "held down" and
"selected". Inputs and switch tracks may remain recessed with a raised thumb.

The owner's reference is
[Neumorphism Button on Dribbble](https://dribbble.com/shots/25989383-Neumorphism-Button).
It records the requested visual direction, not permission to reuse assets or code.

**Acceptance:** compare raised, inset and flush treatments on their actual parent
surfaces at normal desktop and mobile viewing sizes. Verify labels, control
boundaries, hit areas and focus remain identifiable with mouse, keyboard and
touch. Check reduced-motion and forced-colours/shadow-free fallbacks; depth must
never be the only cue for action or state. Contrast requirements in
[DESIGN.md](../DESIGN.md) take precedence over visual blending. This is guidance
for future UI work, not a claim that existing screens have been revalidated.

### Component behaviour

**Buttons**
- Rest: softly raised with a clear label or icon.
- Hover: slightly stronger highlight.
- Pressed: move down approximately 1px and reduce the outer shadow.
- Selected: use a persistent visual state distinct from momentary pressing.
- Focus: show a clear keyboard outline.
- Disabled: preserve legibility while removing interactive emphasis.

**Switches and segmented controls**
- Place a raised thumb or selection inside a recessed track.
- Use position, text or icons alongside colour to communicate state.
- Preserve recognisable switch semantics.

**Inputs**
- Use recessed surfaces, readable labels, and visible boundaries.
- Keep error messages and focus states explicit.

**Cards**
- Use restrained elevation and generous internal spacing.
- Give interactive cards an identifiable action.
- Avoid styling ordinary content cards exactly like buttons.

**Navigation**
- Keep inactive items quiet but readable.
- Use one consistent selected treatment.
- Keep icon size, alignment, and hit areas consistent.

### Layout and typography

- Use a restrained sans-serif typeface with a clear size hierarchy.
- Prefer readable text over the extremely muted labels in the references.
- Keep dashboard information aligned to a consistent grid.
- Establish hierarchy through spacing and typography before adding shadows.
- Keep dense tables and long text on relatively flat surfaces.
- Adapt layouts for mobile without shrinking controls below comfortable touch sizes.

### Motion

- Use brief transitions, usually 120–200ms.
- Physical controls should move subtly and predictably.
- Use spring motion sparingly for moving thumbs or larger transitions.
- Respect reduced-motion preferences.
- Avoid continuous decorative animation.

### Instructions for AI implementation

Before changing a screen:

1. Identify the relevant approved reference components.
2. Reuse existing tokens, components, and interaction states.
3. Explain any component that requires a new visual treatment.
4. Preserve the established lighting, spacing, typography, and accent rules.
5. Render the result and compare it visually with the approved reference.
6. Verify hover, pressed, selected, focus, disabled, loading, and error states where applicable.
7. Check desktop and mobile layouts.

Do not introduce new colours, shadow recipes, radii, icon styles, or competing component systems without updating this guide.

### Definition of done

A screen is ready when:
- Its materials and lighting match the approved reference.
- Interactive controls are distinguishable from static content.
- Text and focus indicators remain clear.
- Components use shared tokens.
- States work with mouse, keyboard, and touch.
- Screenshots have been reviewed at realistic viewing sizes.
