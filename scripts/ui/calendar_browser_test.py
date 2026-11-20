"""SAIL Calendar browser test (round 28, Antigravity).

Self-contained Playwright test: serves crates/sail-dialogue/web/ over an in-process
HTTP server, mocks /api/calendar* and /api/guardian/token, and drives /ws/voice
through a fake WebSocket from scripts/ui/fixtures/calendar/*.json.

    /home/main/SAIL/.venv-voice/bin/python scripts/ui/calendar_browser_test.py [shots_dir]
"""
import functools
import http.server
import json
import socketserver
import sys
import threading
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[2]
WEB_DIR = ROOT / "crates" / "sail-dialogue" / "web"
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "calendar"
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/r28-calendar-shots")
SHOTS.mkdir(parents=True, exist_ok=True)


def load_fixture(name):
    return json.loads((FIXTURES / name).read_text())


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass


def start_static_server():
    handler = functools.partial(QuietHandler, directory=str(WEB_DIR))
    httpd = socketserver.TCPServer(("127.0.0.1", 0), handler)
    port = httpd.server_address[1]
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    return httpd, port


FAKE_WS_INIT = """
(() => {
  localStorage.setItem('sail_owner_token', 'test-owner-token-0123456789abcdef0123456789abcdef');
  const sockets = [];
  window.__sailFakeSockets = sockets;
  window.__sailPushWs = (payload) => {
    const raw = typeof payload === 'string' ? payload : JSON.stringify(payload);
    for (const s of sockets) {
      if (s.readyState === 1 && typeof s.onmessage === 'function') {
        s.onmessage({ data: raw });
      }
    }
  };
  window.WebSocket = function(url) {
    this.url = url;
    this.readyState = 0;
    this.send = () => {};
    this.close = () => { this.readyState = 3; };
    sockets.push(this);
    setTimeout(() => {
      this.readyState = 1;
      if (typeof this.onopen === 'function') this.onopen({});
    }, 20);
  };
})();
"""


def assert_no_horizontal_overflow(page, label):
    overflow = page.evaluate("() => document.documentElement.scrollWidth > window.innerWidth")
    assert not overflow, f"Horizontal overflow detected at {label}"


