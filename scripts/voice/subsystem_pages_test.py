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
    assert set(ids) == registered
    assert page.locator("#subsystemNav a").count() == len(registered)
    page.evaluate("""window.originalCards = [...document.querySelectorAll('.workspace-grid > article')];""")
    for subsystem in ids:
        link = page.locator(f'#subsystemNav a[href="#{subsystem}"]')
        link.focus()
        page.keyboard.press("Enter")
        expect(page.locator("#subsystemPage")).to_be_visible()
        expect(page.locator("#homePage")).to_be_hidden()
        expect(page.locator("#subsystemTitle")).to_have_text(subsystem.title())
        assert page.evaluate("document.activeElement.id") == "subsystemTitle"
        assert page.locator('#subsystemNav [aria-current="page"]').count() == 1
        assert page.evaluate("""originalCards.every(card =>
          document.querySelectorAll('#' + card.id).length === 1 &&
          document.getElementById(card.id) === card)""")
        if subsystem in {"arena", "flow", "discovery"}:
            expect(page.locator("#subsystemNotice")).to_be_visible()
        else:
            expect(page.locator("#subsystemContent .card-tab-panel.active")).to_be_visible()

    page.locator("#homeNav").click()
    expect(page.locator("#homePage")).to_be_visible()
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
    page.locator("#spacesOpen").click()
    page.locator('#spaceList [data-space="guardian"]').click()
    expect(page.locator("#subsystemTitle")).to_have_text("Guardian")
    page.go_back()
    expect(page.locator("#subsystemTitle")).to_have_text("Mind")
    page.go_forward()
    expect(page.locator("#subsystemTitle")).to_have_text("Guardian")
    page.locator("#spacesOpen").click()
    page.keyboard.press("Escape")
    assert page.evaluate("location.hash") == "#guardian"

    if screenshots:
        expect(page.locator("#scrim")).to_be_hidden()
        page.screenshot(path=str(screenshots / "guardian-desktop.png"), full_page=True)
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
                expect(page.locator("#homePage")).to_be_visible()
            else:
                expect(page.locator("#subsystemTitle")).to_have_text(subsystem.title())
            assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), page.evaluate("""({route:location.hash, width:innerWidth, scroll:document.documentElement.scrollWidth, wide:[...document.querySelectorAll('body *')].filter(el=>el.getBoundingClientRect().right>innerWidth+1).map(el=>[el.id,el.className,el.getBoundingClientRect().right]).slice(0,15)})""")
    if screenshots:
        page.set_viewport_size({"width": 390, "height": 844})
        page.locator('.brand').click()
        page.screenshot(path=str(screenshots / "home-mobile.png"), full_page=True)
        page.locator("#spacesTop").click()
        page.locator('#spaceList [data-space="guardian"]').click()
        expect(page.locator("#toolsPanelSafetyReview")).to_be_visible()
        page.screenshot(path=str(screenshots / "guardian-mobile.png"), full_page=True)
    page.goto(url + "/#unknown")
    expect(page.locator("#homePage")).to_be_visible()
    assert page.evaluate("location.hash") == "#home"
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
    print("Subsystem routes, keyboard navigation, history, settings, card identity, Forge dialog, and mobile layouts passed")


if __name__ == "__main__":
    main()
