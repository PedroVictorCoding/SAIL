"""SAIL Mesh > Worker PCs browser test (round 23, Antigravity).

Run the agents_studio example first (on 127.0.0.1:18809) with SAIL_AGENTS_DEMO_TOKEN set.
All /api/mind/mesh/** responses are mocked via page.route() from scripts/ui/fixtures/mesh/*.json.

    python scripts/ui/mesh_browser_test.py [shots_dir]
"""
import copy
import functools
import http.server
import json
import os
import socketserver
import sys
import threading
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

TOKEN = os.environ.get("SAIL_AGENTS_DEMO_TOKEN", "test-owner-token-0123456789abcdef0123456789abcdef")
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/r29-mesh-shots")
SHOTS.mkdir(parents=True, exist_ok=True)
ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "mesh"
COMPUTE_FIXTURES = Path(__file__).resolve().parent / "fixtures" / "mesh_compute"


def load_fixture(name):
    return json.loads((FIXTURES / name).read_text())


def load_compute_fixture(name):
    return json.loads((COMPUTE_FIXTURES / name).read_text())


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass


def start_static_server():
    web = ROOT / "crates" / "sail-dialogue" / "web"
    handler = functools.partial(_QuietHandler, directory=str(web))
    httpd = socketserver.TCPServer(("127.0.0.1", 0), handler)
    port = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, port


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
        page.route(
            f"**{url_path}",
            (lambda fp, ct: lambda r: r.fulfill(body=fp.read_bytes(), content_type=ct))(
                file_path, ctype
            ),
        )


def install_mocks(page):
    empty_tmpl = load_fixture("empty.json")
    mode_off_tmpl = load_fixture("mode_off.json")
    three_tmpl = load_fixture("three_workers.json")
    resting_tmpl = load_fixture("resting_workers.json")
    pair_200_tmpl = load_fixture("pair_200.json")
    err_400 = load_fixture("error_400.json")
    err_502 = load_fixture("error_502.json")
    err_401 = load_fixture("error_401.json")
    err_503 = load_fixture("error_503.json")

    state = {
        "mesh": copy.deepcopy(empty_tmpl),
        "mode": "normal",  # normal | 401 | 503 | abort
        "requests": [],
    }

    def handle_escalation(route):
        return route.fulfill(status=404, json={"error": "not running"})

    def handle_token(route):
        return route.fulfill(status=200, json={"token": TOKEN})

    def handle_mesh(route):
        req = route.request
        auth = req.headers.get("authorization", "")
        assert auth == f"Bearer {TOKEN}", f"Missing/wrong auth header: {auth!r}"

        raw_path = req.url.split("?")[0].split("/api/mind/mesh", 1)[1]
        method = req.method
        body_text = req.post_data or ""
        state["requests"].append({"method": method, "path": raw_path, "body": body_text})

        if state["mode"] == "abort":
            return route.abort("failed")
        if state["mode"] == "401":
            return route.fulfill(status=401, json=err_401)
        if state["mode"] == "503":
            return route.fulfill(status=503, json=err_503)

        if raw_path in ("", "/") and method == "GET":
            return route.fulfill(status=200, json=state["mesh"])

        if raw_path == "/refresh" and method == "POST":
            assert body_text == "", f"POST /refresh must send no body, got {body_text!r}"
            return route.fulfill(status=200, json=state["mesh"])

        if raw_path == "/pair" and method == "POST":
            payload = json.loads(body_text)
            assert set(payload.keys()) <= {"code", "serves"}, f"Unexpected pair keys: {payload}"
            assert isinstance(payload.get("code"), str)
            if "serves" in payload:
                assert isinstance(payload["serves"], list)
            code = payload["code"]
            if code == "bad-code":
                return route.fulfill(status=400, json=err_400)
            if code == "sailmesh://192.168.1.77:7710/?name=gaming-pc&fp=abcd&key=unverified":
                return route.fulfill(status=502, json=err_502)
            resp = copy.deepcopy(pair_200_tmpl)
            state["mesh"]["workers"].append(copy.deepcopy(resp["worker"]))
            return route.fulfill(status=200, json=resp)

        if raw_path.startswith("/workers/") and raw_path.endswith("/enabled") and method == "POST":
            name = raw_path[len("/workers/") : -len("/enabled")]
            payload = json.loads(body_text)
            assert set(payload.keys()) == {"enabled"}, f"Unexpected enabled body: {payload}"
            assert isinstance(payload["enabled"], bool)
            for w in state["mesh"]["workers"]:
                if w["name"] == name:
                    w["state"] = "ready" if payload["enabled"] else "disabled"
                    return route.fulfill(status=200, json={"worker": copy.deepcopy(w)})
            return route.fulfill(status=404, json={"error": f"no worker named {name}"})

        if raw_path.startswith("/workers/") and raw_path.endswith("/lend") and method == "POST":
            name = raw_path[len("/workers/") : -len("/lend")]
            payload = json.loads(body_text)
            assert "mode" in payload and isinstance(payload["mode"], dict)
            for w in state["mesh"]["workers"]:
                if w["name"] == name:
                    if not w.get("compute"):
                        return route.fulfill(status=409, json={"error": "no_compute"})
                    w["compute"]["mode"] = payload["mode"]
                    mode_tag = payload["mode"]["mode"]
                    if mode_tag == "pause":
                        w["compute"]["state"] = {"state": "paused"}
                    elif mode_tag == "play":
                        w["compute"]["state"] = {"state": "lending"}
                    else:
                        w["compute"]["state"] = {"state": "lending"}
                    return route.fulfill(status=200, json={"worker": copy.deepcopy(w)})
            return route.fulfill(status=404, json={"error": f"no worker named {name}"})

        if raw_path.startswith("/workers/") and method == "DELETE":
            name = raw_path[len("/workers/") :]
            before = len(state["mesh"]["workers"])
            state["mesh"]["workers"] = [w for w in state["mesh"]["workers"] if w["name"] != name]
            if len(state["mesh"]["workers"]) == before:
                return route.fulfill(status=404, json={"error": f"no worker named {name}"})
            return route.fulfill(status=204, body="")

        return route.fulfill(status=404, json={"error": f"Unmocked route: {method} {raw_path}"})

    page.route("**/api/**", lambda r: r.fulfill(status=200, json={}))
    page.route("**/voice-assets/manifest.json", lambda r: r.fulfill(status=200, json={}))
    page.route("**/mermaid.js", lambda r: r.fulfill(status=200, content_type="text/javascript", body=b""))
    page.route("**/api/mind/escalation/status", handle_escalation)
    page.route("**/api/guardian/token", handle_token)
    page.route("**/api/mind/mesh", handle_mesh)
    page.route("**/api/mind/mesh/**", handle_mesh)
    return state, mode_off_tmpl, three_tmpl, resting_tmpl


