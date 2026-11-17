"""Fixture-based Capital round 25 B7 UI regression: tax profile and accumulating funds.

Checks, at 1440/768/390/360 px with no page or console errors and no horizontal overflow:
- the tax profile loads from GET /api/capital/dividends/tax and fills the form;
- englobamento shows the marginal-rate field, and Save POSTs the right JSON, then reloads
  the projection;
- an out-of-range marginal rate is refused before any request;
- accumulating funds show their 4% income equivalent with server text as plain text.

Run with: /home/main/SAIL/.venv-voice/bin/python scripts/ui/capital_tax_browser_test.py
"""
import copy
import http.server
import json
from pathlib import Path
import threading
from urllib.parse import urlparse

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[2] / "crates/sail-dialogue/web"
FIXTURES_R24 = Path(__file__).resolve().parent / "fixtures/capital_r24"
TOKEN = "capital-tax-ui-owner-token-at-least-32-bytes"
WIDTHS = (1440, 768, 390, 360)
HOSTILE = "<img src=x onerror=alert(1)>"


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def log_message(self, *_):
        pass


def load_json(name):
    return json.loads((FIXTURES_R24 / name).read_text(encoding="utf-8"))


def main():
    projection = load_json("projection.json")
    projection = copy.deepcopy(projection)
    projection["non_paying"] = ["IWDA.NL", HOSTILE]
    projection["accumulating"] = [
        {"symbol": "IWDA.NL", "value": 20000.0, "annual_equivalent": 800.0},
        {"symbol": HOSTILE, "value": 5000.0, "annual_equivalent": 200.0},
    ]
    distribution = load_json("distribution.json")
    maximize = load_json("maximize.json")

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch()
            for width in WIDTHS:
                run_case(browser, port, width, projection, distribution, maximize)
            browser.close()
    finally:
        server.shutdown()
    print(f"capital_tax_browser_test: OK across {len(WIDTHS)} widths")


def run_case(browser, port, width, projection, distribution, maximize):
    context = browser.new_context(viewport={"width": width, "height": 900})
    page = context.new_page()
    page_errors, console_errors, posted, projection_loads = [], [], [], []
    page.on("pageerror", lambda exc: page_errors.append(str(exc)))
    page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
    page.on("dialog", lambda d: page_errors.append(f"dialog: {d.message}"))

    stored = {"w8ben": False, "pt_regime": {"kind": "flat"}}

    def fulfill(route, payload, status=200):
        route.fulfill(
            status=status,
            content_type="application/json",
            headers={"Cache-Control": "no-store"},
            body=json.dumps(payload),
        )

    def handle_api(route):
        path = urlparse(route.request.url).path
        if path == "/api/guardian/token":
            return fulfill(route, {"token": TOKEN})
        if path == "/api/capital/accounts":
            return fulfill(route, [{"account": "main", "currency": "EUR", "entries": 12}])
        if path == "/api/capital/dividends/tax":
            if route.request.method == "POST":
                body = json.loads(route.request.post_data or "{}")
                posted.append(body)
                stored.clear()
                stored.update(body)
            return fulfill(route, stored)
        if path == "/api/capital/dividends/projection":
            projection_loads.append(1)
            return fulfill(route, projection)
        if path == "/api/capital/dividends/distribution":
            return fulfill(route, distribution)
        if path == "/api/capital/dividends/candidates":
            return fulfill(route, maximize["candidates"])
        if path == "/api/capital/dividends/maximize":
            return fulfill(route, maximize["jobs"]["empty"])
        if path == "/api/capital/dividends/calendar":
            return fulfill(route, [])
        if path == "/api/capital/dividends/record":
            return fulfill(route, {"graded": [], "pending": 0, "notes": []})
        if path == "/api/capital/dividends/retirement":
            return fulfill(route, {"scenarios": [], "assumptions": {"current_age": 35, "retire_age": 55, "target_monthly_net": 2500}})
        if path.startswith("/api/capital/"):
            return fulfill(route, {})
        route.continue_()

    page.route("**/api/**", handle_api)
    page.goto(f"http://127.0.0.1:{port}/capital.html", wait_until="networkidle")
    page.click("#cpTab-income")

    # The stored profile fills the form.
    page.locator("#cpTaxProfile > summary").click()
    expect(page.locator("#cpTaxW8ben")).not_to_be_checked()
    expect(page.locator("#cpTaxRegime")).to_have_value("flat")
    expect(page.locator("#cpTaxMarginalWrap")).to_be_hidden()
    expect(page.locator("#cpProjReserveRate")).to_have_text("28%")

    # An impossible rate is refused before any request.
    page.locator("#cpTaxRegime").select_option("englobamento")
    expect(page.locator("#cpTaxMarginalWrap")).to_be_visible()
    page.locator("#cpTaxMarginal").fill("70")
    page.locator("#cpTaxSave").click()
    expect(page.locator("#cpTaxStatus")).to_contain_text("between 0 and 53%")
    assert not posted, posted

    # Saving sends the contract shape and reloads the projection.
    loads_before = len(projection_loads)
    page.locator("#cpTaxW8ben").check()
    page.locator("#cpTaxMarginal").fill("35")
    page.locator("#cpTaxSave").click()
    expect(page.locator("#cpTaxStatus")).to_contain_text("Saved")
    assert posted == [
        {"w8ben": True, "pt_regime": {"kind": "englobamento", "marginal_rate": 0.35}}
    ], posted
    expect(page.locator("#cpProjReserveRate")).to_have_text("englobamento 35%")
    page.wait_for_load_state("networkidle")
    assert len(projection_loads) > loads_before, "projection not reloaded"

    # Accumulating funds: 4% equivalent, hostile symbol shown as text.
    page.locator("#cpProjHoldingsDetails > summary").click()
    box = page.locator("#cpProjAccumulating")
    expect(box).to_be_visible()
    expect(box).to_contain_text("IWDA.NL")
    expect(box).to_contain_text("800")
    expect(box).to_contain_text(HOSTILE)
    expect(page.locator("#cpProjAccumulatingTotal")).to_contain_text("83.33")
    assert page.locator("#cpProjAccumulating img").count() == 0

    overflow = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
    assert not overflow, f"Horizontal overflow at {width}px"
    assert not page_errors, f"Page errors at {width}px: {page_errors}"
    assert not console_errors, f"Console errors at {width}px: {console_errors}"
    context.close()


if __name__ == "__main__":
    main()
