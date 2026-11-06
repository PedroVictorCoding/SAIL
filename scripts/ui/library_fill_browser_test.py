"""Library fills the whole canvas, standalone and embedded in SAIL's Entertainment pane.

While the Library is open the toolbar (only a Library button there) is hidden and the
status line floats, so no rows are spent above the Library. Standalone: 12px margin
and a home link. Embedded in an iframe: edge to edge, no frame or home link. Opening
a book brings the reader toolbar back. 1440 and 390px, zero page errors.

Run with: .venv-voice/bin/python scripts/ui/library_fill_browser_test.py [shot-dir]
"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / "crates/sail-dialogue/web"
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp")
ALIASES = {"/reader": "reader.html", "/vendor/paper-shaders.js": "vendor/paper-shaders/paper-shaders.js",
           "/vendor/liquidglass.js": "vendor/liquidglass/liquidglass.js"}
POS = {"percent": 46.8, "revision": 1, "anchor": {"section": 0, "block": 0, "char": 0}, "saved_at": "2026-11-06T11:40:00Z"}
BOOKS = [{"id": "one", "title": "The Shape of Attention", "authors": ["Alex Reader"], "format": "epub", "sections": 12, "chars": 1000, "position": POS},
         {"id": "two", "title": "A Field Guide to Quiet Places", "authors": ["Sam Rivers"], "format": "txt", "sections": 1, "chars": 1000, "position": None}]
HOST = b"<!doctype html><body style='margin:0;background:#000'><iframe id=f src='/reader' style='position:fixed;inset:40px 60px 30px 220px;width:calc(100% - 280px);height:calc(100% - 70px);border:0;border-radius:24px'></iframe>"


def route(r):
    path = "/" + r.request.url.split("://", 1)[1].split("/", 1)[1].split("?")[0]
    if path == "/host": return r.fulfill(body=HOST, content_type="text/html")
    if path.startswith("/api/"):
        if path == "/api/guardian/token": return r.fulfill(json={"token": "t" * 32})
        if path.endswith("/library"): return r.fulfill(json={"books": BOOKS})
        if path.endswith("/position"): return r.fulfill(json={"position": POS})
        if "/sections/" in path: return r.fulfill(json={"index": 0, "blocks": [{"kind": "p", "text": "A quiet page. " * 80}]})
        if path.startswith("/api/reader/books/"):
            return r.fulfill(json={"book": BOOKS[0], "toc": [], "sections": [{"index": 0, "title": "One", "chars": 1000, "blocks": 1}]})
        return r.fulfill(status=404, json={})
    f = WEB / ALIASES.get(path, path.lstrip("/"))
    if not f.is_file(): return r.fulfill(status=404, body="")
    r.fulfill(body=f.read_bytes(), content_type={".css": "text/css", ".js": "text/javascript"}.get(f.suffix, "text/html"))


GEOM = """() => { const l = document.getElementById('library').getBoundingClientRect(); const t = document.querySelector('.toolbar');
  return {top: l.top, left: l.left, right: innerWidth - l.right, width: l.width, vw: innerWidth, toolbar: getComputedStyle(t).display,
          home: getComputedStyle(document.querySelector('.library-home')).display, embedded: document.documentElement.dataset.embedded || '',
          radius: getComputedStyle(document.getElementById('library')).borderTopLeftRadius, statusH: document.getElementById('status').getBoundingClientRect().height,
          overflow: document.documentElement.scrollWidth > innerWidth}; }"""
errors = []
with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox", "--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"])
    for width, height in ((1440, 900), (390, 844)):
        ctx = b.new_context(viewport={"width": width, "height": height}); pg = ctx.new_page()
        pg.on("pageerror", lambda e: errors.append(str(e))); pg.route("**/*", route)
        pg.goto("https://sail.test/reader"); pg.locator("#shelf .book").first.wait_for()
        g = pg.evaluate(GEOM)
        margin = 12 if width > 700 else 8
        assert g["toolbar"] == "none" and g["embedded"] == "" and g["home"] != "none", g
        assert abs(g["top"] - margin) <= 1 and abs(g["left"] - margin) <= 1 and abs(g["right"] - margin) <= 1, ("fills the window", g)
        assert not g["overflow"], g
        pg.screenshot(path=str(SHOTS / f"library-fill-{width}.png"))
        # The reader keeps its toolbar.
        pg.get_by_role("button", name="Continue reading →", exact=True).click(); pg.locator("#pages p").first.wait_for()
        assert pg.evaluate("getComputedStyle(document.querySelector('.toolbar')).display") != "none"
        pg.locator("#library-button").click(); pg.locator("#shelf .book").first.wait_for()
        assert pg.evaluate("getComputedStyle(document.querySelector('.toolbar')).display") == "none"
        ctx.close()

    ctx = b.new_context(viewport={"width": 1440, "height": 900}); pg = ctx.new_page()
    pg.on("pageerror", lambda e: errors.append(str(e))); pg.route("**/*", route)
    pg.goto("https://sail.test/host")
    fr = pg.frame_locator("#f"); fr.locator("#shelf .book").first.wait_for()
    frame = pg.frame(url="https://sail.test/reader")
    g = frame.evaluate(GEOM)
    assert g["embedded"] == "true" and g["toolbar"] == "none" and g["home"] == "none", g
    assert g["top"] == 0 and g["left"] == 0 and g["right"] == 0 and g["radius"] == "0px", ("edge to edge in the pane", g)
    assert not g["overflow"], g
    pg.screenshot(path=str(SHOTS / "library-fill-embedded.png"))
    ctx.close(); b.close()

assert not errors, errors
print("Library fill: no toolbar row in the Library, floating status, standalone 12/8px margin with home link, "
      "embedded edge to edge without frame or home link, reader toolbar intact at 1440 and 390px passed")