def open_models_settings(page):
    page.evaluate("window.sailOpenSettings('models')")
    expect(page.locator("#settingsPanel-models")).to_be_visible()
    expect(page.locator("#meshWorkers")).to_be_visible()


_httpd = None
BASE = os.environ.get("SAIL_AGENTS_BASE_URL")
if not BASE:
    _httpd, _port = start_static_server()
    BASE = f"http://127.0.0.1:{_port}"


with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    for width in (1440, 768, 390, 360):
        page = browser.new_page(viewport={"width": width, "height": 1000}, reduced_motion="reduce")
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.add_init_script(f"localStorage.setItem('sail_owner_token', {json.dumps(TOKEN)});")
        route_static_assets(page)
        state, mode_off_tmpl, three_tmpl, resting_tmpl = install_mocks(page)

        page.goto(BASE + "/")
        open_models_settings(page)

        # 1. Empty state (no workers, offload mode)
        expect(page.locator("#meshModeLine")).to_have_text("Background work goes to a free worker first")
        expect(page.locator("#meshTotalsLine")).to_have_text("0 requests ran on workers · 0 fell back to this machine")
        expect(page.locator("#meshEmptyState")).to_contain_text("sail-mesh-worker init")
        expect(page.locator("#meshEmptyState")).to_contain_text("sail-mesh-worker serve --llama-server <path> --model <gguf>")

        # 2. Mode off
        state["mesh"] = copy.deepcopy(mode_off_tmpl)
        page.locator("#meshRefreshBtn").click()
        expect(page.locator("#meshModeLine")).to_have_text("Off — everything runs on this machine (set SAIL_MESH)")
        expect(page.locator("#meshTotalsLine")).to_have_text("4 requests ran on workers · 2 fell back to this machine")

        # 3. Three workers (ready with tools+vision, unreachable with error, disabled)
        state["mesh"] = copy.deepcopy(three_tmpl)
        page.locator("#meshRefreshBtn").click()
        expect(page.locator(".mesh-worker-row")).to_have_count(3)

        desktop = page.locator('.mesh-worker-row[data-worker-name="desktop"]')
        expect(desktop).to_contain_text("192.168.1.40:7710")
        expect(desktop).to_contain_text("windows x86_64")
        expect(desktop.locator(".mesh-pill")).to_have_text("Ready")
        expect(desktop).to_contain_text("Qwen3.6-35B-A3B-Q4_K_M.gguf")
        expect(desktop).to_contain_text("32k ctx")
        expect(desktop).to_contain_text("Tools")
        expect(desktop).to_contain_text("Vision")
        expect(desktop).to_contain_text("running 1 of 2")
        expect(desktop).to_contain_text("11 completed · 1 failed")
        expect(desktop).to_contain_text("4 ms")

        laptop = page.locator('.mesh-worker-row[data-worker-name="laptop"]')
        expect(laptop.locator(".mesh-pill")).to_have_text("Unreachable")
        expect(laptop).to_contain_text("not reachable (connection refused)")

        workstation = page.locator('.mesh-worker-row[data-worker-name="workstation"]')
        expect(workstation.locator(".mesh-pill")).to_have_text("Off")
        expect(workstation.get_by_role("button", name="Turn on")).to_be_visible()

        if width in (1440, 390):
            page.screenshot(path=str(SHOTS / f"mesh-{width}.png"), full_page=True)

        # 3b. Workers resting while their owners use the PC (full rest and a backoff)
        state["mesh"] = copy.deepcopy(resting_tmpl)
        page.locator("#meshRefreshBtn").click()
        expect(page.locator(".mesh-worker-row")).to_have_count(2)
        gamer = page.locator('.mesh-worker-row[data-worker-name="windows"]')
        expect(gamer.locator(".mesh-pill")).to_have_text("Resting")
        expect(gamer.locator(".mesh-worker-rest")).to_have_text(
            "Stepped aside: gaming (full-screen game) · back when it ends"
        )
        studio = page.locator('.mesh-worker-row[data-worker-name="studio"]')
        expect(studio.locator(".mesh-worker-rest")).to_have_text(
            "Stepped aside: other programs use 72% CPU · checks again in 4 min"
        )
        if width in (1440, 390):
            page.screenshot(path=str(SHOTS / f"mesh-resting-{width}.png"), full_page=True)
        state["mesh"] = copy.deepcopy(three_tmpl)
        page.locator("#meshRefreshBtn").click()
        expect(page.locator(".mesh-worker-row")).to_have_count(3)

        # 4. Pairing: password masking, show toggle, 400, 502, 200, secret never persisted
        expect(page.locator("#meshCodeInput")).to_have_attribute("type", "password")
        page.locator("#meshCodeToggle").click()
        expect(page.locator("#meshCodeInput")).to_have_attribute("type", "text")
        page.locator("#meshCodeToggle").click()
        expect(page.locator("#meshCodeInput")).to_have_attribute("type", "password")

        page.locator("#meshCodeInput").fill("bad-code")
        page.locator("#meshPairBtn").click()
        expect(page.locator("#meshPairError")).to_have_text("malformed pairing code: expected sailmesh:// scheme")

        page.locator("#meshCodeInput").fill("sailmesh://192.168.1.77:7710/?name=gaming-pc&fp=abcd&key=unverified")
        page.locator("#meshPairBtn").click()
        expect(page.locator("#meshPairError")).to_have_text("could not verify worker gaming-pc: connection refused")

        secret_code = "sailmesh://192.168.1.77:7710/?name=gaming-pc&fp=0123456789abcdef&key=SECRET_MESH_KEY_DO_NOT_LEAK_999"
        page.locator("#meshCodeInput").fill(secret_code)
        page.locator("#meshServesInput").fill("qwen, llama")
        page.locator("#meshPairBtn").click()

        gaming = page.locator('.mesh-worker-row[data-worker-name="gaming-pc"]')
        expect(gaming).to_be_visible()
        expect(gaming.locator(".mesh-pill")).to_have_text("Ready")
        expect(page.locator("#meshCodeInput")).to_have_value("")
        expect(page.locator("#meshServesInput")).to_have_value("")

        pair_reqs = [r for r in state["requests"] if r["method"] == "POST" and r["path"] == "/pair"]
        assert json.loads(pair_reqs[-1]["body"]) == {"code": secret_code, "serves": ["qwen", "llama"]}

        # Secret must never appear in localStorage, sessionStorage, URL, or DOM
        leaked = page.evaluate(
            """(secret) => {
                const inLS = Object.values(localStorage).some(v => String(v).includes(secret));
                const inSS = Object.values(sessionStorage).some(v => String(v).includes(secret));
                const inURL = location.href.includes(secret);
                const inDOM = document.body.innerText.includes(secret);
                return inLS || inSS || inURL || inDOM;
            }""",
            secret_code,
        )
        assert not leaked, "Pairing secret leaked into storage, URL, or DOM"

        # 5. Enabled toggle (Turn off / Turn on)
        desktop.get_by_role("button", name="Turn off").click()
        expect(desktop.locator(".mesh-pill")).to_have_text("Off")
        expect(desktop.get_by_role("button", name="Turn on")).to_be_visible()
        toggle_reqs = [r for r in state["requests"] if r["path"] == "/workers/desktop/enabled"]
        assert json.loads(toggle_reqs[-1]["body"]) == {"enabled": False}

        workstation.get_by_role("button", name="Turn on").click()
        expect(workstation.locator(".mesh-pill")).to_have_text("Ready")
        toggle_reqs_ws = [r for r in state["requests"] if r["path"] == "/workers/workstation/enabled"]
        assert json.loads(toggle_reqs_ws[-1]["body"]) == {"enabled": True}

        # 6. Forget with inline confirm (204)
        laptop.get_by_role("button", name="Forget").click()
        expect(laptop).to_contain_text("Forget laptop?")
        laptop.get_by_role("button", name="Confirm forget").click()
        expect(page.locator('.mesh-worker-row[data-worker-name="laptop"]')).to_have_count(0)

        # 7. Network failure keeps last data and shows banner
        state["mode"] = "abort"
        page.locator("#meshRefreshBtn").click()
        expect(page.locator("#meshBanner")).to_have_text("Couldn't reach SAIL — showing the last known state")
        expect(page.locator('.mesh-worker-row[data-worker-name="desktop"]')).to_be_visible()

        # 8. 401 and 503 error states
        state["mode"] = "401"
        page.locator("#meshRefreshBtn").click()
        expect(page.locator("#meshBanner")).to_have_text("Owner sign-in needed")

        state["mode"] = "503"
        page.locator("#meshRefreshBtn").click()
        expect(page.locator("#meshBanner")).to_have_text("Mesh is not available in this SAIL process")

        # 9. Compute card & heavy model fixtures (round 29 G2)
        state["mode"] = "normal"
        for fname in (
            "lending.json",
            "loading.json",
            "paused.json",
            "outside_window.json",
            "resting_full.json",
            "resting_busy.json",
            "unavailable.json",
            "no_compute.json",
            "heavy_running.json",
            "heavy_stopped.json",
        ):
            state["mesh"] = load_compute_fixture(fname)
            page.locator("#meshRefreshBtn").click()
            if fname == "no_compute.json":
                expect(page.locator(".mesh-compute-card")).to_have_count(0)
            else:
                expect(page.locator(".mesh-compute-card")).to_be_visible()
            if fname == "lending.json":
                expect(page.locator("#meshWorkers")).to_contain_text("Hibernated apps: chrome.exe")
                page.locator("#meshWorkers").get_by_role("button", name="Pause").click()
                expect(page.locator("#meshWorkers")).to_contain_text("Paused by you")
                page.locator("#meshWorkers").get_by_role("button", name="Lend 1 h").click()
                expect(page.locator("#meshWorkers")).to_contain_text("Lending")
                page.locator("#meshWorkers").get_by_role("button", name="Automatic").click()
            if fname == "heavy_running.json":
                expect(page.locator("#meshHeavyCard")).to_contain_text("gpt-oss-120b-MXFP4")
                expect(page.locator("#meshHeavyCard")).to_contain_text("7.4 tok/s")
                expect(page.locator("#meshHeavyCard")).to_contain_text("Night Shift: Arena course outline")
            if fname == "heavy_stopped.json":
                expect(page.locator("#meshHeavyCard")).to_contain_text("Stopped — PC not lending")
            assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), f"overflow on {fname} at {width}px"

        # Restore normal 3-worker state for layout/overflow & forced-colors checks
        state["mode"] = "normal"
        state["mesh"] = copy.deepcopy(three_tmpl)
        page.locator("#meshRefreshBtn").click()
        expect(page.locator(".mesh-worker-row")).to_have_count(3)

        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), f"horizontal overflow at {width}px"
        assert page.evaluate(
            "(() => { const el = document.getElementById('meshWorkers'); return el.scrollWidth <= el.clientWidth + 1; })()"
        ), f"#meshWorkers overflow at {width}px"

        # Forced colors visibility
        page.emulate_media(forced_colors="active")
        expect(page.locator("#meshRefreshBtn")).to_be_visible()
        expect(page.locator("#meshPairBtn")).to_be_visible()
        expect(page.locator('.mesh-worker-row[data-worker-name="desktop"] .mesh-pill')).to_be_visible()

        assert not errors, errors
        page.close()
        print(f"PASS {width}px: empty, mode_off, 3 workers, pair 400/502/200, toggle, forget, network/401/503, forced-colors")

    browser.close()
