"""Ambient backdrop (Paper Shaders + liquid glass) in a real browser, fixture transport.

Home: the shader mounts behind the Tools Map, follows the ambient state contract
(idle -> speaking -> idle from /ws/voice events) and never sets state itself; each
light change holds for a cooldown and quick flips are coalesced to the latest state.
Settings: `sail.ambientVisuals=off`, forced colours and reduced transparency mount
nothing; reduced motion keeps a still frame; no WebGL2 falls back to CSS light.
Library: lit by the featured cover, colour tiles carry real measurements only, a
the liquid-glass lens works for a floating control, and the light switches off while a book is open.

Run with: .venv-voice/bin/python scripts/ui/ambient_backdrop_browser_test.py [shot-dir]
"""
import base64, sys
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / "crates/sail-dialogue/web"
ICONS = ROOT / "assets/source/icons/sail-icons.svg"
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp")
ALIASES = {
    "/": "index.html", "/reader": "reader.html",
    "/vendor/paper-shaders.js": "vendor/paper-shaders/paper-shaders.js",
    "/vendor/liquidglass.js": "vendor/liquidglass/liquidglass.js",
    "/vendor/xterm.js": "vendor/xterm/xterm.js", "/vendor/addon-fit.js": "vendor/xterm/addon-fit.js",
    "/vendor/xterm.css": "vendor/xterm/xterm.css",
}
POS = {"percent": 46.8, "revision": 1, "anchor": {"section": 0, "block": 0, "char": 0}, "saved_at": "2026-11-06T11:40:00Z"}
BOOKS = [
    {"id": "one", "title": "The Shape of Attention", "authors": ["Alex Reader"], "format": "epub", "sections": 12, "chars": 1000, "position": POS,
     "metadata": {"work_key": "/works/OL1W", "title": "The Shape of Attention", "authors": ["Alex Reader"], "cover_cached": True, "retrieved_at": "2026-11-06T12:00:00Z"}},
    {"id": "two", "title": "A Field Guide to Quiet Places", "authors": ["Sam Rivers"], "format": "txt", "sections": 1, "chars": 1000, "position": None},
]
WS_STUB = """
window.__sockets=[];
class FakeWS { constructor(u){ this.url=u; this.readyState=0; this.sent=[]; __sockets.push(this); setTimeout(()=>{this.readyState=1; this.onopen&&this.onopen();},10);} send(d){this.sent.push(d);} close(){ this.readyState=3; this.onclose&&this.onclose(); } }
FakeWS.OPEN=1; window.WebSocket=FakeWS;
window.__push=(m)=>{ const s=__sockets.at(-1); s.onmessage({data:JSON.stringify(m)}); };
"""
NO_WEBGL2 = "const g=HTMLCanvasElement.prototype.getContext; HTMLCanvasElement.prototype.getContext=function(t,...a){ return t==='webgl2'?null:g.call(this,t,...a); };"
COVER = None


def route(r):
    path = "/" + r.request.url.split("://", 1)[1].split("/", 1)[1].split("?")[0]
    if path == "/icons.svg":
        return r.fulfill(body=ICONS.read_bytes(), content_type="image/svg+xml")
    if path.startswith("/api/"):
        if path == "/api/guardian/token": return r.fulfill(json={"token": "t" * 32})
        if path.endswith("/library"): return r.fulfill(json={"books": BOOKS})
        if path.endswith("/cover"): return r.fulfill(body=COVER, content_type="image/jpeg")
        if path.endswith("/position"): return r.fulfill(json={"position": POS})
        if "/sections/" in path: return r.fulfill(json={"index": 0, "blocks": [{"kind": "p", "text": "A quiet page. " * 80}]})
        if path.startswith("/api/reader/books/"):
            return r.fulfill(json={"book": BOOKS[0], "toc": [], "sections": [{"index": 0, "title": "One", "chars": 1000, "blocks": 1}]})
        return r.fulfill(status=404, json={})
    f = WEB / ALIASES.get(path, path.lstrip("/"))
    if not f.is_file(): return r.fulfill(status=404, body="")
    ct = {".css": "text/css", ".js": "text/javascript"}.get(f.suffix, "text/html")
    r.fulfill(body=f.read_bytes(), content_type=ct)


errors = []


def page(browser, url, *, width=1440, height=900, init=(), media=None, storage=None):
    ctx = browser.new_context(viewport={"width": width, "height": height}, reduced_motion=(media or {}).get("reduced_motion", "no-preference"),
                              forced_colors=(media or {}).get("forced_colors", "none"))
    pg = ctx.new_page()
    pg.on("pageerror", lambda e: errors.append(f"{url}: {e}"))
    if storage: pg.add_init_script(f"localStorage.setItem('sail.ambientVisuals', {storage!r});")
    for script in (WS_STUB, *init): pg.add_init_script(script)
    pg.route("**/*", route)
    pg.goto("https://sail.test" + url)
    return pg


