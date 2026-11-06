"""While You Were Away cards in a real browser, fixture briefing.

The five cards sit in a grid that fills the stage (several columns and rows on desktop,
one column on phones); each card is lit from below by its own hue. Checks: grid layout,
the left rail fits the window without scrolling,
with no sideways scroll, every card above the mode bar on desktop, an overflowing card
body is keyboard-scrollable, actions keep working, no page overflow at 1440 and 390px,
forced colours drop the glow, zero page errors.

Run with: .venv-voice/bin/python scripts/ui/away_cards_browser_test.py [shot-dir]
"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / "crates/sail-dialogue/web"
ICONS = ROOT / "assets/source/icons/sail-icons.svg"
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp")
ALIASES = {
    "/": "index.html",
    "/vendor/paper-shaders.js": "vendor/paper-shaders/paper-shaders.js",
    "/vendor/liquidglass.js": "vendor/liquidglass/liquidglass.js",
    "/vendor/xterm.js": "vendor/xterm/xterm.js", "/vendor/addon-fit.js": "vendor/xterm/addon-fit.js",
    "/vendor/xterm.css": "vendor/xterm/xterm.css",
}
BRIEFING = {
    "headline": "Welcome to SAIL. Here is your prepared session staging.",
    "local_time_str": "Fri, Nov 6 · 19:00",
    "dismissed": False,
    "staged_music": {
        "circadian_slot": "Evening Wind-Down", "name": "[4 Hz] Delta Green Noise | 8 Hours of Green Noise with Ethereal Ambient Music and Delta Waves",
        "by": "Pedro", "is_spotify": True, "uri": "spotify:playlist:x",
        "rationale": "Calm, acoustic or ambient selections to unwind into the evening. " * 6,
    },
    "learning_plan": {"domain": "Arena Learn", "topic": "What is a TCP acknowledgment?", "estimated_minutes": 15,
                      "rationale": "Resume where you left off in your active Arena Learn curriculum."},
    "events_to_watch": [{"urgency": "clear", "due_relative": "Clear", "title": "No urgent events pending. You have a clear schedule horizon."}],
    "news_radar": [
        {"relevant_interest": "Kernel Security & Guardian", "source": "Kernel Security Advisory", "url": "https://example.org/a",
         "title": "Linux Landlock LSM: Scoped Network & IPC Policies Landed", "summary": "Landlock extends sandbox protections to socket operations."},
        {"relevant_interest": "Rust & Asynchronous Systems", "source": "Rust Systems Digest", "url": "https://example.org/b",
         "title": "Rust Async Concurrency: Cooperative Cancellation Best Practices", "summary": "Patterns for deterministic cancellation tokens across tokio select! branches."},
    ],
    "project_continuity": {"active_workspace": "SAIL", "forge_summary": "Branch verified. All workspace tests passing.",
                           "suggested_action": "Inspect your return briefing cards below and launch your staged music or learning lesson."},
    "system_sentinel": {"storage_status": "Root: 37.0 GB free", "memory_headroom": "5.6 / 30.0 GB used"},
}
WS_STUB = "class W{constructor(){this.readyState=0;setTimeout(()=>{this.readyState=1;this.onopen&&this.onopen()},10)}send(){}close(){}};W.OPEN=1;window.WebSocket=W;"


def route(r):
    path = "/" + r.request.url.split("://", 1)[1].split("/", 1)[1].split("?")[0]
    if path == "/icons.svg": return r.fulfill(body=ICONS.read_bytes(), content_type="image/svg+xml")
    if path == "/api/briefing/latest": return r.fulfill(json={"briefing": BRIEFING})
    if path.startswith("/api/"): return r.fulfill(status=404, json={})
    f = WEB / ALIASES.get(path, path.lstrip("/"))
    if not f.is_file(): return r.fulfill(status=404, body="")
    r.fulfill(body=f.read_bytes(), content_type={".css": "text/css", ".js": "text/javascript"}.get(f.suffix, "text/html"))


errors = []
with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox", "--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"])
    for width, height in ((1440, 900), (390, 844)):
        ctx = b.new_context(viewport={"width": width, "height": height})
        pg = ctx.new_page(); pg.on("pageerror", lambda e: errors.append(str(e)))
        pg.add_init_script(WS_STUB); pg.route("**/*", route)
        pg.goto("https://sail.test/")
        pg.wait_for_function("!document.getElementById('returnBriefingDeck').hidden")
        pg.locator(".tm-seg button, button").filter(has_text="While You Were Away").first.click()
        row = pg.locator("#returnBriefingDeck .briefing-cards-grid")
        row.wait_for()
        pg.wait_for_timeout(400)

        info = pg.evaluate("""() => { const r = document.querySelector('#returnBriefingDeck .briefing-cards-grid'); const cs = getComputedStyle(r);
          const cards = [...r.querySelectorAll('.briefing-card')], rect = c => c.getBoundingClientRect();
          return { display: cs.display, scrollsX: r.scrollWidth > r.clientWidth + 1,
                   hues: cards.map(c => getComputedStyle(c).getPropertyValue('--card-a').trim()),
                   rows: new Set(cards.map(c => Math.round(rect(c).top / 8))).size,
                   cols: new Set(cards.map(c => Math.round(rect(c).left))).size,
                   widths: new Set(cards.map(c => Math.round(rect(c).width))).size }; }""")
        assert info["display"] == "grid" and not info["scrollsX"], info
        assert len(set(info["hues"])) == 5, f"each card has its own light: {info['hues']}"
        if width > 400:
            assert info["rows"] >= 2 and info["cols"] >= 2, ("cards form a grid, not one row", info)
        else:
            assert info["cols"] == 1, ("one column on phones", info)
        assert info["widths"] == 1, ("equal-width cards", info)
        assert pg.evaluate("document.documentElement.scrollWidth <= innerWidth"), f"page overflow at {width}"
        if width > 400:
            # The left rail fits the window with no scrolling, here and in a short window, with
            # Now Playing shown at its real size (Spotify isn't in the fixture).
            pg.evaluate("""() => { const np = document.getElementById('tmNowPlaying'); np.hidden = false;
              np.innerHTML = '<div class=tm-np-row><span class=tm-np-art></span><div class=tm-np-text><p class=tm-np-label>Now Playing</p>'
                + '<p class=tm-np-name>A song</p><p class=tm-np-by>An artist</p></div></div><div class=tm-np-ctl><button class=tm-np-btn>1</button></div>'; }""")
            for h in (height, 720):
                pg.set_viewport_size({"width": width, "height": h}); pg.wait_for_timeout(200)
                over = pg.evaluate("(s => s.scrollHeight - s.clientHeight)(document.querySelector('.tm-side'))")
                assert over <= 0, f"rail scrolls by {over}px at {width}x{h}"
            pg.set_viewport_size({"width": width, "height": height}); pg.wait_for_timeout(200)
        # A whole card (light and action included) fits above the mode bar.
        fit = pg.evaluate("""() => { const cards = [...document.querySelectorAll('#returnBriefingDeck .briefing-card')];
          const c = (innerWidth > 400 ? cards[cards.length - 1] : cards[0]).getBoundingClientRect();
          const bar = document.querySelector('.tm-ctl-mid')?.getBoundingClientRect(); return [c.bottom, bar ? bar.top : innerHeight]; }""")
        assert fit[0] <= fit[1] + 1, f"card bottom {fit[0]} below mode bar {fit[1]} at {width}"
        # An overflowing body is scrollable by keyboard; short ones add no tab stop.
        focus = pg.evaluate("""() => [...document.querySelectorAll('#returnBriefingDeck .briefing-card-body')].map(b =>
          [b.closest('.briefing-card').id, b.scrollHeight > b.clientHeight + 1, b.getAttribute('tabindex') === '0'])""")
        assert all(over == tab for _, over, tab in focus), focus
        assert any(over for _, over, _ in focus), f"fixture has an overflowing card: {focus}"
        pg.screenshot(path=str(SHOTS / f"away-cards-{width}.png"))

        # Actions still work: Continue Lesson navigates.
        pg.evaluate("window.__nav=[]; window.sailNavigate=(id)=>__nav.push(id)")
        pg.locator("#briefingStartLearnBtn").scroll_into_view_if_needed(); pg.locator("#briefingStartLearnBtn").click()
        assert "arena" in pg.evaluate("__nav"), "Continue Lesson opens Arena"
        ctx.close()

    ctx = b.new_context(viewport={"width": 1440, "height": 900}, forced_colors="active")
    pg = ctx.new_page(); pg.add_init_script(WS_STUB); pg.route("**/*", route); pg.goto("https://sail.test/")
    pg.wait_for_function("!document.getElementById('returnBriefingDeck').hidden")
    pg.locator("button").filter(has_text="While You Were Away").first.click(); pg.wait_for_timeout(300)
    assert pg.evaluate("getComputedStyle(document.querySelector('#briefingMusicCard'), '::before').display") == "none"
    ctx.close(); b.close()

assert not errors, errors
print("Away cards: a grid of five lit cards with no sideways scroll, keyboard-scrollable "
      "overflowing body, working actions, forced colours at 1440 and 390px passed (fixture briefing)")
