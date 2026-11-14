"""SAIL Agents > Messaging browser test (round 22, Antigravity).

Run the agents_studio example first (on 127.0.0.1:18809) with SAIL_AGENTS_DEMO_TOKEN set.
All round-22 gateway API responses are mocked via page.route() from scripts/ui/fixtures/gateway/*.json.

    python scripts/ui/gateway_browser_test.py [shots_dir]
"""
import copy
import json
import os
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

BASE = os.environ.get("SAIL_AGENTS_BASE_URL", "http://127.0.0.1:18809")
TOKEN = os.environ["SAIL_AGENTS_DEMO_TOKEN"]
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/r22-gateway-shots")
SHOTS.mkdir(parents=True, exist_ok=True)
ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "gateway"


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


def install_mocks(page):
    disconnected_tmpl = load_fixture("disconnected.json")
    connected_tmpl = load_fixture("connected.json")
    err_bad_token = load_fixture("error_bad_token.json")
    err_502 = load_fixture("error_502.json")
    err_lockout = load_fixture("error_lockout.json")

    state = {
        "gw": copy.deepcopy(disconnected_tmpl),
    }

    def handle_gateway(route):
        req = route.request
        path = req.url.split("?")[0].split("/api/agents/gateway", 1)[1]
        method = req.method

        if path in ("", "/") and method == "GET":
            return route.fulfill(status=200, json=state["gw"])

        if path == "/connect" and method == "POST":
            body = json.loads(req.post_data or "{}")
            tok = body.get("token", "")
            if tok == "bad-token":
                return route.fulfill(status=400, json=err_bad_token)
            if tok == "502-token":
                return route.fulfill(status=502, json=err_502)
            state["gw"] = copy.deepcopy(connected_tmpl)
            return route.fulfill(status=200, json={"status": state["gw"]["status"]})

        if path == "/disconnect" and method == "POST":
            state["gw"]["status"] = {
                "connected": False,
                "bot_username": None,
                "error": None,
                "last_poll_at": None,
            }
            return route.fulfill(status=200, json={"status": state["gw"]["status"]})

        if path == "/pair/approve" and method == "POST":
            body = json.loads(req.post_data or "{}")
            code = (body.get("code") or "").strip().upper()
            if code == "LOCKME":
                state["gw"]["pairing"]["locked_until"] = err_lockout["locked_until"]
                return route.fulfill(status=423, json=err_lockout)
            for idx, item in enumerate(state["gw"]["pairing"]["pending"]):
                if item["code"] == code:
                    state["gw"]["pairing"]["pending"].pop(idx)
                    paired = {
                        "sender_id": item["sender_id"],
                        "chat_id": item["chat_id"],
                        "name": item["name"],
                        "paired_at": 1794660200,
                    }
                    state["gw"]["pairing"]["paired"].append(paired)
                    return route.fulfill(status=200, json={"paired": paired})
            if code == "MANUAL99":
                paired = {
                    "sender_id": 3003,
                    "chat_id": 3003,
                    "name": "Carol",
                    "paired_at": 1794660250,
                }
                state["gw"]["pairing"]["paired"].append(paired)
                return route.fulfill(status=200, json={"paired": paired})
            return route.fulfill(status=400, json={"error": "Code not found or expired."})

        if path == "/pair/deny" and method == "POST":
            body = json.loads(req.post_data or "{}")
            code = (body.get("code") or "").strip().upper()
            state["gw"]["pairing"]["pending"] = [
                p for p in state["gw"]["pairing"]["pending"] if p["code"] != code
            ]
            return route.fulfill(status=200, json={"ok": True})

        if path == "/pair/revoke" and method == "POST":
            body = json.loads(req.post_data or "{}")
            sid = body.get("sender_id")
            state["gw"]["pairing"]["paired"] = [
                p for p in state["gw"]["pairing"]["paired"] if p["sender_id"] != sid
            ]
            return route.fulfill(status=200, json={"ok": True})

        if path == "/policy" and method == "POST":
            body = json.loads(req.post_data or "{}")
            state["gw"]["pairing"]["policy"] = body["policy"]
            return route.fulfill(status=200, json={"ok": True})

        if path.startswith("/chats/") and path.endswith("/new") and method == "POST":
            cid = int(path.split("/")[2])
            for c in state["gw"]["chats"]:
                if c["chat_id"] == cid:
                    c["turns"] = 0
            return route.fulfill(status=200, json={"ok": True})

        if path == "/outbox/retry" and method == "POST":
            state["gw"]["outbox"] = {"queued": 0, "failed_recently": 0}
            return route.fulfill(status=200, json={"ok": True})

        return route.fulfill(status=404, json={"error": f"Unmocked route: {method} {path}"})

    page.route("**/api/agents/gateway", handle_gateway)
    page.route("**/api/agents/gateway/**", handle_gateway)


