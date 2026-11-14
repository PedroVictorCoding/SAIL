"""Memory > Review browser test (round 22, Antigravity).

Run the agents_studio example first (on 127.0.0.1:18809) with SAIL_AGENTS_DEMO_TOKEN set.
All round-22 memory review API responses are mocked via page.route() from scripts/ui/fixtures/memory/*.json.

    python scripts/ui/memory_review_browser_test.py [shots_dir]
"""
import copy
import json
import os
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

BASE = os.environ.get("SAIL_AGENTS_BASE_URL", "http://127.0.0.1:18809")
TOKEN = os.environ["SAIL_AGENTS_DEMO_TOKEN"]
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/r22-memory-shots")
SHOTS.mkdir(parents=True, exist_ok=True)
ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "memory"


def load_fixture(name):
    return json.loads((FIXTURES / name).read_text())


def route_static_assets(page):
    web = ROOT / "crates" / "sail-dialogue" / "web"
    page.route(
        "**/icons.svg",
        lambda r: r.fulfill(
            body=(ROOT / "assets/source/icons/sail-icons.svg").read_bytes(),
            content_type="image/svg+xml",
        ),
    )
    aliases = {
        "/vendor/xterm.js": (web / "vendor/xterm/xterm.js", "text/javascript"),
        "/vendor/addon-fit.js": (web / "vendor/xterm/addon-fit.js", "text/javascript"),
        "/vendor/xterm.css": (web / "vendor/xterm/xterm.css", "text/css"),
        "/vendor/paper-shaders.js": (web / "vendor/paper-shaders/paper-shaders.js", "text/javascript"),
        "/vendor/liquidglass.js": (web / "vendor/liquidglass/liquidglass.js", "text/javascript"),
    }
    for url_path, (file_path, ctype) in aliases.items():
        page.route(f"**{url_path}", (lambda fp, ct: lambda r: r.fulfill(body=fp.read_bytes(), content_type=ct))(file_path, ctype))


def install_mocks(page, empty_mode=False):
    review_tmpl = load_fixture("empty.json" if empty_mode else "review.json")
    conflict_err = load_fixture("save_conflict.json")

    state = {
        "review": copy.deepcopy(review_tmpl),
    }

    def handle_review(route):
        req = route.request
        path = req.url.split("?")[0].split("/api/memory/review", 1)[1]
        method = req.method

        if path in ("", "/") and method == "GET":
            return route.fulfill(status=200, json=state["review"])

        if path == "/enabled" and method == "POST":
            body = json.loads(req.post_data or "{}")
            state["review"]["enabled"] = bool(body.get("enabled"))
            return route.fulfill(status=200, json={"enabled": state["review"]["enabled"]})

        if path == "/pin" and method == "POST":
            body = json.loads(req.post_data or "{}")
            cid = body.get("claim_id")
            pinned = bool(body.get("pinned"))
            for line in state["review"]["profile"]["lines"]:
                if line["claim_id"] == cid:
                    line["pinned"] = pinned
                    line["why"] = "pinned" if pinned else "verified"
            return route.fulfill(status=200, json={"ok": True})

        parts = [p for p in path.split("/") if p]
        if len(parts) == 2 and method == "POST":
            item_id, action = parts
            if action == "save" and item_id == "0123456789abcdef0123456789abcdef":
                return route.fulfill(status=409, json=conflict_err)
            if action in ("save", "discard"):
                state["review"]["pending"] = [
                    p for p in state["review"]["pending"] if p["id"] != item_id
                ]
                return route.fulfill(status=200, json={"ok": True})

        return route.fulfill(status=404, json={"error": f"Unmocked route: {method} {path}"})

    page.route("**/api/memory/review", handle_review)
    page.route("**/api/memory/review/**", handle_review)
    # Keep existing Memory tab overview call quiet if triggered
    page.route("**/api/memory/read", lambda r: r.fulfill(status=200, json={"generation": 1, "entities": [], "history": []}))


