"""SAIL Presence & Announce browser test (round 26, Antigravity).

Self-contained Playwright test: serves crates/sail-dialogue/web/ over an in-process
HTTP server, mocks /api/presence* and /api/proactive/items/*/feedback, and drives
/ws/voice through a fake WebSocket from scripts/ui/fixtures/presence/*.json.

    /home/main/SAIL/.venv-voice/bin/python scripts/ui/presence_browser_test.py [shots_dir]
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
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "presence"
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/r26-presence-shots")
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
  localStorage.setItem('sail_presence_hands_free', 'false');

  const sockets = [];
  window.__sailFakeSockets = sockets;
  window.__sailSentWsMessages = [];
  window.__sailPushWs = (payload) => {
    const raw = typeof payload === 'string' ? payload : JSON.stringify(payload);
    for (const s of sockets) {
      if (s.readyState === 1 && typeof s.onmessage === 'function') {
        s.onmessage({ data: raw });
      }
    }
  };

  const RealWebSocket = window.WebSocket;
  window.WebSocket = function(url, protocols) {
    if (String(url).includes('/ws/terminal')) {
      return {
        url,
        readyState: 0,
        bufferedAmount: 0,
        onopen: null,
        onclose: null,
        onerror: null,
        onmessage: null,
        send() {},
        close() { this.readyState = 3; },
      };
    }
    if (String(url).includes('/ws/voice')) {
      const sock = {
        url,
        readyState: 0,
        bufferedAmount: 0,
        onopen: null,
        onclose: null,
        onerror: null,
        onmessage: null,
        send(data) {
          window.__sailSentWsMessages.push(data);
        },
        close() {
          this.readyState = 3;
          if (typeof this.onclose === 'function') this.onclose({});
        },
      };
      sockets.push(sock);
      setTimeout(() => {
        sock.readyState = 1;
        if (typeof sock.onopen === 'function') sock.onopen({});
      }, 25);
      return sock;
    }
    return protocols ? new RealWebSocket(url, protocols) : new RealWebSocket(url);
  };
  window.WebSocket.OPEN = 1;
  window.WebSocket.CLOSED = 3;
})();
"""


def route_static_aliases(page):
    page.route("**/favicon.ico", lambda r: r.fulfill(status=204, body=""))
    page.route("**/voice-assets/**", lambda r: r.fulfill(json={}))
    page.route(
        "**/icons.svg",
        lambda r: r.fulfill(
            body=(ROOT / "assets/source/icons/sail-icons.svg").read_bytes(),
            content_type="image/svg+xml",
        ),
    )
    aliases = {
        "/vendor/xterm.js": (WEB_DIR / "vendor/xterm/xterm.js", "text/javascript"),
        "/vendor/addon-fit.js": (WEB_DIR / "vendor/xterm/addon-fit.js", "text/javascript"),
        "/vendor/xterm.css": (WEB_DIR / "vendor/xterm/xterm.css", "text/css"),
        "/vendor/paper-shaders.js": (WEB_DIR / "vendor/paper-shaders/paper-shaders.js", "text/javascript"),
        "/vendor/liquidglass.js": (WEB_DIR / "vendor/liquidglass/liquidglass.js", "text/javascript"),
    }
    for url_path, (fp, ctype) in aliases.items():
        if fp.exists():
            page.route(
                f"**{url_path}",
                (lambda p, ct: lambda r: r.fulfill(body=p.read_bytes(), content_type=ct))(fp, ctype),
            )
        else:
            page.route(
                f"**{url_path}",
                (lambda ct: lambda r: r.fulfill(body="", content_type=ct))(ctype),
            )


def install_api_mocks(page, state_fixture, settings_store, feedback_calls):
    # Register catch-all first so specific routes registered after it take precedence.
    page.route("**/api/**", lambda r: r.fulfill(json={}))
    page.route("**/api/guardian/token", lambda r: r.fulfill(json={"token": "test-owner-token-0123456789abcdef0123456789abcdef"}))
    page.route("**/api/presence/heartbeat", lambda r: r.fulfill(json={"state": state_fixture}))

    def handle_settings(route):
        if route.request.method == "POST":
            body = json.loads(route.request.post_data or "{}")
            clamped = {
                "away_after_secs": int(body.get("away_after_secs", 600)),
                "greet_on_arrival": bool(body.get("greet_on_arrival", True)),
                "greet_after_away_secs": int(body.get("greet_after_away_secs", 2700)),
                "speak_announcements": bool(body.get("speak_announcements", True)),
                "speak_on": list(body.get("speak_on", ["web", "dock"])),
                "max_spoken_per_hour": max(0, min(6, int(body.get("max_spoken_per_hour", 3)))),
                "telegram_when_away": bool(body.get("telegram_when_away", False)),
            }
            settings_store.clear()
            settings_store.update(clamped)
            route.fulfill(json=clamped)
        else:
            route.fulfill(json=settings_store)

    page.route("**/api/presence/settings", handle_settings)
    page.route("**/api/presence", lambda r: r.fulfill(json=state_fixture))

    def handle_feedback(route):
        feedback_calls.append({
            "url": route.request.url,
            "body": json.loads(route.request.post_data or "{}"),
        })
        route.fulfill(json={"ok": True})

    page.route("**/api/proactive/items/*/feedback", handle_feedback)


