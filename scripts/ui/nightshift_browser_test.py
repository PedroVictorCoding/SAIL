"""SAIL Night Shift browser test (round 27, Antigravity).

Self-contained Playwright test: serves crates/sail-dialogue/web/ over an in-process
HTTP server, mocks /api/nightshift* and /api/guardian/token, and drives /ws/voice
through a fake WebSocket from scripts/ui/fixtures/nightshift/*.json.

    /home/main/SAIL/.venv-voice/bin/python scripts/ui/nightshift_browser_test.py [shots_dir]
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
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "nightshift"
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/r27-nightshift-shots")
SHOTS.mkdir(parents=True, exist_ok=True)


def load_fixture(name):
    return json.loads((FIXTURES / name).read_text())


def make_long_diff():
    lines_300 = ["diff --git a/crates/sail-dialogue/web/capital.js b/crates/sail-dialogue/web/capital.js",
                 "index 1111111..2222222 100644",
                 "--- a/crates/sail-dialogue/web/capital.js",
                 "+++ b/crates/sail-dialogue/web/capital.js",
                 "@@ -1,4 +1,300 @@"]
    for i in range(1, 296):
        prefix = "+" if i % 3 == 0 else ("-" if i % 7 == 0 else " ")
        lines_300.append(f"{prefix}const row_{i} = formatDividendDate('2026-12-01', 'very_long_identifier_for_mobile_overflow_check_{i}');")

    lines_420 = ["diff --git a/crates/sail-dialogue/web/capital.css b/crates/sail-dialogue/web/capital.css",
                 "index 3333333..4444444 100644",
                 "--- a/crates/sail-dialogue/web/capital.css",
                 "+++ b/crates/sail-dialogue/web/capital.css",
                 "@@ -1,4 +1,420 @@"]
    for i in range(1, 416):
        lines_420.append(f"+.col-{i} {{ min-width: 0; }}")

    return "\n".join(lines_300 + lines_420)


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
    page.on("console", lambda msg: console_errors.append(msg.text) if msg.type == "error" else None)

    current_payload = {"data": load_fixture("empty.json")}
    saved_settings = []
    approved_orders = []
    diff_text = make_long_diff()

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
                body = fpath.read_bytes() if fpath.exists() else b""
                route.fulfill(status=200, content_type=ctype, body=body)
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
        if "/api/nightshift/settings" in url and method == "PUT":
            body = json.loads(route.request.post_data or "{}")
            saved_settings.append(body)
            route.fulfill(status=200, content_type="application/json", body=json.dumps(body))
            return
        if "/api/nightshift/errands/" in url and url.endswith("/approve") and method == "POST":
            body = json.loads(route.request.post_data or "{}")
            approved_orders.append(body)
            queued = load_fixture("queued_with_plan.json")["errands"][0]
            route.fulfill(status=200, content_type="application/json", body=json.dumps(queued))
            return
        if "/api/nightshift/errands/" in url and url.endswith("/diff"):
            route.fulfill(status=200, content_type="text/x-diff; charset=utf-8", body=diff_text)
            return
        if url.endswith("/api/nightshift") and method == "GET":
            route.fulfill(status=200, content_type="application/json", body=json.dumps(current_payload["data"]))
            return
        if "/api/guardian/token" in url:
            route.fulfill(status=200, content_type="application/json",
                          body=json.dumps({"token": "test-owner-token-0123456789abcdef0123456789abcdef"}))
            return
        if "/api/presence" in url:
            route.fulfill(status=200, content_type="application/json", body=json.dumps({"settings": {}}))
            return
        if "/api/" in url:
            route.fulfill(status=200, content_type="application/json", body="{}")
            return
        route.continue_()

    page.route("**/*", handle_route)
    page.goto(base_url, wait_until="domcontentloaded")
    expect(page.locator("#nightshiftNavBtn")).to_be_attached()

    # Open Night Shift page
    page.click("#nightshiftNavBtn")
    ns_page = page.locator("#nightshiftPage")
    expect(ns_page).to_be_visible()
    expect(ns_page).to_contain_text("No Night Shift errands yet")
    assert_no_horizontal_overflow(page, f"{width}x{height} empty")

    # Test all fixtures sequentially
    for fname in ["shaping.json", "unshapeable.json", "queued_with_plan.json", "waiting_for_heavy.json",
                  "working.json", "shipped.json", "external_used.json", "mesh_resting.json"]:
        current_payload["data"] = load_fixture(fname)
        page.evaluate("() => window.SailNightShift.refresh()")
        page.wait_for_timeout(80)
        assert_no_horizontal_overflow(page, f"{width}x{height} {fname}")

    # Check waiting_for_heavy errand card callout, pooled_morning, waited_all_night, external_used & mesh_resting
    current_payload["data"] = load_fixture("waiting_for_heavy.json")
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page).to_contain_text("Waiting for the PC to lend")
    assert_no_horizontal_overflow(page, f"{width}x{height} waiting_for_heavy_card")

    current_payload["data"] = load_fixture("pooled_morning.json")
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page.locator(".ns-morning-spoken")).to_contain_text("Ran on the big model (host + windows)")
    expect(ns_page.locator("article.ns-card").last).to_contain_text("Ran on the big model (host + windows)")
    assert_no_horizontal_overflow(page, f"{width}x{height} pooled_morning")

    current_payload["data"] = load_fixture("waited_all_night.json")
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page.locator(".ns-morning-spoken")).to_contain_text("Waited — the PC wasn't lending")
    expect(ns_page.locator("article.ns-card").last).to_contain_text("Waited — the PC wasn't lending")
    assert_no_horizontal_overflow(page, f"{width}x{height} waited_all_night")

    current_payload["data"] = load_fixture("external_used.json")
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page).to_contain_text("Sent to Claude Code")

    current_payload["data"] = load_fixture("mesh_resting.json")
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page).to_contain_text("PC (windows) resting")

    # Test proposed editable work order + approve
    current_payload["data"] = load_fixture("proposed.json")
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page).to_contain_text("SAIL's draft work order")
    title_inp = ns_page.locator("input.ns-input").first
    title_inp.fill("Updated ex-dividend column")
    ns_page.get_by_role("button", name="Approve for tonight").click()
    page.wait_for_timeout(80)
    assert len(approved_orders) == 1 and approved_orders[0]["order"]["title"] == "Updated ex-dividend column"

    # Test morning_mixed + diff viewer (300-line open, >400-line collapsed, XSS safe)
    mixed = load_fixture("morning_mixed.json")
    mixed["errands"][0]["ask"] = '<img src=x onerror="window.__xss=1">'
    current_payload["data"] = mixed
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page).to_contain_text("Overnight I finished two of four jobs")
    expect(ns_page).to_contain_text("Model-written note")
    assert page.evaluate("() => window.__xss === undefined"), "XSS executed from server text!"

    ns_page.get_by_role("button", name="View diff").first.click()
    expect(ns_page.locator(".ns-diff-viewer")).to_be_visible()
    headers = ns_page.locator(".ns-diff-file-hdr")
    assert headers.count() == 2
    assert headers.nth(0).get_attribute("aria-expanded") == "true"
    assert headers.nth(1).get_attribute("aria-expanded") == "false"
    assert_no_horizontal_overflow(page, f"{width}x{height} morning_mixed_diff")

    if width in (1440, 390):
        page.screenshot(path=str(SHOTS / f"nightshift_{width}.png"))

    # Test WebSocket push updates
    ws_errand = load_fixture("ws_errand.json")
    page.evaluate("(p) => window.__sailPushWs(p)", ws_errand)
    expect(ns_page).to_contain_text("Working · reviewing")

    ws_morning = load_fixture("ws_morning.json")
    page.evaluate("(p) => window.__sailPushWs(p)", ws_morning)
    expect(ns_page).to_contain_text("Overnight I finished both jobs")

    # Test R8 morning_issue_and_halt fixture (halted sentence + GitHub issue PR link vs plain line)
    current_payload["data"] = load_fixture("morning_issue_and_halt.json")
    page.evaluate("() => window.SailNightShift.refresh()")
    expect(ns_page.locator(".ns-morning-halted")).to_contain_text(
        "The night stopped early: the model server stopped responding at 03:12."
    )
    pr_links = ns_page.locator("a.ns-morning-pr-link")
    assert pr_links.count() == 1, f"Expected 1 PR link, got {pr_links.count()}"
    assert pr_links.first.get_attribute("href") == "https://github.com/PedroVictorCoding/SAIL/pull/501"
    expect(ns_page).to_contain_text("Tests failed on host.")
    assert_no_horizontal_overflow(page, f"{width}x{height} morning_issue_and_halt")

    # Test Settings -> Night Shift
    page.locator("#nightshiftPage").get_by_role("button", name="Settings").click()
    ns_settings = page.locator("#settingsPanel-nightshift")
    expect(ns_settings).to_be_visible()
    expect(ns_settings.get_by_text("Include forge-ready GitHub issues")).to_have_count(0)
    expect(ns_settings).to_contain_text("Use the big model when the PC is lending")
    expect(ns_settings).to_contain_text("If the PC isn't lending, use Qwen instead")
    ns_settings.locator("#nsSetEnabled").check()
    ns_settings.locator("#nsSetUseHeavy").uncheck()
    ns_settings.locator("#nsSetFallbackToHost").check()
    ns_settings.locator("#nsSetSaveBtn").click()
    page.wait_for_timeout(80)
    assert len(saved_settings) == 1
    assert saved_settings[0]["include_forge_ready_issues"] is False
    assert saved_settings[0]["enabled"] is True
    assert saved_settings[0]["use_heavy"] is False
    assert saved_settings[0]["fallback_to_host"] is True

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
                print(f"PASS nightshift_browser_test {w}x{h}")
            browser.close()
    finally:
        httpd.shutdown()


if __name__ == "__main__":
    main()
