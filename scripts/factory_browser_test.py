"""Verifies the Tools Map workspace in a real headless browser.

Serves `crates/sail-dialogue/web` the way Dialogue does (index.html plus the static
files the page requests), verifies Factory and Settings share the Tools Map,
and checks system popups, persistent Dialogue, browser history and mobile layouts.

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

    # Count only work scheduled by toolsmap.js, independently of the shared UI.
    page.add_init_script("""
      window.factoryWork = {frames: 0, requests: 0};
      const raf = window.requestAnimationFrame;
      window.requestAnimationFrame = function(callback) {
        const map = new Error().stack.includes('toolsmap.js');
        return raf.call(window, time => {
          if (map) window.factoryWork.frames++;
          callback(time);
        });
      };
      const fetch = window.fetch;
      window.fetch = function(...args) {
        if (new Error().stack.includes('toolsmap.js')) window.factoryWork.requests++;
        return fetch.apply(window, args);
      };
    """)
    page.goto(base, wait_until="load")
    page.wait_for_function("window.sailFactory !== undefined")
    assert page.locator('[data-ui-style]').count() == 0
    page.evaluate("localStorage.setItem('sail_ui_style', 'tools-map')")
    page.reload(wait_until="load")
    assert page.locator('#factoryMapContent > #toolsMap').count() == 1
    assert page.evaluate("localStorage.getItem('sail_ui_style')") is None

    def check_embedded():
        page.wait_for_selector('#factoryMapContent > #toolsMap')
        assert page.locator('#toolsMap').count() == 1
        assert page.locator('#toolsMap').evaluate("n => getComputedStyle(n).position") != 'fixed'
        assert page.locator('.tm-side, .tm-top, .tm-ask, .tm-home, .tm-title').count() == 0
        assert page.locator('.tm-label').count() == 13
        assert page.is_hidden('#subsystemNotice')
        assert page.is_hidden('.subsystem-about')
        assert page.is_visible('#factoryTitle')
        for selector in ['#homeView', '#orbCanvas', '#conversation', '#textForm']:
            assert page.is_visible(selector), selector
        assert page.locator('#settingsOpen:visible, #settingsRail:visible').count() >= 1
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
        assert page.locator('#toolsMap').evaluate("n => n.scrollWidth <= n.clientWidth")

    page.click('[data-page="factory"]')
    check_embedded()
    assert page.title() == 'SAIL — Factory'
    page.screenshot(path=str(SHOT))

    # Native assistant input stays operable while Factory is active.
    page.fill('#textInput', 'Message beside Factory')
    assert page.input_value('#textInput') == 'Message beside Factory'
    page.locator('#settingsOpen:visible, #settingsRail:visible').first.click()
    page.wait_for_selector('#contextSheet')
    page.click('#contextSheet .close-sheet')
    page.locator('.tm-label[data-station="mind"]').click()
    assert page.locator('.tm-label[data-station="mind"]').get_attribute('aria-pressed') == 'true'
    page.locator('#tmTab-tasks').click()
    assert page.locator('#tmTabPanel').get_attribute('aria-labelledby') == 'tmTab-tasks'
    page.locator('.tm-open').click()
    page.wait_for_function("window.sailCurrentPage() === 'mind'")
    assert page.is_visible('#subsystemDialog')
    assert page.is_visible('#toolsMap')
    assert page.is_visible('.subsystem-about')
    assert page.is_visible('#textForm')
    page.fill('#textInput', 'Message beside Factory')
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-system.png')))
    page.wait_for_timeout(100)
    stopped = page.evaluate('({...window.factoryWork})')
    page.wait_for_timeout(5200)
    assert page.evaluate('window.factoryWork') == stopped, 'hidden map must stop polling and animation'
    page.go_back()
    check_embedded()
    assert page.locator('.tm-label[data-station="mind"]').get_attribute('aria-pressed') == 'true'
    assert page.locator('#tmTab-tasks').get_attribute('aria-selected') == 'true'
    assert page.input_value('#textInput') == 'Message beside Factory'
    page.evaluate("window.sailFactory.show()")
    check_embedded()
    assert page.locator('#tmTab-tasks').get_attribute('aria-selected') == 'true'

    # View modes and keyboard camera controls use the embedded viewport.
    for mode in ['topology', 'flows', 'resources', 'map']:
        page.locator(f'[data-mode="{mode}"].tm-seg').click()
        assert page.locator('#toolsMap').get_attribute('data-mode') == mode
    page.locator('#tmViewport').focus()
    before = page.locator('.tm-world').get_attribute('style')
    page.keyboard.press('ArrowRight')
    assert page.locator('.tm-world').get_attribute('style') != before
    before = page.locator('.tm-world').get_attribute('style')
    page.keyboard.press('+')
    assert page.locator('.tm-world').get_attribute('style') != before
    page.get_by_role('button', name='Fit map', exact=True).click()

    # Live preference survives route changes; animation stops independently.
    page.click('#tmLive')
    page.evaluate("window.sailNavigate('home')")
    page.wait_for_function("window.sailCurrentPage() === 'factory'")
    page.evaluate("window.sailToolsMap.show()")
    check_embedded()
    assert page.locator('#tmLive').get_attribute('aria-pressed') == 'false'
    requests = page.evaluate('window.factoryWork.requests')
    page.wait_for_timeout(5200)
    assert page.evaluate('window.factoryWork.requests') == requests
    page.click('#tmLive')
    page.wait_for_function('window.factoryWork.requests > ' + str(requests))

    # Container width, rather than window width, determines inspector placement.
    for width, name in [(1920, 'wide'), (1180, 'narrow'), (390, 'mobile')]:
        page.set_viewport_size({"width": width, "height": 900})
        page.wait_for_timeout(150)
        check_embedded()
        layout = page.evaluate("""() => {
          const root = document.getElementById('toolsMap').getBoundingClientRect();
          const stage = document.querySelector('.tm-stage').getBoundingClientRect();
          const inspector = document.getElementById('tmInspector').getBoundingClientRect();
          return {width: root.width, stacked: inspector.top >= stage.bottom};
        }""")
        assert layout['stacked'] == (layout['width'] < 900), layout
        page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-' + name + '.png')))

    # Resize within the same browser viewport also refits the map.
    page.set_viewport_size({"width": 1440, "height": 900})
    page.wait_for_timeout(100)
    before = page.locator('.tm-world').get_attribute('style')
    page.locator('#factoryMapContent').evaluate("n => n.style.width = '320px'")
    page.wait_for_timeout(100)
    assert page.locator('.tm-world').get_attribute('style') != before
    page.locator('#factoryMapContent').evaluate("n => n.style.width = ''")

    page.emulate_media(reduced_motion='reduce', forced_colors='active')
    page.keyboard.press('Tab')
    page.locator('#tmViewport').focus()
    assert page.locator('#tmViewport').evaluate("n => getComputedStyle(n).outlineStyle") != 'none'
    page.locator('[data-mode="resources"].tm-seg').click()
    assert page.locator('[data-mode="resources"].tm-seg').get_attribute('aria-pressed') == 'true'
    page.emulate_media(reduced_motion='no-preference', forced_colors='none')

    page.reload(wait_until='load')
    check_embedded()
    page.click('#homeNav')
    page.wait_for_function("window.sailCurrentPage() === 'factory'")
    page.locator('#settingsOpen:visible, #settingsRail:visible').first.click()
    page.click('#openFactorySettings')
    check_embedded()
    assert page.is_hidden('#contextSheet')
    page.goto(base + '#unknown')
    page.wait_for_function("location.hash === '#home'")
    assert page.is_visible('#toolsMap')

    # Every system opens its original live card inside the workspace popup.
    page.evaluate("window.sailNavigate('guardian')")
    page.wait_for_function("window.sailCurrentPage() === 'guardian'")
    assert page.is_visible('#subsystemDialog #toolsCard')
    page.keyboard.press('Escape')
    page.wait_for_function("window.sailCurrentPage() === 'factory'")
    assert page.is_hidden('#subsystemDialog')
    page.evaluate("window.sailNavigate('dialogue')")
    page.wait_for_function("window.sailCurrentPage() === 'dialogue'")
    assert page.is_hidden('#subsystemDialog')
    assert page.locator('#textInput').evaluate('n => document.activeElement === n')
    for target in ['mind', 'arena', 'core', 'flow', 'forge']:
        page.evaluate('(id) => window.sailNavigate(id)', target)
        page.wait_for_function('(id) => window.sailCurrentPage() === id', arg=target)
        assert page.is_visible('#subsystemDialog')
        assert page.is_visible('#textForm')
        page.click('#subsystemClose')
        page.wait_for_function("window.sailCurrentPage() === 'factory'")
    page.goto(base + '#mind', wait_until='load')
    page.wait_for_selector('#subsystemDialog[open] #modelsCard')
    assert page.is_visible('#toolsMap')
    page.click('#subsystemClose')
    page.wait_for_function("window.sailCurrentPage() === 'factory'")
    page.locator('.tm-label[data-station="mind"]').click()
    page.locator('.tm-open').click()
    page.wait_for_selector('#subsystemDialog[open]')
    page.click('#subsystemClose')
    page.wait_for_function("window.sailCurrentPage() === 'factory'")
    page.wait_for_function("document.activeElement.id === 'tmOpenSystem'")
    page.set_viewport_size({'width': 390, 'height': 900})
    page.evaluate("window.sailNavigate('mind')")
    page.wait_for_selector('#subsystemDialog[open]')
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-system-mobile.png')))
    page.locator('#textInput').fill('Dialogue remains available')
    page.click('#subsystemClose')
    page.wait_for_function("window.sailCurrentPage() === 'factory'")

    # Existing reminders.js startup error is outside the Factory implementation.
    unrelated = [error for error in errors if error == 'createBtn is not defined']
    assert not [error for error in errors if error not in unrelated], errors
    if unrelated:
        print(f"Existing reminders.js startup error observed on {len(unrelated)} loads: {unrelated[0]}")
    assert not [entry for entry in failed if 'factory.js' in entry or 'toolsmap' in entry], failed
    browser.close()

server.shutdown()
print(f"Embedded Factory checks passed: shell access, routing, selection, polling, camera and responsive layout; screenshots: {SHOT}")
