"""Fixture-based Capital round 25 UI regression (calendar, projection record, insider chips).

Run with: /home/main/SAIL/.venv-voice/bin/python scripts/ui/capital_r25_browser_test.py
"""
import http.server
import json
from pathlib import Path
import threading
from urllib.parse import urlparse

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[2] / "crates/sail-dialogue/web"
FIXTURES_R25 = Path(__file__).resolve().parent / "fixtures/capital_r25"
FIXTURES_R24 = Path(__file__).resolve().parent / "fixtures/capital_r24"
TOKEN = "capital-r25-ui-owner-token-at-least-32-bytes"
WIDTHS = (1440, 768, 390, 360)


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def log_message(self, *_):
        pass


def load_json(folder, name):
    return json.loads((folder / name).read_text(encoding="utf-8"))


def main():
    cal_fx = load_json(FIXTURES_R25, "calendar.json")
    rec_graded = load_json(FIXTURES_R25, "record_graded.json")
    rec_pending = load_json(FIXTURES_R25, "record_pending.json")
    insider_fx = load_json(FIXTURES_R25, "insider.json")
    board_fx = load_json(FIXTURES_R25, "board.json")
    trade_fx = load_json(FIXTURES_R25, "trade.json")

    proj_fx = load_json(FIXTURES_R24, "projection.json")
    dist_fx = load_json(FIXTURES_R24, "distribution.json")
    max_fx = load_json(FIXTURES_R24, "maximize.json")

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    try:
        with sync_playwright() as p:
            browser = p.chromium.launch()
            for width in WIDTHS:
                for rec_mode, rec_fx in (("graded", rec_graded), ("pending", rec_pending)):
                    run_case(
                        browser,
                        port,
                        width,
                        rec_mode,
                        cal_fx,
                        rec_fx,
                        insider_fx,
                        board_fx,
                        trade_fx,
                        proj_fx,
                        dist_fx,
                        max_fx,
                    )
            browser.close()
    finally:
        server.shutdown()

    print(f"capital_r25_browser_test: OK across {len(WIDTHS)} widths and 2 record states")


def run_case(
    browser,
    port,
    width,
    rec_mode,
    cal_fx,
    rec_fx,
    insider_fx,
    board_fx,
    trade_fx,
    proj_fx,
    dist_fx,
    max_fx,
):
    context = browser.new_context(viewport={"width": width, "height": 900})
    page = context.new_page()
    page_errors = []
    console_errors = []
    page.on("pageerror", lambda exc: page_errors.append(str(exc)))
    page.on("console", lambda msg: console_errors.append(msg.text) if msg.type == "error" else None)

    def fulfill_json(route, payload, status=200):
        route.fulfill(
            status=status,
            content_type="application/json",
            headers={"Cache-Control": "no-store"},
            body=json.dumps(payload),
        )

    def handle_api(route):
        path = urlparse(route.request.url).path
        if path == "/api/guardian/token":
            return fulfill_json(route, {"token": TOKEN})
        if path == "/api/capital/accounts":
            return fulfill_json(route, [{"account": "main", "currency": "EUR", "entries": 12}])
        if path == "/api/capital/dividends/projection":
            return fulfill_json(route, proj_fx)
        if path == "/api/capital/dividends/distribution":
            return fulfill_json(route, dist_fx)
        if path == "/api/capital/dividends/calendar":
            return fulfill_json(route, cal_fx)
        if path == "/api/capital/dividends/record":
            return fulfill_json(route, rec_fx)
        if path == "/api/capital/dividends/candidates":
            return fulfill_json(route, max_fx["candidates"])
        if path == "/api/capital/dividends/maximize":
            return fulfill_json(route, max_fx["jobs"]["empty"])
        if path == "/api/capital/signals/insider":
            return fulfill_json(route, insider_fx)
        if path == "/api/capital/signals/board":
            return fulfill_json(route, board_fx)
        if path == "/api/capital/signals/board/record":
            return fulfill_json(route, {"graded": [], "pending": 0, "summary": []})
        if path == "/api/capital/signals/trade":
            return fulfill_json(route, trade_fx)
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

    # 1. Income tab: calendar above fan, plus "How the projection has done" panel
    page.click("#cpTab-income")
    expect(page.locator("#cpUpcomingCalendarWrap")).to_be_visible()
    expect(page.locator("#cpUpcomingCalendarWrap")).to_contain_text("Upcoming dividends (next 90 days)")
    expect(page.locator("#cpUpcomingCalendarWrap")).to_contain_text("XMPL.US")
    expect(page.locator("#cpUpcomingCalendarWrap")).to_contain_text("est.")
    expect(page.locator("#cpUpcomingCalendarWrap")).to_contain_text("high")

    expect(page.locator("#cpProjectionRecordPanel")).to_be_visible()
    expect(page.locator("#cpProjectionRecordPanel")).to_contain_text("How the projection has done")
    if rec_mode == "graded":
        expect(page.locator("#cpProjectionRecordPanel")).to_contain_text("2025-11")
        expect(page.locator("#cpProjectionRecordPanel")).to_contain_text("100.0%")
    else:
        expect(page.locator("#cpProjectionRecordPanel")).to_contain_text("3 monthly snapshots waiting 12 months")

    overflow_inc = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
    assert not overflow_inc, f"Horizontal overflow on income ({rec_mode}) at {width}px"

    # 2. Signals tab & Outlook tab: insider-cluster chips on per-stock rows and cards
    if rec_mode == "graded":
        page.click("#cpTab-signals")
        expect(page.locator("#cpSigbBody")).to_contain_text("Insider cluster · 3 buyers")
        overflow_sig = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
        assert not overflow_sig, f"Horizontal overflow on signals at {width}px"

        page.click("#cpTab-outlook")
        expect(page.locator("#cpSignalsCardsGrid")).to_contain_text("Insider cluster · 3 buyers")
        overflow_out = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
        assert not overflow_out, f"Horizontal overflow on outlook at {width}px"

    assert not page_errors, f"Page errors at {width}px ({rec_mode}): {page_errors}"
    assert not console_errors, f"Console errors at {width}px ({rec_mode}): {console_errors}"
    context.close()


if __name__ == "__main__":
    main()
