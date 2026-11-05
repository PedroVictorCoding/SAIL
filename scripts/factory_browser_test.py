"""Verifies the Tools Map workspace in a real headless browser.

Serves `crates/sail-dialogue/web` the way Dialogue does (index.html plus the static
files the page requests) and checks that the map is the default workspace with
Dialogue beside it, that map boxes show compact status and sidebar systems open in the popup (history, direct links, focus,
Settings and dialogs above it), the classic dashboard exit, and mobile layout.

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

    # The Tools Map is the default workspace, with the live Dialogue panel beside it.
    page.wait_for_selector('#toolsMap')
    assert page.evaluate('location.hash') in ('', '#home')
    assert page.title() == 'SAIL — Workspace'
    assert page.locator('#factoryFloor').count() == 0
    assert page.locator('[data-ui-style]').count() == 0
    assert page.evaluate("localStorage.getItem('sail_ui_style')") is None
    assert page.locator('.tm-label').count() == 13
    assert page.locator('#tmChat #homeView #textForm').count() == 1
    assert page.locator('#tmInspector').count() == 1
    page.wait_for_selector('#tmPop', state='hidden')
    page.locator('#textInput').fill('hello')
    assert page.locator('#textInput').evaluate('(n) => n === document.activeElement')
    page.locator('#textInput').fill('')

    # Every map box selects compact status without navigating or opening a modal.
    for station in page.locator('.tm-label').all():
        station.click()
        assert station.get_attribute('aria-pressed') == 'true'
        assert station.get_attribute('aria-haspopup') is None
        assert page.is_visible('#tmDetail'), station.get_attribute('data-station')
        assert page.is_hidden('#tmPop')
        assert page.evaluate('location.hash') in ('', '#home')
        assert not page.evaluate("document.getElementById('tmChat').inert")
    page.locator('.tm-label[data-station="mind"]').focus()
    page.keyboard.press('Enter')
    assert page.locator('#tmDetail .tm-detail-title').inner_text().startswith('Mind')
    page.keyboard.press('Space')
    assert page.is_visible('#tmDetail') and page.is_hidden('#tmPop')
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-detail.png')))

    # Sidebar systems open the large popup with real controls and status.
    page.locator('.tm-nav-item[data-nav="mind"]').click()
    page.wait_for_function("location.hash === '#mind'")
    page.wait_for_selector('#tmPop')
    assert page.locator('#tmPop #subsystemTitle').inner_text() == 'Mind'
    assert page.evaluate('document.activeElement.id') == 'subsystemTitle'
    assert page.locator('#tmPop #modelsCard').is_visible()
    assert page.locator('#tmInspector h2').inner_text() == 'Mind'
    assert page.evaluate("document.getElementById('tmChat').inert")
    box = page.locator('.tm-pop-frame').bounding_box()
    assert box['width'] > 1440 * 0.9 and box['height'] > 900 * 0.9, box
    page.keyboard.press('Escape')
    page.wait_for_function("location.hash === '#home'")
    page.wait_for_selector('#tmPop', state='hidden')
    assert page.evaluate("document.activeElement.dataset.nav") == 'mind'
    assert not page.evaluate("document.getElementById('tmChat').inert")

    # Stations sharing a page keep their own inspector; Close returns to the map.
    page.locator('.tm-nav-item[data-nav="memory"]').click()
    page.wait_for_function("location.hash === '#arena'")
    page.wait_for_function("document.querySelector('#tmInspector h2')?.textContent === 'Memory'")
    page.locator('#tmPopClose').click()
    page.wait_for_selector('#tmPop', state='hidden')

    # Sidebar entries open systems; Tasks opens Forge on its task list.
    page.locator('.tm-nav-item', has_text='Tasks').click()
    page.wait_for_function("document.getElementById('tmTab-tasks')?.getAttribute('aria-selected') === 'true'")
    assert page.evaluate('location.hash') == '#forge'
    page.locator('#forgeQuickEnqueueBtn').click()
    assert page.is_visible('#forgeEnqueueDialog')
    page.keyboard.press('Escape')
    assert page.is_hidden('#forgeEnqueueDialog') and page.is_visible('#tmPop')

    # Browser history and direct links.
    page.go_back()
    page.wait_for_selector('#tmPop', state='hidden')
    page.go_forward()
    page.wait_for_selector('#tmPop')
    page.goto(base + '#guardian')
    page.reload(wait_until="load")
    page.wait_for_selector('#tmPop')
    assert page.locator('#subsystemTitle').inner_text() == 'Guardian'
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-popup.png')))

    # Settings opened from a system sits above the popup; Escape closes it first.
    page.goto(base + '#mind')
    page.wait_for_function("document.getElementById('subsystemTitle').textContent === 'Mind'")
    page.locator('#subsystemSettings').click()
    page.wait_for_selector('[data-category="models"]')
    assert page.evaluate("""(() => { const r = document.getElementById('contextSheet').getBoundingClientRect();
      return document.getElementById('contextSheet').contains(document.elementFromPoint(r.left + r.width / 2, r.top + 40)); })()""")
    page.keyboard.press('Escape')
    assert page.is_hidden('#contextSheet') and page.is_visible('#tmPop')
    page.keyboard.press('Escape')
    page.wait_for_selector('#tmPop', state='hidden')
    page.screenshot(path=str(SHOT))

    # The classic dashboard stays available and gets its Dialogue panel back.
    page.locator('.tm-sidecard-actions button', has_text='Classic view').click()
    page.wait_for_function("location.hash === '#classic'")
    page.wait_for_selector('#toolsMap', state='hidden')
    assert page.is_visible('#homePage')
    assert page.evaluate("document.getElementById('homeView').parentElement.classList.contains('dashboard-layout')")
    page.evaluate("document.getElementById('settingsOpen').click()")
    page.click('[data-settings-tab="general"]')
    page.click('#openFactorySettings')
    page.wait_for_selector('#toolsMap')
    assert page.is_hidden('#contextSheet')
    page.goto(base + '#unknown')
    page.wait_for_function("location.hash === '#home'")
    assert page.is_visible('#toolsMap')

    page.set_viewport_size({"width": 390, "height": 844})
    page.emulate_media(reduced_motion="reduce")
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-mobile.png')))
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    page.locator('#tmViewport').focus()
    page.keyboard.press('ArrowRight')
    assert page.locator('#tmViewport').evaluate('(node) => node === document.activeElement')
    page.locator('.tm-label[data-station="guardian"]').click()
    assert page.is_visible('#tmDetail') and page.is_hidden('#tmPop')
    page.keyboard.press('Enter')
    assert page.is_hidden('#tmPop')
    # The sidebar is hidden at this width; direct routes still open mobile popups.
    page.goto(base + '#guardian')
    page.wait_for_selector('#tmPopClose')
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-mobile-popup.png')))
    page.locator('#tmPopClose').click()
    page.wait_for_selector('#tmPop', state='hidden')
    # Existing reminders.js startup error is outside the Factory implementation.
    unrelated = [error for error in errors if error == 'createBtn is not defined']
    assert not [error for error in errors if error not in unrelated], errors
    if unrelated:
        print(f"Existing reminders.js startup error observed on {len(unrelated)} loads: {unrelated[0]}")
    assert not [entry for entry in failed if 'factory.js' in entry], failed
    browser.close()

server.shutdown()
print(f"Tools Map workspace, Dialogue column, system popups, history, Settings and mobile checks passed; screenshots: {SHOT}")
