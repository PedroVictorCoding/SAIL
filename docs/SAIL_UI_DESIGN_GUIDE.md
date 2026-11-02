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