with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    for width in (1440, 768, 390, 360):
        page = browser.new_page(viewport={"width": width, "height": 1000}, reduced_motion="reduce")
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.add_init_script(f"localStorage.setItem('sail_owner_token', {json.dumps(TOKEN)});")
        route_static_assets(page)
        install_mocks(page, empty_mode=False)

        page.goto(BASE + "/#memory")
        expect(page.locator("#memoryReviewTab")).to_be_visible()
        page.locator("#memoryReviewTab").click()
        expect(page.locator("#memoryPanelReview")).to_be_visible()

        # Toggle off by default -> turn on
        toggle = page.locator("#mrEnabledToggle")
        expect(toggle).not_to_be_checked()
        toggle.check()
        expect(toggle).to_be_checked()

        # Profile preview & Pin/Unpin
        expect(page.locator("#mrProfileSize")).to_have_text("112 / 1,400 characters")
        expect(page.locator("#mrProfilePreview")).to_contain_text("Prefers replies in Portuguese")
        line102 = page.locator('[data-claim-id="102"]')
        expect(line102).to_contain_text("verified")
        line102.get_by_role("button", name="Pin").click()
        expect(line102).to_contain_text("pinned")
        expect(line102.get_by_role("button", name="Unpin")).to_be_visible()

        # Pending list + scan findings + 409 error on save + discard
        flagged = page.locator('[data-pending-id="0123456789abcdef0123456789abcdef"]')
        expect(flagged).to_contain_text("Flagged by memory safety scan:")
        expect(flagged).to_contain_text("prompt_injection")

        if width in (1440, 390):
            page.screenshot(path=str(SHOTS / f"memory-review-{width}.png"), full_page=True)

        flagged.get_by_role("button", name="Save").click()
        expect(flagged).to_contain_text("Validation rejected: conflicting claim was superseded.")

        # Save clean item
        clean = page.locator('[data-pending-id="fedcba9876543210fedcba9876543210"]')
        clean.get_by_role("button", name="Save").click()
        expect(page.locator('[data-pending-id="fedcba9876543210fedcba9876543210"]')).to_have_count(0)

        # Discard all with two-step confirm
        page.locator("#mrBulkActions").get_by_role("button", name="Discard all").click()
        page.locator("#mrBulkActions").get_by_role("button", name="Confirm discard all (1)").click()
        expect(page.locator("#mrPendingList")).to_contain_text("No memory changes waiting for review.")

        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), f"horizontal overflow at {width}px"

        # Forced colors visibility
        page.emulate_media(forced_colors="active")
        expect(page.locator("#mrEnabledToggle")).to_be_visible()
        expect(page.locator("#mrRefresh")).to_be_visible()

        assert not errors, errors
        page.close()

        # Also check empty state at 1440px
        if width == 1440:
            empty_page = browser.new_page(viewport={"width": 1440, "height": 1000}, reduced_motion="reduce")
            empty_errors = []
            empty_page.on("pageerror", lambda e: empty_errors.append(str(e)))
            empty_page.add_init_script(f"localStorage.setItem('sail_owner_token', {json.dumps(TOKEN)});")
            route_static_assets(empty_page)
            install_mocks(empty_page, empty_mode=True)
            empty_page.goto(BASE + "/#memory")
            empty_page.locator("#memoryReviewTab").click()
            expect(empty_page.locator("#mrProfileSize")).to_have_text("0 / 1,400 characters")
            expect(empty_page.locator("#mrProfileLines")).to_contain_text("No profile lines in context yet.")
            expect(empty_page.locator("#mrPendingList")).to_contain_text("No memory changes waiting for review.")
            assert not empty_errors, empty_errors
            empty_page.close()

        print(f"PASS {width}px: memory review toggle, profile pin/unpin, findings, 409, discard all, forced-colors")

    browser.close()