with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    for width in (1440, 768, 390, 360):
        page = browser.new_page(viewport={"width": width, "height": 1000}, reduced_motion="reduce")
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.add_init_script(
            f"localStorage.setItem('sail_owner_token', {json.dumps(TOKEN)}); localStorage.removeItem('sail.agents.section');"
        )
        route_static_assets(page)
        install_mocks(page)

        # Open Messaging directly via #agents?section=messaging
        page.goto(BASE + "/#agents?section=messaging")
        expect(page.locator("#subsystemContent #gatewayCard")).to_be_visible()
        expect(page.locator("#gatewayConnPill")).to_have_text("Not connected")
        expect(page.locator("#gatewaySetup")).to_be_visible()
        expect(page.locator("#gatewayPendingList")).to_contain_text("No pending pairing requests.")
        expect(page.locator("#gatewayPairedList")).to_contain_text("No paired contacts yet.")
        expect(page.locator("#gatewayChatsList")).to_contain_text("No Telegram conversations yet.")

        # Bad token + 502 error states; token is password-masked and never echoed.
        expect(page.locator("#gatewayTokenInput")).to_have_attribute("type", "password")
        page.locator("#gatewayTokenInput").fill("bad-token")
        page.locator("#gatewayConnectBtn").click()
        expect(page.locator("#gatewayConnectError")).to_have_text("Telegram rejected that token (Unauthorized).")

        page.locator("#gatewayTokenInput").fill("502-token")
        page.locator("#gatewayConnectBtn").click()
        expect(page.locator("#gatewayConnectError")).to_have_text("Telegram API unreachable (502 Bad Gateway).")

        # Connect with valid token; verify @username shown and token never echoed.
        secret_tok = "999999:SECRET_BOT_TOKEN_DO_NOT_ECHO_12345"
        page.locator("#gatewayTokenInput").fill(secret_tok)
        page.locator("#gatewayConnectBtn").click()
        expect(page.locator("#gatewayConnPill")).to_have_text("Connected as @sail_local_bot")
        expect(page.locator("#gatewaySetup")).to_be_hidden()
        assert secret_tok not in page.locator("#gatewayCard").inner_text()

        if width in (1440, 390):
            page.screenshot(path=str(SHOTS / f"gateway-{width}.png"), full_page=True)

        # Approve pending sender Bob via card button, then type manual code and lockout.
        bob = page.locator('[data-pending-code="ABCD2345"]')
        expect(bob).to_contain_text("Bob")
        bob.get_by_role("button", name="Approve").click()
        expect(page.locator('[data-paired-id="2002"]')).to_contain_text("Bob")

        # Manual code entry + lockout error
        page.locator("#gatewayCodeInput").fill("LOCKME")
        page.locator("#gatewayCodeSubmit").click()
        expect(page.locator("#gatewayCodeError")).to_contain_text("Too many wrong pairing codes.")
        expect(page.locator("#gatewayLockout")).to_be_visible()

        page.locator("#gatewayCodeInput").fill("MANUAL99")
        page.locator("#gatewayCodeSubmit").click()
        expect(page.locator('[data-paired-id="3003"]')).to_contain_text("Carol")

        # Revoke Carol with two-step confirm
        carol = page.locator('[data-paired-id="3003"]')
        carol.get_by_role("button", name="Revoke").click()
        carol.get_by_role("button", name="Confirm revoke").click()
        expect(page.locator('[data-paired-id="3003"]')).to_have_count(0)

        # Policy segmented control
        page.locator('.gateway-seg-btn[data-policy="decline"]').click()
        expect(page.locator('.gateway-seg-btn[data-policy="decline"]')).to_have_attribute("aria-pressed", "true")

        # Chat reset (/new)
        alice_chat = page.locator('[data-chat-id="1001"]')
        expect(alice_chat).to_contain_text("4 turns")
        alice_chat.get_by_role("button", name="Start a new conversation").click()
        expect(alice_chat).to_contain_text("0 turns")

        # Outbox retry
        expect(page.locator("#gatewayOutboxQueued")).to_have_text("2")
        expect(page.locator("#gatewayOutboxFailed")).to_have_text("1")
        page.locator("#gatewayRetryOutbox").click()
        expect(page.locator("#gatewayOutboxQueued")).to_have_text("0")
        expect(page.locator("#gatewayOutboxFailed")).to_have_text("0")

        # Disconnect with two-step confirm
        page.locator("#gatewayDisconnectWrap").get_by_role("button", name="Disconnect").click()
        page.locator("#gatewayDisconnectWrap").get_by_role("button", name="Confirm disconnect").click()
        expect(page.locator("#gatewayConnPill")).to_have_text("Not connected")
        expect(page.locator("#gatewaySetup")).to_be_visible()

        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), f"horizontal overflow at {width}px"

        # Forced colors visibility
        page.emulate_media(forced_colors="active")
        expect(page.locator("#gatewayRefresh")).to_be_visible()
        expect(page.locator("#gatewayConnectBtn")).to_be_visible()
        expect(page.locator('.gateway-seg-btn[data-policy="pair"]')).to_be_visible()

        assert not errors, errors
        page.close()
        print(f"PASS {width}px: gateway setup, pairing, policy, chats, outbox, errors, forced-colors")

    browser.close()
