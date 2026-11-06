"""Tools Map light response in a real browser, fixture /ws/voice transport.

The map's block faces mix in a little of the light the backdrop shows (after its
cooldown), so a state change (idle -> speaking) re-tints them with the backdrop; a sheen drifts over the map and the core halo
breathes. The change stays small: a top face moves only a few percent towards the
light. Reduced motion keeps the tint and stops the motion; ambient visuals off and
forced colours remove it. Zero page errors.

Run with: .venv-voice/bin/python scripts/ui/map_light_browser_test.py [shot-dir]
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
WS_STUB = """
window.__sockets=[];
class FakeWS { constructor(u){ this.url=u; this.readyState=0; __sockets.push(this); setTimeout(()=>{this.readyState=1; this.onopen&&this.onopen();},10);} send(){} close(){ this.readyState=3; this.onclose&&this.onclose(); } }
FakeWS.OPEN=1; window.WebSocket=FakeWS;
window.__push=(m)=>{ const s=__sockets.at(-1); s.onmessage({data:JSON.stringify(m)}); };
"""
BASE_TOP = (23, 27, 35)  # #171b23, the unlit slab top


def route(r):
    path = "/" + r.request.url.split("://", 1)[1].split("/", 1)[1].split("?")[0]
    if path == "/icons.svg": return r.fulfill(body=ICONS.read_bytes(), content_type="image/svg+xml")
    if path.startswith("/api/"): return r.fulfill(status=404, json={})
    f = WEB / ALIASES.get(path, path.lstrip("/"))
    if not f.is_file(): return r.fulfill(status=404, body="")
    r.fulfill(body=f.read_bytes(), content_type={".css": "text/css", ".js": "text/javascript"}.get(f.suffix, "text/html"))


def rgb(text):
    import re
    nums = [float(n) for n in re.findall(r"[\d.]+", text)]
    if text.startswith("color(srgb"): return tuple(round(n * 255) for n in nums[:3])
    return tuple(round(n) for n in nums[:3])


TOP = "getComputedStyle(document.querySelector('.tm-box.slab .f-top')).fill"
errors = []


def open_map(b, **ctx_kw):
    storage = ctx_kw.pop("storage", None)
    ctx = b.new_context(viewport={"width": 1440, "height": 900}, **ctx_kw)
    pg = ctx.new_page(); pg.on("pageerror", lambda e: errors.append(str(e)))
    if storage: pg.add_init_script(f"localStorage.setItem('sail.ambientVisuals', {storage!r});")
    pg.add_init_script(WS_STUB); pg.route("**/*", route)
    pg.goto("https://sail.test/")
    pg.wait_for_function("document.documentElement.dataset.sailState==='idle' && document.querySelector('.tm-sheen')")
    pg.wait_for_timeout(700)
    return ctx, pg


with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox", "--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"])

    ctx, pg = open_map(b)
    idle = rgb(pg.evaluate(TOP))
    assert idle != BASE_TOP, "top faces carry a little of the idle light"
    assert max(abs(a - c) for a, c in zip(idle, BASE_TOP)) <= 16, f"tint stays light: {idle} vs {BASE_TOP}"
    sheen = pg.evaluate("(() => { const s = getComputedStyle(document.querySelector('.tm-sheen')); return [s.animationName, s.pointerEvents, +s.opacity, s.mixBlendMode]; })()")
    assert sheen[0] == "tm-sheen-drift" and sheen[1] == "none" and 0 < sheen[2] <= .2 and sheen[3] == "screen", sheen
    assert pg.evaluate("getComputedStyle(document.querySelector('.tm-halo')).animationName") == "tm-halo-breathe"
    # Labels stay above the light and clickable.
    hit = pg.evaluate("(() => { const l = document.querySelector('.tm-label'); const r = l.getBoundingClientRect(); return document.elementFromPoint(r.x + r.width/2, r.y + r.height/2)?.closest('.tm-label') === l; })()")
    assert hit, "map labels are not covered by the sheen"
    pg.screenshot(path=str(SHOTS / "map-light-idle.png"))

    pg.evaluate("__push({type:'session',session_id:'s1',learning:true,control:{is_muted:false,max_recording_ms:180000}})")
    pg.evaluate("__push({type:'turn_started',turn_id:1,epoch:1,transcript:'hi',state:'thinking'})")
    pg.evaluate("__push({type:'response_finished',epoch:1})")
    assert pg.evaluate("document.documentElement.dataset.sailState") == "speaking"
    # The map follows the light the backdrop shows, so it honours the same cooldown.
    shown = "document.documentElement.style.getPropertyValue('--sail-shown-a')"
    if pg.evaluate(shown) != pg.evaluate("SailAmbient.STATES.speaking.light[0]"):
        assert rgb(pg.evaluate(TOP)) == idle, "map holds its light while the backdrop holds"
    pg.wait_for_function(f"{shown} === SailAmbient.STATES.speaking.light[0]", timeout=8000)
    pg.wait_for_timeout(1500)
    speaking = rgb(pg.evaluate(TOP))
    assert speaking != idle, "faces re-tint with the state light"
    assert speaking[0] > idle[0], f"speaking (warm) light warms the faces: {idle} -> {speaking}"
    assert max(abs(a - c) for a, c in zip(speaking, BASE_TOP)) <= 24, f"still a light touch: {speaking}"
    pg.screenshot(path=str(SHOTS / "map-light-speaking.png"))
    ctx.close()

    ctx, pg = open_map(b, reduced_motion="reduce")
    assert pg.evaluate("getComputedStyle(document.querySelector('.tm-sheen')).animationName") == "none"
    assert pg.evaluate("getComputedStyle(document.querySelector('.tm-halo')).animationName") == "none"
    assert rgb(pg.evaluate(TOP)) != BASE_TOP, "reduced motion keeps the tint"
    ctx.close()

    ctx, pg = open_map(b, storage="off")
    assert pg.evaluate("getComputedStyle(document.querySelector('.tm-sheen')).display") == "none"
    assert rgb(pg.evaluate(TOP)) == BASE_TOP, "visuals off: original faces"
    ctx.close()

    ctx, pg = open_map(b, forced_colors="active")
    assert pg.evaluate("getComputedStyle(document.querySelector('.tm-sheen')).display") == "none"
    ctx.close()
    b.close()

assert not errors, errors
print("Map light response: faces tint with idle and speaking light (small change), drifting sheen and breathing halo "
      "below clickable labels, reduced motion still, visuals off and forced colours plain passed (fixture transport)")
