"""Regression test for the Terminal subsystem page and sidebar navigation.

Run with: python scripts/ui/terminal_page_browser_test.py
Requires Playwright (e.g. via .venv-voice).
"""

import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2] / "crates/sail-dialogue/web"


class UIHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def do_GET(self):
        if self.path.startswith("/vendor/") and "/xterm/" not in self.path:
            self.path = "/vendor/xterm/" + self.path.split("/")[-1]
        if self.path == "/icons.svg":
            self.send_response(200)
            self.send_header("Content-Type", "image/svg+xml")
            self.end_headers()
            self.wfile.write((ROOT.parents[2] / "assets/source/icons/sail-icons.svg").read_bytes())
        elif self.path.startswith("/api/"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{}")
        else:
            super().do_GET()

    def log_message(self, *_args):
        pass


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), UIHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1000})
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))

        # 1. Direct deep-link navigation to #terminal
        page.goto(f"http://127.0.0.1:{server.server_port}/#terminal")
        page.wait_for_timeout(400)
        assert page.evaluate("location.hash") == "#terminal"
        assert page.locator("#subsystemTitle").text_content() == "Terminal"
        assert page.locator("#tmPane #subsystemPage").is_visible()
        assert page.locator("#subsystemContent #terminalCard").is_visible()
        assert page.locator("#terminalContainer").is_visible()
        assert page.locator("#terminalRailBtn").get_attribute("aria-current") == "page"
        assert "selected" in page.locator("#terminalRailBtn").get_attribute("class")

        # 2. Return to home
        page.locator("#homeNav").click()
        page.wait_for_timeout(300)
        assert page.evaluate("location.hash") == "#home"
        assert not page.locator("#tmPane #subsystemPage").is_visible()
        assert not page.locator("#terminalCard").is_visible()

        # 3. Open Terminal via rail button
        page.locator("#terminalRailBtn").click()
        page.wait_for_timeout(400)
        assert page.evaluate("location.hash") == "#terminal"
        assert page.locator("#subsystemContent #terminalCard").is_visible()
        assert page.locator("#terminalContainer").is_visible()

        # 4. Open Terminal via shortcut (Alt+T)
        page.locator("#homeNav").click()
        page.wait_for_timeout(300)
        page.keyboard.press("Alt+t")
        page.wait_for_timeout(400)
        assert page.evaluate("location.hash") == "#terminal"
        assert page.locator("#subsystemContent #terminalCard").is_visible()

        # 5. Responsive layout check
        for width in [1440, 820, 390]:
            page.set_viewport_size({"width": width, "height": 844})
            page.wait_for_timeout(200)
            scroll_w = page.evaluate("document.documentElement.scrollWidth")
            inner_w = page.evaluate("innerWidth")
            assert scroll_w <= inner_w + 1, (
                f"Horizontal overflow at width {width}: {scroll_w} > {inner_w}"
            )

        assert not errors, f"Page errors: {errors}"
        print("PASS: Terminal page navigation, visibility, rail sync, shortcuts, and responsive layout")
        browser.close()

    server.shutdown()


if __name__ == "__main__":
    main()
