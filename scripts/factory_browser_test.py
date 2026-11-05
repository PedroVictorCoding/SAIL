"""Verifies the Factory tools map in a real headless browser.

Serves `crates/sail-dialogue/web` the way Dialogue does (index.html plus the static
files the page requests), verifies Factory and Settings share the Tools Map,
and checks station navigation, browser history, reloads and mobile exit controls.

Run with: .venv-voice/bin/python scripts/factory_browser_test.py [screenshot.png]
"""
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
SHOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/factory-room.png")


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith("/vendor/") and "/xterm/" not in self.path:
            self.path = "/vendor/xterm/" + self.path.split("/")[-1]
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

    page.goto(base, wait_until="load")
    page.wait_for_function("window.sailFactory !== undefined")

    # Factory uses the Settings map, without restoring a global style override.
    assert page.locator('#factoryFloor').count() == 0
    assert page.locator('[data-ui-style]').count() == 0
    page.evaluate("localStorage.setItem('sail_ui_style', 'tools-map')")
    page.reload(wait_until="load")
    assert page.locator('#toolsMap').count() == 0
    assert page.evaluate("localStorage.getItem('sail_ui_style')") is None

    page.click('[data-page="factory"]')
    page.wait_for_selector('#toolsMap')
    assert page.evaluate('location.hash') == '#factory'
    assert page.locator('.tm-label').count() == 13
    assert page.locator('#factoryCanvas').count() == 0
    assert page.title() == 'SAIL — Factory'
    page.locator('.tm-nav-item[data-nav="mind"]').click()
    assert page.locator('.tm-label[data-station="mind"]').get_attribute('aria-pressed') == 'true'
    page.locator('.tm-open').click()
    page.wait_for_function("location.hash === '#mind'")
    page.wait_for_selector('#toolsMap', state='hidden')
    page.go_back()
    page.wait_for_selector('#toolsMap')
    assert page.locator('#toolsMap').count() == 1

    page.reload(wait_until="load")
    page.wait_for_selector('#toolsMap')
    page.screenshot(path=str(SHOT))
    page.locator('.tm-sidecard-actions button', has_text='Home').click()
    page.wait_for_function("location.hash === '#home'")
    page.wait_for_selector('#toolsMap', state='hidden')
    page.evaluate("document.getElementById('settingsOpen').click()")
    page.click('#openFactorySettings')
    page.wait_for_selector('#toolsMap')
    assert page.is_hidden('#contextSheet')

    page.set_viewport_size({"width": 390, "height": 844})
    page.emulate_media(reduced_motion="reduce")
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-mobile.png')))
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    page.locator('#tmViewport').focus()
    page.keyboard.press('ArrowRight')
    assert page.locator('#tmViewport').evaluate('(node) => node === document.activeElement')
    page.locator('.tm-home').click()
    page.wait_for_selector('#toolsMap', state='hidden')
    # Existing reminders.js startup error is outside the Factory implementation.
    unrelated = [error for error in errors if error == 'createBtn is not defined']
    assert not [error for error in errors if error not in unrelated], errors
    if unrelated:
        print(f"Existing reminders.js startup error observed on {len(unrelated)} loads: {unrelated[0]}")
    assert not [entry for entry in failed if 'factory.js' in entry], failed
    browser.close()

server.shutdown()
print(f"Factory tools map routing, Settings, station navigation and mobile checks passed; screenshots: {SHOT}")
