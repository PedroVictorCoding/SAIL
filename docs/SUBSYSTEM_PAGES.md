# Subsystem workspaces

Home remains the system overview. Open any subsystem from the navigation rail,
Home's subsystem directory, the Spaces chooser, or the Workspace picker on mobile.
The assistant and conversation stay available while navigating.

Every registered `SubsystemId` has a linkable workspace:

| Page | URL fragment | Connected interface |
| --- | --- | --- |
| Core | `#core` | Device metrics |
| Dialogue | `#dialogue` | Voice session, audio diagnostics, voice preferences |
| Arena | `#arena` | Responsibilities; knowledge browser not connected yet |
| Canvas | `#canvas` | Visual results, camera and desktop preview tabs |
| Flow | `#flow` | Responsibilities; workflow controls not connected yet |
| Discovery | `#discovery` | Responsibilities; research controls not connected yet |
| Agents | `#agents` | Authorized tools and activity |
| Mind | `#mind` | Local models, governor, inference preferences |
| Bridge | `#bridge` | Host tools and activity |
| Guardian | `#guardian` | Safety review and owner approval preferences |
| Forge | `#forge` | Coding jobs, enqueue dialog, live output, coding preferences |

Use `#home` to return to the overview. Reloads and browser back/forward preserve
the selected route. Unknown routes return to Home. Closing Spaces preserves the
current workspace. Pages reuse the original live card elements, so navigation
does not duplicate IDs, reset the conversation, or disconnect event listeners.
The additional Spaces shortcuts remain available; Vision opens Canvas's camera tab.

## Browser regression check

Install the dependencies in `scripts/voice/requirements-test.txt` and the Playwright
Chromium browser, then run:

```sh
python scripts/voice/subsystem_pages_test.py
```

This serves the checkout locally with stub health responses and no live WebSocket
backend. It checks that every registered subsystem has a page, keyboard focus,
route history and deep links, settings navigation, Forge's enqueue dialog,
original card identity, and layouts at desktop and mobile widths. It does not
exercise model inference, hardware capture, or real job submission.