def run():
    httpd, port = start_static_server()
    base_url = f"http://127.0.0.1:{port}"
    announce_speak = load_fixture("announce_speak.json")
    announce_show = load_fixture("announce_show.json")
    greeting = load_fixture("presence_greeting.json")
    hf_listening = load_fixture("hands_free_listening.json")
    hf_follow_up = load_fixture("hands_free_follow_up.json")
    hf_off = load_fixture("hands_free_off.json")
    not_addressed = load_fixture("not_addressed.json")
    presence_state = load_fixture("presence_state.json")
    default_settings = load_fixture("presence_settings.json")

    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(args=["--no-sandbox"])
            for width in (1440, 768, 390, 360):
                page = browser.new_page(viewport={"width": width, "height": 900}, reduced_motion="reduce")
                page_errors = []
                console_errors = []
                page.on("pageerror", lambda e: page_errors.append(str(e)))
                page.on("console", lambda msg: console_errors.append(msg.text) if msg.type == "error" else None)

                page.add_init_script(FAKE_WS_INIT)
                route_static_aliases(page)
                settings_store = dict(default_settings)
                feedback_calls = []
                install_api_mocks(page, presence_state, settings_store, feedback_calls)

                page.goto(base_url + "/")
                page.wait_for_function("() => window.__sailFakeSockets.length > 0 && window.__sailFakeSockets[0].readyState === 1")

                # 1. Verify surface hello was sent on ws open
                sent = page.evaluate("() => window.__sailSentWsMessages.map(m => typeof m === 'string' ? JSON.parse(m) : null).filter(Boolean)")
                surface_msgs = [m for m in sent if m.get("type") == "surface"]
                assert len(surface_msgs) >= 1 and surface_msgs[0]["surface"] == "web", surface_msgs

                # 2. Push announce (speak=true and speak=false) + greeting
                page.evaluate("(m) => window.__sailPushWs(m)", announce_speak)
                page.evaluate("(m) => window.__sailPushWs(m)", announce_show)
                page.evaluate("(m) => window.__sailPushWs(m)", greeting)

                stack = page.locator("#announceStack")
                expect(stack).to_be_visible()
                expect(stack.locator(".announce-card")).to_have_count(2)
                expect(stack.locator(".announce-spoken-badge")).to_have_count(1)
                expect(stack.locator(".announce-btn-open")).to_have_count(1)
                expect(page.locator("#presenceGreeting")).to_be_visible()
                expect(page.locator("#presenceGreeting")).to_contain_text("Welcome back.")

                # 3. Why? expands reasons
                first_card = stack.locator('.announce-card[data-item-id="a1b2"]')
                first_card.locator(".announce-why-btn").click()
                expect(first_card.locator(".announce-reasons")).to_be_visible()
                expect(first_card.locator(".announce-reasons")).to_contain_text("urgency 0.70")

                # 4. Feedback removes card & posts exact payload
                useful_btn = first_card.get_by_role("button", name="Useful")
                useful_btn.evaluate("el => el.scrollIntoView({block: 'center'})")
                useful_btn.click()
                expect(stack.locator(".announce-card")).to_have_count(1)
                assert any(c["body"].get("feedback") == "useful" for c in feedback_calls), feedback_calls

                # 5. Hands-free states + not_addressed leaves no text in DOM
                page.locator("#handsFreeBtn").click()
                page.evaluate("(m) => window.__sailPushWs(m)", hf_listening)
                expect(page.locator("#handsFreeStatePill")).to_be_visible()
                expect(page.locator("#handsFreeStateText")).to_have_text('Listening for "SAIL"')

                page.evaluate("(m) => window.__sailPushWs(m)", hf_follow_up)
                expect(page.locator("#handsFreeStateText")).to_have_text("Go ahead")

                snap_expr = "() => ['#announceStack','#presenceGreeting','#conversation','#liveTranscript','#transcript','#handsFreeStatePill'].map(s => document.querySelector(s)?.innerText || '')"
                snap_before = page.evaluate(snap_expr)
                page.evaluate("(m) => window.__sailPushWs(m)", not_addressed)
                snap_after = page.evaluate(snap_expr)
                assert snap_before == snap_after, f"not_addressed must not add any text to DOM: {snap_before} != {snap_after}"

                page.evaluate("(m) => window.__sailPushWs(m)", hf_off)
                expect(page.locator("#handsFreeStatePill")).to_be_hidden()

                # 6. Settings -> Presence round trip with clamped echo
                page.locator("#settingsOpen").evaluate("el => { el.hidden = false; el.click(); }")
                with page.expect_response("**/api/presence/settings"):
                    page.locator('[data-settings-tab="presence"]').click()
                expect(page.locator("#presencePlaceValue")).to_have_text("You're at the dock")
                expect(page.locator("#presenceMaxSpoken")).to_have_value("3")
                page.locator("#presenceMaxSpoken").fill("9")
                page.locator("#presenceSettingsSave").click()
                expect(page.locator("#presenceSettingsStatus")).to_have_text("Presence preferences saved.")
                expect(page.locator("#presenceMaxSpoken")).to_have_value("6")
                page.locator(".vscode-close-btn").click()

                # 7. Overflow + zero errors
                overflow = page.evaluate("() => document.documentElement.scrollWidth > window.innerWidth")
                assert not overflow, f"Horizontal overflow at {width}px"
                assert not page_errors, f"Page errors at {width}px: {page_errors}"
                assert not console_errors, f"Console errors at {width}px: {console_errors}"

                page.screenshot(path=str(SHOTS / f"presence-{width}.png"), full_page=True)
                print(f"PASS {width}px: announce, Why?, feedback, greeting, hands_free, not_addressed, settings clamp, 0 errors, no overflow")
                page.close()
            browser.close()
    finally:
        httpd.shutdown()


if __name__ == "__main__":
    run()
