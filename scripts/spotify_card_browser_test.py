"""Verifies the Spotify Now Playing card in the Tools Map sidebar in a headless browser.

The Spotify API is mocked with Playwright routes; this checks the interface only: the
Set up -> Connect -> Now Playing states, the setup dialog, that changes send the owner
token, and that playback controls keep keyboard focus.

Run with: .venv-voice/bin/python scripts/spotify_card_browser_test.py [screenshot.png]
"""
import json
import functools
import http.server
import os
import sys
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
WEB = ROOT / "crates" / "sail-dialogue" / "web"
ICONS = ROOT / "assets" / "source" / "icons" / "sail-icons.svg"
SHOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/spotify-card.png")


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith("/vendor/") and "/xterm/" not in self.path:
            self.path = "/vendor/xterm/" + self.path.split("/")[-1]
        if self.path == "/tracker":
            self.path = "/tracker.html"
        if self.path == "/icons.svg":
            body = ICONS.read_bytes()
            self.send_response(200)
            self.send_header("content-type", "image/svg+xml")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def log_message(self, *args):
        pass


server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 0), functools.partial(Handler, directory=str(WEB))
)
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f"http://127.0.0.1:{server.server_port}/"

with sync_playwright() as p:
    browser = p.chromium.launch(
        executable_path=os.environ.get("SAIL_TEST_CHROMIUM"), headless=True, args=["--no-sandbox"]
    )
    page = browser.new_page(viewport={"width": 1440, "height": 900})
    errors, failed = [], []
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.on("response", lambda response: failed.append(f"{response.status} {response.url}")
            if response.status >= 400 else None)

    status = {"configured": False, "connected": False, "redirect_uri": "https://127.0.0.1:8088/api/spotify/callback", "now_playing": None}
    calls = []
    def handle(route):
        req = route.request
        if req.url.endswith('/status'):
            route.fulfill(status=200, content_type='application/json', body=json.dumps(status)); return
        calls.append((req.url.rsplit('/',1)[1], req.post_data, req.headers.get('authorization')))
        body = {"now_playing": dict(status["now_playing"] or {}, is_playing=False)} if req.url.endswith('/control') else {"url": "about:blank#spotify", "configured": True, "redirect_uri": status["redirect_uri"]}
        route.fulfill(status=200, content_type='application/json', body=json.dumps(body))
    page.route("**/api/spotify/**", handle)
    page.add_init_script("localStorage.setItem('sail_owner_token', 'x'.repeat(40))")
    page.goto(base, wait_until="load")
    page.wait_for_selector('#tmNowPlaying:not([hidden])')
    np = page.locator('#tmNowPlaying')
    assert 'Set up Spotify' in np.inner_text()
    np.screenshot(path=str(SHOT.with_name(SHOT.stem + '-setup.png')))
    np.get_by_role('button', name='Set up Spotify').click()
    page.wait_for_selector('#tmSpotifySetup[open]')
    assert page.locator('#tmSpotifyRedirect').inner_text() == status["redirect_uri"]
    assert page.evaluate("document.activeElement.id") == 'tmSpotifyClient'
    page.locator('#tmSpotifySetup').screenshot(path=str(SHOT.with_name(SHOT.stem + '-dialog.png')))
    page.fill('#tmSpotifyClient', '0123456789abcdef0123456789abcdef')
    with page.context.expect_page() as popup:
        page.get_by_role('button', name='Save and connect').click()
    popup.value.close()
    page.wait_for_selector('#tmSpotifySetup', state='hidden')
    assert [c[0] for c in calls] == ['setup', 'login'], calls
    assert calls[0][2] == 'Bearer ' + 'x'*40
    status.update(configured=True)
    page.wait_for_timeout(5500)  # the map polls every 5 s
    assert 'Connect Spotify' in np.inner_text(), np.inner_text()
    status.update(connected=True, now_playing={"is_playing": True, "name": "How to Disappear Completely", "by": "Radiohead", "album": "Kid A", "image": None, "device": "Pedro's phone"})
    page.wait_for_timeout(5500)
    assert 'How to Disappear Completely' in np.inner_text() and 'Now Playing' in np.inner_text()
    np.screenshot(path=str(SHOT.with_name(SHOT.stem + '-playing.png')))
    page.locator('#tmNowPlaying [data-key="pause"]').focus()
    page.keyboard.press('Enter')
    page.wait_for_selector('#tmNowPlaying [data-key="resume"]')
    assert calls[-1][0] == 'control' and json.loads(calls[-1][1]) == {"action": "pause"}
    assert page.evaluate("document.activeElement.dataset.key") == 'resume', 'focus kept on the play/pause control'
    assert 'Paused' in np.inner_text()
    page.screenshot(path=str(SHOT))
    assert not [e for e in errors if e != 'createBtn is not defined'], errors
    browser.close()

server.shutdown()
print(f"Spotify Now Playing card checks passed; screenshots: {SHOT}")
