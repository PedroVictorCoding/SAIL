"""Verifies the Tools Map workspace in a real headless browser.

Serves `crates/sail-dialogue/web` the way Dialogue does (index.html plus the static
files the page requests) and checks that the map is the default workspace with
Dialogue beside it, that map boxes show compact status first, that systems and the
Tracker replace the map in the centre (history, direct links, focus, Settings and
dialogs), the classic dashboard exit, and mobile layout.

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

    page.goto(base, wait_until="load")
    page.wait_for_function("window.sailFactory !== undefined")

    # Home: the Tools Map in the centre, with the live Dialogue panel beside it.
    page.wait_for_selector('#toolsMap')
    assert page.evaluate('location.hash') in ('', '#home')
    assert page.title() == 'SAIL — Workspace'
    assert page.locator('#factoryFloor').count() == 0
    assert page.locator('[data-ui-style]').count() == 0
    assert page.evaluate("localStorage.getItem('sail_ui_style')") is None
    assert page.locator('.tm-label').count() == 13
    assert page.locator('#tmChat #homeView #textForm').count() == 1
    assert page.is_hidden('#tmPane') and page.is_visible('#tmViewport')
    assert page.locator('.tm-nav-item[data-nav="home"]').get_attribute('aria-current') == 'page'

    # The conversation follows streamed replies unless the owner has scrolled up.
    gap = "(() => { const c = document.getElementById('conversation'); return c.scrollHeight - c.scrollTop - c.clientHeight; })()"
    def stream(chunks):
        page.evaluate("""(n) => { for (let i = 0; i < n; i++) window.__reply += 'word '.repeat(12) + '\\n\\n';
          setMessageText(window.__node, window.__reply); }""", chunks)
        page.wait_for_timeout(100)
    page.evaluate("appendConversation('You', 'hello'); window.__node = appendConversation('SAIL', ''); window.__reply = ''")
    for _ in range(12):
        stream(2)
    assert page.evaluate(gap) <= 4, 'did not follow the streamed reply'
    conv = page.locator('#conversation').bounding_box()
    page.mouse.move(conv['x'] + 40, conv['y'] + 40)
    page.mouse.wheel(0, -300)
    page.wait_for_timeout(200)
    held = page.evaluate("document.getElementById('conversation').scrollTop")
    stream(6)
    assert page.evaluate("document.getElementById('conversation').scrollTop") == held, 'pulled away from where the owner scrolled'
    page.mouse.wheel(0, 100000)
    page.wait_for_timeout(300)
    stream(4)
    assert page.evaluate(gap) <= 4, 'did not resume following at the bottom'
    page.mouse.wheel(0, -300)
    page.wait_for_timeout(200)
    page.evaluate("appendConversation('You', 'next'); window.__node = appendConversation('SAIL', ''); window.__reply = ''")
    stream(4)
    assert page.evaluate(gap) <= 4, 'own message did not resume following'
    page.evaluate("document.getElementById('conversation').replaceChildren()")

    def centre_box(selector):
        return page.locator(selector).bounding_box()

    map_box = centre_box('.tm-stage')

    # Every map box first selects its compact status window, without navigating.
    for station in page.locator('.tm-label').all():
        station.click()
        assert station.get_attribute('aria-pressed') == 'true'
        assert page.is_visible('#tmDetail'), station.get_attribute('data-station')
        assert page.is_hidden('#tmPane')
        assert page.evaluate('location.hash') in ('', '#home')
    page.locator('.tm-label[data-station="mind"]').focus()
    page.keyboard.press('Enter')
    assert page.locator('#tmDetail .tm-detail-title').inner_text().startswith('Mind')
    assert page.is_hidden('#tmPane')
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-detail.png')))

    # Selecting it again replaces the map in the centre with its real controls and
    # status; the sidebar, header, Dialogue and activity stay put and usable.
    page.keyboard.press('Space')
    page.wait_for_selector('#tmPane')
    page.wait_for_function("location.hash === '#mind'")
    assert page.is_hidden('.tm-stage')
    assert page.locator('#tmPane #subsystemTitle').inner_text() == 'Mind'
    assert page.evaluate('document.activeElement.id') == 'subsystemTitle'
    assert page.locator('#tmPane #modelsCard').is_visible()
    # The Dialogue column stays on Chat; its info tabs show the open system's status.
    assert page.locator('#tmTab-chat').get_attribute('aria-selected') == 'true'
    assert page.is_hidden('#tmInspector') and page.is_visible('#tmChat #homeView')
    page.locator('#tmTab-context').click()
    assert page.locator('#tmInspector .tm-insp-head h3').inner_text() == 'Mind'
    assert page.is_hidden('#tmChat #homeView')
    assert page.locator('#tmInspector').get_attribute('aria-labelledby') == 'tmTab-context'
    assert page.locator('#tmInfo-overview').get_attribute('aria-selected') == 'true'
    assert page.locator('#tmInfoPanel').get_attribute('aria-labelledby') == 'tmInfo-overview'
    page.locator('#tmInfo-overview').focus()
    page.keyboard.press('ArrowRight')
    assert page.evaluate('document.activeElement.id') == 'tmInfo-tasks'
    assert page.locator('#tmInspector .tm-card h3').first.inner_text() == 'Task Queue'
    page.keyboard.press('End')
    assert page.evaluate('document.activeElement.id') == 'tmInfo-config'
    page.locator('#tmTab-context').focus()
    page.keyboard.press('Home')
    assert page.evaluate('document.activeElement.id') == 'tmTab-chat'
    assert page.is_visible('#tmChat #homeView') and page.is_hidden('#tmInspector')
    tabs_right = page.locator('#tmTabs').bounding_box()
    chat_box = page.locator('#tmChat').bounding_box()
    assert tabs_right['x'] + tabs_right['width'] <= chat_box['x'] + chat_box['width'], (tabs_right, chat_box)
    assert centre_box('#tmPane') == map_box, (centre_box('#tmPane'), map_box)
    assert page.locator('.tm-nav-item[data-nav="mind"]').get_attribute('aria-current') == 'page'
    assert page.locator('#tmCrumb').inner_text() == 'Mind'
    assert page.locator('#tmPaneBack').is_hidden()  # Home in the sidebar leads back
    page.locator('#textInput').fill('still usable beside a system')
    assert page.locator('#textInput').evaluate('(n) => n === document.activeElement')
    page.locator('#textInput').fill('')
    page.keyboard.press('Escape')
    assert page.is_visible('#tmPane'), 'Escape no longer leaves a system'
    page.locator('#tmTab-context').click()
    assert page.locator('#tmInfo-config').get_attribute('aria-selected') == 'true'  # Context keeps its view

    # Sidebar entries switch the centre directly; Home brings the map back.
    page.locator('.tm-nav-item[data-nav="guardian"]').click()
    page.wait_for_function("document.querySelector('#tmInspector .tm-insp-head h3')?.textContent === 'Guardian'")
    assert page.evaluate('location.hash') == '#guardian'
    page.locator('.tm-nav-item[data-nav="home"]').click()
    page.wait_for_selector('#tmPane', state='hidden')
    assert page.is_visible('#tmViewport') and page.evaluate('location.hash') == '#home'

    # Stations sharing a page keep their own inspector.
    page.locator('.tm-label[data-station="memory"]').click()
    page.locator('.tm-label[data-station="memory"]').click()
    page.wait_for_function("document.querySelector('#tmInspector .tm-insp-head h3')?.textContent === 'Memory'")
    assert page.evaluate('location.hash') == '#arena'
    page.locator('.tm-nav-item[data-nav="home"]').click()
    page.wait_for_selector('#tmPane', state='hidden')
    assert page.evaluate("document.activeElement.dataset.station") == 'memory'

    # Systems without a page still show in the centre, without changing the route.
    page.locator('.tm-label[data-station="entertainment"]').click()
    page.locator('.tm-label[data-station="entertainment"]').click()
    assert page.is_visible('#tmPane') and page.is_visible('#tmPaneTitle')
    assert page.is_hidden('#subsystemPage')
    assert page.evaluate('location.hash') == '#home'
    page.locator('.tm-nav-item[data-nav="home"]').click()
    page.wait_for_selector('#tmPane', state='hidden')

    # Tracker loads in the centre too, without its own way back to SAIL.
    page.locator('.tm-nav-item[data-nav="tracker"]').click()
    page.wait_for_function("location.hash === '#tracker'")
    page.wait_for_selector('#tmTracker')
    assert page.title() == 'SAIL — Tracker'
    assert page.is_hidden('.tm-pane-main')
    assert page.locator('#tmInspector .tm-insp-head h3').inner_text() == 'SAIL Core'
    assert page.locator('.tm-nav-item[data-nav="tracker"]').get_attribute('aria-current') == 'page'
    frame = page.frame_locator('#tmTracker')
    frame.locator('#tkRefresh, .tk-head, body').first.wait_for()
    page.wait_for_function("document.getElementById('tmTracker').contentDocument?.readyState === 'complete'")
    assert page.evaluate("document.getElementById('tmTracker').contentDocument.querySelector('.tk-back') === null")
    tracker_box = centre_box('#tmPane')
    assert tracker_box == map_box, (tracker_box, map_box)
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-tracker.png')))

    # Tasks opens Forge on its task list; dialogs from a system work in place.
    page.locator('.tm-nav-item', has_text='Tasks').click()
    page.wait_for_function("document.getElementById('tmInfo-tasks')?.getAttribute('aria-selected') === 'true'")
    page.wait_for_function("document.querySelector('#tmInspector .tm-insp-head h3')?.textContent === 'Forge'")
    assert page.evaluate('location.hash') == '#forge'
    assert page.is_hidden('.tm-pane-tracker')
    page.locator('#forgeQuickEnqueueBtn').click()
    assert page.is_visible('#forgeEnqueueDialog')
    page.keyboard.press('Escape')
    assert page.is_hidden('#forgeEnqueueDialog') and page.is_visible('#tmPane')

    # Browser history and direct links.
    page.go_back()
    page.wait_for_function("location.hash === '#tracker'")
    page.wait_for_selector('#tmTracker')
    page.go_back()
    page.wait_for_selector('#tmPane', state='hidden')
    page.go_forward()
    page.wait_for_selector('#tmTracker')
    page.goto(base + '#guardian')
    page.reload(wait_until="load")
    page.wait_for_selector('#tmPane')
    assert page.locator('#subsystemTitle').inner_text() == 'Guardian'
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-system.png')))

    # Settings opened from a system opens above the workspace.
    page.goto(base + '#mind')
    page.wait_for_function("document.getElementById('subsystemTitle').textContent === 'Mind'")
    page.locator('#subsystemSettings').click()
    page.wait_for_selector('[data-category="models"]')
    page.keyboard.press('Escape')
    assert page.is_hidden('#contextSheet') and page.is_visible('#tmPane')
    page.locator('.tm-nav-item[data-nav="home"]').click()
    page.wait_for_selector('#tmPane', state='hidden')
    page.locator('#tmTab-context').click()
    page.locator('#tmInfo-overview').click()
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-column-overview.png')))
    # Asking from the header search returns the column to the conversation.
    page.locator('#tmAskInput').fill('hello')
    page.locator('#tmAskInput').press('Enter')
    assert page.locator('#tmTab-chat').get_attribute('aria-selected') == 'true'
    page.screenshot(path=str(SHOT))

    # The classic dashboard stays available and gets its Dialogue panel back.
    page.locator('.tm-sidecard-actions button', has_text='Classic view').click()
    page.wait_for_function("location.hash === '#classic'")
    page.wait_for_selector('#toolsMap', state='hidden')
    assert page.is_visible('#homePage')
    assert page.evaluate("document.getElementById('homeView').parentElement.classList.contains('dashboard-layout')")
    page.locator('#trackerNav').click()
    page.wait_for_selector('#tmTracker')
    page.goto(base + '#classic')
    page.wait_for_selector('#homePage')
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
    assert page.is_visible('#tmDetail') and page.is_hidden('#tmPane')
    page.locator('.tm-label[data-station="guardian"]').click()
    page.wait_for_selector('#tmPaneBack')  # no sidebar on phones: the pane has its own way back
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    page.screenshot(path=str(SHOT.with_name(SHOT.stem + '-mobile-system.png')))
    page.locator('#tmPaneBack').click()
    page.wait_for_selector('#tmPane', state='hidden')
    page.goto(base + '#tracker')
    page.wait_for_selector('#tmTracker')
    assert page.locator('#tmTracker').bounding_box()['height'] > 500
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    # Existing reminders.js startup error is outside the Factory implementation.
    unrelated = [error for error in errors if error == 'createBtn is not defined']
    assert not [error for error in errors if error not in unrelated], errors
    if unrelated:
        print(f"Existing reminders.js startup error observed on {len(unrelated)} loads: {unrelated[0]}")
    assert not [entry for entry in failed if 'factory.js' in entry], failed
    browser.close()

server.shutdown()
print(f"Tools Map workspace, Dialogue column and auto-scroll, centre systems and Tracker, history, Settings and mobile checks passed; screenshots: {SHOT}")
