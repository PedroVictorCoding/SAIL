"""Fixture-based Capital round 24 UI regression (Signals + Income); no live network.

Run with: /home/main/SAIL/.venv-voice/bin/python scripts/ui/capital_r24_browser_test.py
"""
import http.server
import json
from pathlib import Path
import threading
from urllib.parse import urlparse

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[2] / "crates/sail-dialogue/web"
FIXTURES = Path(__file__).resolve().parent / "fixtures/capital_r24"
TOKEN = "capital-r24-ui-owner-token-at-least-32-bytes"
WIDTHS = (1440, 768, 390, 360)


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def log_message(self, *_):
        pass


def load_fixture(name):
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


def main():
    trade_fx = load_fixture("trade.json")
    proj_fx = load_fixture("projection.json")
    dist_fx = load_fixture("distribution.json")
    max_fx = load_fixture("maximize.json")

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    try:
        with sync_playwright() as p:
            browser = p.chromium.launch()
            for width in WIDTHS:
                for state_name in ("empty", "running", "done", "no_beat", "failed"):
                    run_case(browser, port, width, state_name, trade_fx, proj_fx, dist_fx, max_fx)
            browser.close()
    finally:
        server.shutdown()

    print(f"capital_r24_browser_test: OK across {len(WIDTHS)} widths and 5 states")


def run_case(browser, port, width, max_state, trade_fx, proj_fx, dist_fx, max_fx):
    context = browser.new_context(viewport={"width": width, "height": 900})
    page = context.new_page()
    page_errors = []
    console_errors = []
    page.on("pageerror", lambda exc: page_errors.append(str(exc)))
    page.on("console", lambda msg: console_errors.append(msg.text) if msg.type == "error" else None)

    candidates = list(max_fx["candidates"])
    current_job = max_fx["jobs"][max_state]

    def fulfill_json(route, payload, status=200):
        route.fulfill(
            status=status,
            content_type="application/json",
            headers={"Cache-Control": "no-store"},
            body=json.dumps(payload),
        )

    def handle_api(route):
        req = route.request
        path = urlparse(req.url).path
        if path == "/api/guardian/token":
            return fulfill_json(route, {"token": TOKEN})
        if path == "/api/capital/accounts":
            return fulfill_json(route, [{"account": "main", "currency": "EUR", "entries": 12}])
        if path == "/api/capital/signals/trade":
            return fulfill_json(route, trade_fx)
        if path == "/api/capital/dividends/projection":
            return fulfill_json(route, proj_fx)
        if path == "/api/capital/dividends/distribution":
            return fulfill_json(route, dist_fx)
        if path == "/api/capital/dividends/candidates":
            if req.method == "POST":
                body = json.loads(req.post_data or "{}")
                for c in candidates:
                    if c["symbol"] == body.get("symbol"):
                        c["enabled"] = bool(body.get("enabled"))
            return fulfill_json(route, candidates)
        if path == "/api/capital/dividends/maximize":
            if req.method == "POST":
                return fulfill_json(route, {"started": True, "job": current_job}, status=202)
            return fulfill_json(route, current_job)
        if path == "/api/capital/limits":
            return fulfill_json(route, {"max_position_pct": 25})
        if path == "/api/capital/outlook":
            return fulfill_json(route, {"rows": [], "notes": []})
        if path == "/api/capital/insights":
            return fulfill_json(route, {"dividends": []})
        if path == "/api/capital/dividends/retirement":
            return fulfill_json(route, {"scenarios": [], "assumptions": {"current_age": 35, "retire_age": 55, "target_monthly_net": 2500}})
        if path.startswith("/api/capital/"):
            return fulfill_json(route, {})
        route.continue_()

    page.route("**/api/**", handle_api)
    page.goto(f"http://127.0.0.1:{port}/capital.html", wait_until="networkidle")

    # 1. Check Signals / Outlook tab (trade cards, heat bar, regime chips)
    if width == 1440 and max_state == "done":
        page.click("#cpTab-outlook")
        expect(page.locator("#cpTradeContextBar")).to_be_visible()
        expect(page.locator("#cpTradeContextBar")).to_contain_text("1.75% / 3.0%")
        expect(page.locator("#cpTradeContextBar")).to_contain_text("SPY: Uptrend · Calm")
        expect(page.locator("#cpSignalsCardsGrid")).to_contain_text("Reached 1R 54%")
        expect(page.locator("#cpSignalsCandidatesGrid")).to_contain_text("14 trades to a verdict")
        overflow = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
        assert not overflow, f"Horizontal overflow on outlook at {width}px"

    # 2. Check Income tab across all widths & states
    page.click("#cpTab-income")
    expect(page.locator("#cpMaximizePanel")).to_be_visible()
    expect(page.locator("#cpIncomeFanWrap")).to_be_visible()
    expect(page.locator("#cpIncomeFanWrap")).to_contain_text("68%")
    expect(page.locator("#cpProjSpecialsNote")).to_contain_text("JNJ")

    if max_state == "empty":
        expect(page.locator("#cpMaxResult")).to_contain_text("Run maximizer")
    elif max_state == "running":
        expect(page.locator("#cpMaxResult")).to_contain_text("Searching allocations")
    elif max_state == "done":
        expect(page.locator("#cpMaxResult")).to_contain_text("This month's buy list")
        expect(page.locator("#cpMaxResult")).to_contain_text("PEP")
        expect(page.locator("#cpMaxResult")).to_contain_text("consider buying")
        # Open holdings details and tap safety chip on JNJ to reveal reasons
        if width == 1440:
            page.locator("#cpProjHoldingsDetails summary").click()
            page.locator("#cpProjHoldingsBody .cp-safety-chip").first.click()
            expect(page.locator(".cp-safety-detail-row")).to_contain_text("Free cash flow covered")
    elif max_state == "no_beat":
        expect(page.locator("#cpMaxResult")).to_contain_text("No allocation beats Proportional")
    elif max_state == "failed":
        expect(page.locator("#cpMaxResult")).to_contain_text("insufficient dividend history")

    overflow = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
    assert not overflow, f"Horizontal overflow on income ({max_state}) at {width}px"
    assert not page_errors, f"Page errors at {width}px ({max_state}): {page_errors}"
    assert not console_errors, f"Console errors at {width}px ({max_state}): {console_errors}"
    context.close()


if __name__ == "__main__":
    main()