def run_viewport(browser, base_url, width, height):
    context = browser.new_context(viewport={"width": width, "height": height})
    context.add_init_script(FAKE_WS_INIT)
    page = context.new_page()

    page_errors = []
    console_errors = []
    page.on("pageerror", lambda err: page_errors.append(str(err)))
    page.on("console", lambda msg: console_errors.append(msg.text)
            if msg.type == "error" and "422 (Unprocessable Entity)" not in msg.text else None)

    state = {
        "sources": load_fixture("sources_first_run.json"),
        "window": load_fixture("week_empty.json"),
        "prefs": load_fixture("preferences.json"),
        "event_detail": load_fixture("event_local.json"),
        "created": [],
        "patched": [],
    }

    VENDOR_MAP = {
        "/vendor/xterm.js": (WEB_DIR / "vendor/xterm/xterm.js", "text/javascript"),
        "/vendor/addon-fit.js": (WEB_DIR / "vendor/xterm/addon-fit.js", "text/javascript"),
        "/vendor/xterm.css": (WEB_DIR / "vendor/xterm/xterm.css", "text/css"),
        "/vendor/paper-shaders.js": (WEB_DIR / "vendor/paper-shaders/paper-shaders.js", "text/javascript"),
        "/vendor/liquidglass.js": (WEB_DIR / "vendor/liquidglass/liquidglass.js", "text/javascript"),
    }

    def handle_route(route):
        url = route.request.url
        method = route.request.method
        for prefix, (fpath, ctype) in VENDOR_MAP.items():
            if url.endswith(prefix):
                route.fulfill(status=200, content_type=ctype, body=fpath.read_bytes() if fpath.exists() else b"")
                return
        if url.endswith("/favicon.ico"):
            route.fulfill(status=204, body=b"")
            return
        if url.endswith("/mermaid.js"):
            route.fulfill(status=200, content_type="text/javascript", body=b"")
            return
        if url.endswith("/icons.svg"):
            route.fulfill(status=200, content_type="image/svg+xml", body=b'<svg xmlns="http://www.w3.org/2000/svg"></svg>')
            return
        if url.endswith("/voice-assets/manifest.json"):
            route.fulfill(status=200, content_type="application/json", body=b"{}")
            return

        if "/api/calendar/preferences" in url:
            route.fulfill(status=200, content_type="application/json", body=json.dumps(state["prefs"]))
            return
        if "/api/calendar/sources" in url and method == "GET":
            route.fulfill(status=200, content_type="application/json", body=json.dumps(state["sources"]))
            return
        if "/api/calendar/sources" in url and method == "POST":
            route.fulfill(status=422, content_type="application/json", body=json.dumps(load_fixture("error_blocked_address.json")))
            return
        if "/api/calendar/events?" in url and method == "GET":
            route.fulfill(status=200, content_type="application/json", body=json.dumps(state["window"]))
            return
        if "/api/calendar/events/" in url and method == "GET":
            route.fulfill(status=200, content_type="application/json", body=json.dumps(state["event_detail"]))
            return
        if "/api/calendar/events/" in url and method == "PATCH":
            body = json.loads(route.request.post_data or "{}")
            state["patched"].append(body)
            ev = dict(state["event_detail"])
            if "prepare" in body:
                ev["prepare"] = body["prepare"]
            route.fulfill(status=200, content_type="application/json", body=json.dumps(ev))
            return
        if "/api/calendar/events" in url and method == "POST":
            body = json.loads(route.request.post_data or "{}")
            state["created"].append(body)
            route.fulfill(status=201, content_type="application/json", body=json.dumps(load_fixture("event_local.json")))
            return
        if "/api/calendar/import/preview" in url and method == "POST":
            route.fulfill(status=200, content_type="application/json", body=json.dumps(load_fixture("import_preview.json")))
            return
        if "/api/calendar/import/commit" in url and method == "POST":
            route.fulfill(status=200, content_type="application/json", body=json.dumps(load_fixture("import_result.json")))
            return
        if "/api/guardian/token" in url:
            route.fulfill(status=200, content_type="application/json",
                          body=json.dumps({"token": "test-owner-token-0123456789abcdef0123456789abcdef"}))
            return
        if "/api/" in url:
            route.fulfill(status=200, content_type="application/json", body="{}")
            return
        route.continue_()

    page.route("**/*", handle_route)
    page.goto(base_url, wait_until="domcontentloaded")
    expect(page.locator("#calendarNavBtn")).to_be_attached()

    page.click("#calendarNavBtn")
    cal = page.locator("#calendarPage")
    expect(cal).to_be_visible()

    # Empty state in Agenda view
    cal.get_by_role("button", name="Agenda").click()
    expect(cal).to_contain_text("No events this week.")
    assert_no_horizontal_overflow(page, f"{width}x{height} empty")

    # Mixed sources & week view + XSS check
    state["sources"] = load_fixture("sources_mixed.json")
    mixed = load_fixture("week_mixed.json")
    mixed["occurrences"][0]["title"] = '<img src=x onerror="window.__xss=1"> Algorithms lecture'
    state["window"] = mixed
    page.evaluate("() => window.SailCalendar.refreshAll()")
    cal.get_by_role("button", name="Week").click()
    cal.get_by_role("button", name="Next period").click()
    expect(cal).to_contain_text("Algorithms lecture")
    expect(cal).to_contain_text("Sync failed (timed out after 20 s)")
    assert page.evaluate("() => window.__xss === undefined"), "XSS executed!"
    assert_no_horizontal_overflow(page, f"{width}x{height} week_mixed")

    # Truncated banner
    state["window"] = load_fixture("window_truncated.json")
    page.evaluate("() => window.SailCalendar.refreshWindow()")
    expect(cal).to_contain_text("Showing the first 5,000 events")

    # Month dense + "+4 more" -> Day view
    state["window"] = load_fixture("month_dense.json")
    cal.get_by_role("button", name="Month").click()
    more_btn = cal.locator(".cal-more-btn").first
    expect(more_btn).to_be_visible()
    more_btn.click()
    expect(cal.locator(".cal-week-grid")).to_be_visible()

    # Detail sheet + prepare toggle on read-only feed event
    state["event_detail"] = load_fixture("event_feed_readonly.json")
    cal.locator(".cal-event-pill").first.click()
    expect(cal.locator(".cal-modal")).to_contain_text("Read-only feed")
    cal.get_by_role("button", name="Needs preparation: Off").click()
    assert any(p.get("prepare") is True for p in state["patched"])
    page.keyboard.press("Escape")
    expect(cal.locator(".cal-modal")).to_be_hidden()

    # Keyboard shortcuts: n -> Add event, ArrowRight/Left, t -> Today
    page.keyboard.press("n")
    expect(cal.locator(".cal-modal")).to_contain_text("Add event")
    cal.locator(".cal-modal input.cal-input").first.fill("Quick Dentist")
    cal.get_by_role("button", name="Save event").click()
    expect(cal.locator(".cal-modal")).to_be_hidden()
    assert len(state["created"]) == 1
    page.keyboard.press("ArrowRight")
    page.keyboard.press("ArrowLeft")
    page.keyboard.press("t")

    # Sources panel: blocked address error + ws calendar_changed
    cal.get_by_role("button", name="Sources").click()
    expect(cal.locator(".cal-sources-panel")).to_be_visible()
    cal.locator(".cal-sources-panel input.cal-input").nth(0).fill("Private")
    cal.locator(".cal-sources-panel input.cal-input").nth(1).fill("https://192.168.1.5/cal.ics")
    cal.get_by_role("button", name="Add a calendar feed").click()
    expect(cal).to_contain_text("that address points inside a private network")

    page.evaluate("(p) => window.__sailPushWs(p)", load_fixture("ws_calendar_changed.json"))
    assert_no_horizontal_overflow(page, f"{width}x{height} final")

    if width in (1440, 390):
        page.screenshot(path=str(SHOTS / f"calendar_{width}.png"))

    assert not page_errors, f"Page errors at {width}x{height}: {page_errors}"
    assert not console_errors, f"Console errors at {width}x{height}: {console_errors}"
    context.close()


def main():
    httpd, port = start_static_server()
    base_url = f"http://127.0.0.1:{port}/index.html"
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch()
            for w, h in [(1440, 900), (768, 1024), (390, 844), (360, 800)]:
                run_viewport(browser, base_url, w, h)
                print(f"PASS calendar_browser_test {w}x{h}")
            browser.close()
    finally:
        httpd.shutdown()


if __name__ == "__main__":
    main()