with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox", "--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"])
    probe = b.new_page()
    COVER = base64.b64decode(probe.evaluate("""() => { const c=document.createElement('canvas');c.width=60;c.height=90;const x=c.getContext('2d');
      x.fillStyle='#1f6fd1';x.fillRect(0,0,60,45);x.fillStyle='#e8a317';x.fillRect(0,45,60,45);return c.toDataURL('image/jpeg').split(',')[1]; }"""))

    # Pure helpers.
    probe.add_script_tag(path=str(WEB / "ambient-backdrop.js"))
    grey = probe.evaluate("SailBackdrop.palette(['#566078','#334155','#0f172a'],'expressive')")
    assert all(abs(int(c[1:3], 16) - int(c[5:7], 16)) < 80 for c in grey), f"Offline must stay grey, got {grey}"
    vivid = probe.evaluate("SailBackdrop.paletteFromPixels([31,111,209,255, 232,163,23,255, 20,20,20,255, 200,200,200,255])")
    assert vivid[:2] and {vivid[0], vivid[1]} == {"#1f6fd1", "#e8a317"}, vivid
    assert probe.evaluate("SailBackdrop.visualsMode(null, q => q.includes('reduced-transparency'))") == "off"
    assert probe.evaluate("SailBackdrop.visualsMode(null, q => q.includes('pointer: coarse'))") == "subtle"
    assert probe.evaluate("SailBackdrop.visualsMode('expressive', q => q.includes('pointer: coarse'))") == "expressive"
    assert probe.evaluate("SailBackdrop.visualsMode('bogus', () => false)") == "expressive"

    # Home follows the ambient state contract.
    pg = page(b, "/")
    pg.wait_for_function("document.documentElement.dataset.sailState==='idle'")
    assert pg.evaluate("document.documentElement.classList.contains('sail-backdrop-on')")
    assert pg.evaluate("!!document.querySelector('.sail-backdrop > canvas')"), "shader canvas mounted"
    assert pg.evaluate("document.documentElement.dataset.sailVisuals") == "expressive"
    assert pg.evaluate("SailBackdrop.current.shader.speed") > 0
    lit = lambda: pg.evaluate("[0,1,2,3].map(i => SailBackdrop.current.layer.style.getPropertyValue('--sail-backdrop-'+i))")
    want = lambda s: pg.evaluate(f"SailBackdrop.palette(SailAmbient.STATES[{s!r}].light,'expressive').slice(1)")
    settle = lambda s: pg.wait_for_function(
        f"JSON.stringify([0,1,2,3].map(i => SailBackdrop.current.layer.style.getPropertyValue('--sail-backdrop-'+i)))"
        f" === JSON.stringify(SailBackdrop.palette(SailAmbient.STATES[{s!r}].light,'expressive').slice(1))", timeout=8000)
    settle("idle")
    pg.screenshot(path=str(SHOTS / "backdrop-home-idle.png"))
    # Cooldown: the first change shows at once; a burst right after it is held and
    # coalesced, so executing is never painted and speaking appears once.
    cooldown = pg.evaluate("SailBackdrop.COOLDOWN_MS")
    pg.wait_for_timeout(cooldown + 200)
    pg.evaluate("__push({type:'session',session_id:'s1',learning:true,control:{is_muted:false,max_recording_ms:180000}})")
    assert pg.evaluate("""() => {
        const ind = document.getElementById('learningIndicator');
        const conn = document.getElementById('connection');
        const indStyle = getComputedStyle(ind);
        const connStyle = getComputedStyle(conn);
        return !ind.hidden && indStyle.color === 'rgb(23, 19, 11)' && indStyle.backgroundImage.includes('gradient')
            && connStyle.color === 'rgb(23, 19, 11)' && connStyle.backgroundImage.includes('gradient')
            && indStyle.borderRadius === '999px' && connStyle.borderRadius === '999px';
    }"""), "fixed indicator buttons use live UI vibrant styling with dark ink on paper shader backdrop"
    pg.evaluate("__push({type:'turn_started',turn_id:1,epoch:1,transcript:'hi',state:'thinking'})")
    assert lit() == want("thinking"), "first change is immediate"
    pg.evaluate("__push({type:'tool_event',epoch:1,event:{tool:'fs_read',state:'executing'}})")
    pg.evaluate("__push({type:'tool_event',epoch:1,event:{tool:'fs_read',state:'completed'}})")
    pg.evaluate("__push({type:'response_finished',epoch:1})")
    assert pg.evaluate("document.documentElement.dataset.sailState") == "speaking"
    seen = []
    for _ in range(int(cooldown / 250) + 4):
        seen.append(lit()); pg.wait_for_timeout(250)
    assert want("executing") not in seen, "a coalesced intermediate state is never shown"
    assert seen[0] == want("thinking") and seen[-1] == want("speaking"), "holds, then shows the latest state"
    changes = sum(1 for a, b in zip(seen, seen[1:]) if a != b)
    assert changes == 1, f"one light change after the hold, saw {changes}"
    pg.screenshot(path=str(SHOTS / "backdrop-home-speaking.png"))
    pg.evaluate("__push({type:'turn_finished',epoch:1,state:'idle'})")
    settle("idle")
    assert pg.evaluate("document.documentElement.scrollWidth <= innerWidth")
    pg.context.close()

    # Owner setting off / accessibility media: nothing mounts, page unchanged.
    for kw in ({"storage": "off"}, {"media": {"forced_colors": "active"}}):
        pg = page(b, "/", **kw)
        pg.wait_for_function("document.documentElement.dataset.sailVisuals")
        assert pg.evaluate("document.documentElement.dataset.sailVisuals") == "off", kw
        assert pg.evaluate("!document.querySelector('.sail-backdrop') && !document.documentElement.classList.contains('sail-backdrop-on')"), kw
        pg.context.close()

    # Reduced motion: a still frame. Subtle setting is honoured.
    pg = page(b, "/", media={"reduced_motion": "reduce"}, storage="subtle")
    pg.wait_for_function("window.SailBackdrop && SailBackdrop.current")
    assert pg.evaluate("SailBackdrop.current.mode") == "subtle"
    assert pg.evaluate("SailBackdrop.current.shader.speed") == 0
    pg.context.close()

    # No WebGL2: CSS light stands in, no errors.
    pg = page(b, "/", init=(NO_WEBGL2,))
    pg.wait_for_function("window.SailBackdrop && SailBackdrop.current")
    assert pg.evaluate("document.querySelector('.sail-backdrop').classList.contains('sail-backdrop--css')")
    assert pg.evaluate("SailBackdrop.current.shader") is None
    pg.context.close()

    # Library: cover light, colour tiles, liquid-glass lens, off while reading.
    for width in (1440, 390):
        pg = page(b, "/reader", width=width, height=1000 if width > 400 else 844)
        pg.locator("#shelf .book").first.wait_for()
        pg.wait_for_function("document.querySelector('#continue .cover-image')?.style.opacity==='1'")
        pg.wait_for_timeout(900)
        tiles = pg.evaluate("[...document.querySelectorAll('#library-summary > div')].map(d => d.className)")
        assert tiles == ["reading-progress", "last-read-summary", "chapter-summary", "collection-summary"], tiles
        light = pg.evaluate("SailBackdrop.current.layer.style.getPropertyValue('--sail-backdrop-1')")
        assert light != pg.evaluate("SailBackdrop.palette(['#f2a65a','#d9467a','#2a1440'],'expressive')[2]"), "cover, not default, lights the Library"
        assert pg.evaluate("document.documentElement.scrollWidth <= innerWidth"), f"overflow {width}"
        if width > 400:
            # The Library uses no lens (it fills the canvas); the lens capability still
            # works for a floating control placed over the light.
            assert pg.evaluate("document.querySelectorAll('.sail-lens').length") == 0
            pg.evaluate("""() => { const d = document.createElement('div'); d.id = 'lensProbe';
              Object.assign(d.style, {position: 'fixed', right: '20px', bottom: '20px', width: '180px', height: '56px', borderRadius: '20px'});
              document.body.append(d); SailBackdrop.current.addLens(d); }""")
            pg.wait_for_function("document.querySelector('.sail-backdrop').classList.contains('sail-backdrop--glass')", timeout=15000)
            assert pg.evaluate("document.querySelectorAll('.sail-lens').length") == 1
            assert pg.evaluate("!!document.querySelector('.sail-lens canvas')"), "liquid glass rendered into the lens"
            pg.evaluate("document.getElementById('lensProbe').remove()")
        pg.screenshot(path=str(SHOTS / f"backdrop-library-{width}.png"), full_page=True)
        pg.get_by_role("button", name="Continue reading →", exact=True).click()
        pg.locator("#pages p").first.wait_for()
        assert pg.evaluate("SailBackdrop.current.layer.hidden && !document.documentElement.classList.contains('sail-backdrop-on')"), "no light behind book text"
        assert pg.evaluate("SailBackdrop.current.shader.speed") == 0
        pg.locator("#library-button").click(); pg.locator("#shelf .book").first.wait_for()
        assert pg.evaluate("!SailBackdrop.current.layer.hidden")
        pg.context.close()
    b.close()

assert not errors, errors
print("Ambient backdrop: contract-following Home light, off/forced-colours/reduced-motion/no-WebGL2 fallbacks, "
      "cover-lit Library with real-data tiles, liquid-glass lens, opaque reading at 1440 and 390px passed (fixture transport)")
