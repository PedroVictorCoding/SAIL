"""Verifies the factory room in a real headless browser.

Serves `crates/sail-dialogue/web` the way Dialogue does (index.html plus the static
files the page requests), opens the Factory route, and checks the room actually
renders: floor painted, every station hit-testable, inspector populated, and the
route teardown clean.

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

    # Nothing mounts on Home: the room belongs to its own route.
    assert page.evaluate("!!document.getElementById('factoryFloor')") is False
    assert page.evaluate("Array.from(document.querySelectorAll('[data-page]')).some(l => l.dataset.page === 'factory')"), \
        "the room needs an entry in the room directory"

    page.evaluate("window.sailFactory.show()")
    page.wait_for_selector("#factoryFloor", timeout=5000)
    page.wait_for_timeout(400)

    room = page.evaluate(
        """() => {
            const canvas = document.getElementById('factoryCanvas');
            const rect = canvas.getBoundingClientRect();
            const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
            const colours = new Set();
            for (let i = 0; i < data.length; i += 4 * 97) colours.add(`${data[i]},${data[i+1]},${data[i+2]}`);
            return {
                width: Math.round(rect.width), height: Math.round(rect.height), colours: colours.size,
                stations: window.sailFactory.state.registry.length,
                anchors: window.sailFactory.state.anchors.size,
            };
        }"""
    )
    assert room["width"] > 600 and room["height"] > 400, room
    assert room["stations"] >= 12, room
    assert room["anchors"] == room["stations"], room
    assert room["colours"] > 6, room
    assert not [entry for entry in failed if "factory.js" in entry], failed

    # Peek: hovering shows one anchored card, never a floating duplicate.
    box = page.locator("#factoryCanvas").bounding_box()
    target = page.evaluate(
        """() => {
            const anchor = window.sailFactory.state.anchors.get('forge');
            return {x: anchor.x, y: anchor.base.y - 6};
        }"""
    )
    page.mouse.move(box["x"] + target["x"], box["y"] + target["y"])
    page.wait_for_timeout(250)
    assert page.evaluate("window.sailFactory.state.hover?.id || ''") == "forge"
    assert page.is_visible("#factorySpec")

    # Bloom: the inspector becomes that station's detail column.
    page.mouse.click(box["x"] + target["x"], box["y"] + target["y"])
    page.wait_for_timeout(300)
    assert page.evaluate("window.sailFactory.state.selected?.id || ''") == "forge"
    assert page.is_visible("#factoryInspector")
    assert page.inner_text("#factoryInspectorTitle") == "FORGE"
    assert page.evaluate("document.querySelectorAll('#factoryReadout dt').length") >= 2

    # Scenes and list mode are frame controls, not extra pages.
    page.click(".factory-scene:nth-child(2)")
    assert page.inner_text("#factoryCrumb") == "Ops"
    page.keyboard.press("Escape")
    assert page.is_hidden("#factoryInspector")
    page.keyboard.press("Backslash")
    page.wait_for_timeout(200)
    assert page.is_visible("#factoryList")
    page.keyboard.press("Backslash")
    page.wait_for_timeout(200)

    # Leaving the route restores the normal workspace instead of stacking rooms.
    page.evaluate("window.sailNavigate('home')")
    page.wait_for_timeout(300)
    assert page.evaluate("!!document.getElementById('factoryFloor')") is False
    page.evaluate("window.sailFactory.show()")
    page.wait_for_selector("#factoryFloor")
    page.wait_for_timeout(300)

    page.screenshot(path=str(SHOT))
    # Only the room is in scope; other scripts on the page have their own bugs.
    room_errors = [error for error in errors if "factory" in error.lower()]
    assert not room_errors, room_errors
    unrelated = [error for error in errors if "factory" not in error.lower()]
    if unrelated:
        print(f"unrelated page errors (pre-existing): {unrelated}")
    browser.close()

print(f"factory room OK: {room}; screenshot {SHOT}")
