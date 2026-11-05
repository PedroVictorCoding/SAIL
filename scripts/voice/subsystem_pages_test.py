"""Offline navigation regression test; uses the existing Playwright test dependency.

Run: python scripts/voice/subsystem_pages_test.py
No SAIL daemon, model server, microphone, or external service is required.
"""
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import argparse
import re
import threading

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[2]


class UIHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT / "crates/sail-dialogue/web"), **kwargs)

    def do_GET(self):
        # Dialogue serves the vendored xterm files from /vendor/<name>.
        if self.path.startswith("/vendor/") and "/xterm/" not in self.path:
            self.path = "/vendor/xterm/" + self.path.split("/")[-1]
        if self.path == "/icons.svg":
            self.send_response(200)
            self.send_header("Content-Type", "image/svg+xml")
            self.end_headers()
            self.wfile.write((ROOT / "assets/source/icons/sail-icons.svg").read_bytes())
        elif self.path.startswith("/api/"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{}")
        else:
            super().do_GET()

    def log_message(self, *_args):
        pass


def check_pages(page, url, screenshots=None):
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.goto(url)
    page.wait_for_function("typeof window.sailNavigate === 'function'")
    ids = page.evaluate("subsystemPages.map(page => page.id)")
    # New registered subsystems must not silently miss out on navigation.
    types = (ROOT / "crates/sail-common/src/types.rs").read_text()
    variants = types.split("pub enum SubsystemId {", 1)[1].split("}", 1)[0]
    registered = {name.lower() for name in re.findall(r"^\s*(\w+),", variants, re.M)}
    # Every registered subsystem gets one page; extra rooms (the Factory) have their own tests.
    assert len(ids) == len(set(ids)), "each page must appear once in the navigation"
    assert registered - {"ui"} <= set(ids), "the Ui subsystem is the interface itself"
    assert page.locator("#subsystemNav a").count() == len(ids)
    ids = [subsystem for subsystem in ids if subsystem not in {"factory", "dialogue"}]
    # Card tabs follow the WAI-ARIA tabs pattern; arrow keys replace the old cycle buttons.
    # The card dashboard is the #classic route; the default workspace is the Tools Map.
    page.goto(url + "/#classic")
    expect(page.locator("#homePage")).to_be_visible()
    assert page.locator(".card-cycle-btn").count() == 0
    page.evaluate("window.originalCards = [...document.querySelectorAll('.workspace-grid > article')]")
    page.evaluate("window.sailNavigate('core')")
    expect(page.locator('#subsystemDialog')).to_be_visible()
    first = page.locator('#systemCard [role="tab"]').first
    first.focus()
    page.keyboard.press("ArrowRight")
    expect(page.locator('#systemCard [role="tab"][aria-controls="systemPanelGuardian"]')).to_have_attribute("aria-selected", "true")
    expect(page.locator("#systemPanelGuardian")).to_be_visible()
    assert page.evaluate("document.activeElement.getAttribute('aria-controls')") == "systemPanelGuardian"
    page.keyboard.press("Home")
    expect(page.locator("#systemPanelMetrics")).to_be_visible()
    # Tab strips sit on their own row and never overlap the card title.
    assert page.evaluate("""[...document.querySelectorAll('#subsystemContent .card-heading')].every(h => {
      const title = h.querySelector('.card-title').getBoundingClientRect();
      const tabs = h.querySelector('.card-tabs')?.getBoundingClientRect();
      return !tabs || tabs.top >= title.bottom - 1;
    })""")
    for subsystem in ids:
        link = page.locator(f'#subsystemNav a[href="#{subsystem}"]')
        if page.locator("#homePage").is_visible():
            link.focus()
            page.keyboard.press("Enter")
        else:
            page.evaluate(f"sailNavigate('{subsystem}')")
        expect(page.locator("#subsystemTitle")).to_have_text(subsystem.title())
        expect(page.locator("#tmPop #subsystemPage")).to_be_visible()
        expect(page.locator("#homePage")).to_be_hidden()
        assert page.evaluate("document.activeElement.id") == "subsystemTitle"
        assert page.locator('#subsystemNav [aria-current="page"]').count() == 1
        assert page.evaluate("""originalCards.every(card =>
          document.querySelectorAll('#' + card.id).length === 1 &&
          document.getElementById(card.id) === card)""")
        if subsystem in {"flow", "discovery"}:
            expect(page.locator("#subsystemNotice")).to_be_visible()
        elif page.evaluate(f"subsystemPages.find(p => p.id === '{subsystem}').panel"):
            expect(page.locator("#subsystemContent .card-tab-panel.active")).to_be_visible()
        else:
            expect(page.locator("#subsystemContent > article")).to_be_visible()

    page.locator("#homeNav").click()
    expect(page.locator("#factoryWorkspace")).to_be_visible()
    expect(page.locator("#subsystemDialog")).to_be_hidden()
    assert page.evaluate("originalCards.every(card => card.parentElement.matches('.workspace-grid'))")
    page.locator('#subsystemNav a[href="#forge"]').click()
    expect(page.locator("#subsystemContent #forgeCard")).to_be_visible()
    page.locator("#forgeQuickEnqueueBtn").click()
    expect(page.locator("#forgeEnqueueDialog")).to_be_visible()
    page.locator("#forgeCancelBtn").click()

    page.goto(url + "/#mind")
    expect(page.locator("#subsystemTitle")).to_have_text("Mind")
    page.locator("#subsystemSettings").click()
    expect(page.locator('[data-category="models"]')).to_be_visible()
    page.keyboard.press("Escape")
    expect(page.locator("#tmPop")).to_be_visible()
    page.keyboard.press("Escape")
    expect(page.locator("#tmPop")).to_be_hidden()
    page.locator('.tm-label[data-station="guardian"]').click()
    expect(page.locator("#subsystemTitle")).to_have_text("Guardian")
    page.go_back()
    expect(page.locator("#tmPop")).to_be_hidden()
    page.go_back()
    expect(page.locator("#subsystemTitle")).to_have_text("Mind")
    page.go_forward()
    page.go_forward()
    expect(page.locator("#subsystemTitle")).to_have_text("Guardian")
    assert page.evaluate("location.hash") == "#guardian"

    if screenshots:
        page.screenshot(path=str(screenshots / "guardian-desktop.png"))
    # Spaces (classic dashboard) still routes into the popup, including card panels.
    page.goto(url + "/#classic")
    page.locator("#spacesOpen").click()
    page.locator('#spaceList [data-space="vision"]').click()
    expect(page.locator("#subsystemTitle")).to_have_text("Canvas")
    expect(page.locator("#canvasPanelCamera")).to_be_visible()
    assert page.locator("#pagePicker").count() == 0
    assert page.locator("#subsystemDirectory").count() == 0
    for width in (390, 820):
        page.set_viewport_size({"width": width, "height": 844})
        for subsystem in [*ids, "home"]:
            if subsystem == "home":
                page.locator('.brand').click()
            else:
                page.locator("#spacesTop").click()
                page.locator(f'#spaceList [data-space="{subsystem}"]').click()
            if subsystem == "home":
                expect(page.locator("#factoryWorkspace")).to_be_visible()
                expect(page.locator("#subsystemDialog")).to_be_hidden()
            else:
                expect(page.locator("#subsystemTitle")).to_have_text(subsystem.title())
                expect(page.locator("#tmPopClose")).to_be_visible()
            assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), page.evaluate("""({route:location.hash, width:innerWidth, scroll:document.documentElement.scrollWidth, wide:[...document.querySelectorAll('body *')].filter(el=>el.getBoundingClientRect().right>innerWidth+1).map(el=>[el.id,el.className,el.getBoundingClientRect().right]).slice(0,15)})""")
    if screenshots:
        page.set_viewport_size({"width": 390, "height": 844})
        page.evaluate("sailNavigate('classic')")
        page.screenshot(path=str(screenshots / "home-mobile.png"), full_page=True)
        page.evaluate("sailNavigate('guardian')")
        expect(page.locator("#toolsPanelSafetyReview")).to_be_visible()
        page.screenshot(path=str(screenshots / "guardian-mobile.png"))
    page.goto(url + "/#unknown")
    expect(page.locator("#factoryWorkspace")).to_be_visible()
    expect(page.locator("#subsystemDialog")).to_be_hidden()
    assert page.evaluate("location.hash") == "#factory"
    assert not errors, errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--screenshots", type=Path, help="Optional screenshot output directory")
    args = parser.parse_args()
    if args.screenshots:
        args.screenshots.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), UIHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            try:
                page = browser.new_page(
                    viewport={"width": 1440, "height": 1000}, reduced_motion="reduce"
                )
                check_pages(page, f"http://127.0.0.1:{server.server_port}", args.screenshots)
            finally:
                browser.close()
    finally:
        server.shutdown()
        server.server_close()
    print("Subsystem popups, classic dashboard, keyboard navigation, history, settings, card identity, Forge dialog, and mobile layouts passed")


if __name__ == "__main__":
    main()
