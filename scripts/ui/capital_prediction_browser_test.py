"""Fixture-based Capital stock-forecast UI regression; no real model claims.
Run with a Playwright-equipped Python (e.g. .venv-voice/bin/python).
"""
import http.server
import json
import os
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[2] / "crates/sail-dialogue/web"

class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)
    def log_message(self, *_):
        pass


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    output = Path("/tmp/sail-capital-prediction-qa")
    output.mkdir(exist_ok=True)
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1000})
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        job = {"id": "fixture", "state": "running", "request": {"symbol": "AAPL", "interval": "1d", "horizon": 2}, "error": None, "result": None}
        state = {"enabled": True, "yahoo_enabled": True, "model": "google/timesfm-2.5-200m-pytorch", "job": None}
        submitted = []
        def api(route):
            path = route.request.url.split("/api/")[1]
            assert path == "guardian/token" or route.request.headers.get("authorization") == "Bearer " + "x" * 40
            if path == "guardian/token": data = {"token": "x" * 40}
            elif path == "capital/accounts": data = []
            elif path == "capital/prediction":
                if route.request.method == "POST":
                    submitted.append(route.request.post_data_json)
                    state["job"] = job
                    return route.fulfill(status=202, json=job)
                data = state
            else: data = {}
            route.fulfill(json=data)
        page.route("**/api/**", api)
        page.goto(f"http://127.0.0.1:{server.server_port}/capital.html#prediction")
        expect(page.locator("#cpPanel-prediction")).to_be_visible()
        expect(page.locator("#cpPredConfig")).to_contain_text("Local TimesFM enabled")
        expect(page.locator("#cpTab-prediction")).to_have_attribute("aria-selected", "true")
        expect(page.locator("#cpPredStatus")).to_contain_text("Choose a ticker")
        page.screenshot(path=str(output / "empty-1440.png"))
        page.locator("#cpPredSymbol").fill("AAPL")
        page.locator("#cpPredHorizon").fill("2")
        page.locator("#cpPredRun").click()
        expect(page.locator("#cpPredStatus")).to_contain_text("running")
        expect(page.locator("#cpPredRun")).to_be_disabled()
        assert submitted == [{"symbol": "AAPL", "period": "1y", "interval": "1d", "horizon": 2}]
        # A fresh browser navigation recovers the active backend job.
        page.reload()
        expect(page.locator("#cpPredStatus")).to_contain_text("running")
        assert len(submitted) == 1
        job["state"] = "completed"
        job["result"] = {
            "symbol": "AAPL", "currency": "USD", "source": "Fixture only", "fetched_at": "2026-11-13T12:00:00Z", "context_points": 64,
            "model": state["model"], "model_revision": "a" * 40, "adjustment": "Fixture adjusted close", "exchange_timezone": "America/New_York",
            "chart_svg": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 780 330" role="img" aria-label="Fixture chart"><path d="M70 200L530 160L730 140" fill="none" stroke="#e5be79"/></svg>',
            "notes": ["Fixture: uncalibrated model deciles.", "Calendar projections are not exchange sessions."],
            "forecast": [{"step": i, "calendar_time": f"2026-11-{13+i}T12:00:00Z", "median": 105, "deciles": list(range(101, 110))} for i in [1,2]],
            "history": [],
        }
        expect(page.locator("#cpPredStatus")).to_contain_text("completed", timeout=10000)
        expect(page.locator("#cpPredRows tr")).to_have_count(2)
        for width in [1440, 768, 390, 360]:
            page.set_viewport_size({"width": width, "height": 1000})
            assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), width
            expect(page.locator("#cpPredRun")).to_be_visible()
            page.screenshot(path=str(output / f"completed-{width}.png"), full_page=True)
        live_path = os.environ.get("SAIL_PREDICTION_QA_RESULT")
        if live_path:
            real = json.loads(Path(live_path).read_text())
            original = job["result"]
            job["result"] = real
            job["request"]["horizon"] = len(real["forecast"])
            page.reload()
            expect(page.locator("#cpPredRows tr")).to_have_count(len(real["forecast"]))
            for width in [1440, 390]:
                page.set_viewport_size({"width": width, "height": 1000})
                assert page.evaluate("document.documentElement.scrollWidth <= innerWidth")
                page.screenshot(path=str(output / f"actual-timesfm-{width}.png"), full_page=True)
            job["result"] = original
            job["request"]["horizon"] = 2
            page.reload()
            expect(page.locator("#cpPredRows tr")).to_have_count(2)
        page.locator("#cpPanel-prediction summary").click()
        with page.expect_download() as download:
            page.locator("#cpPredExport").click()
        data = json.loads(Path(download.value.path()).read_text())
        assert data["forecast"][0]["deciles"] == list(range(101,110))
        page.emulate_media(reduced_motion="reduce", forced_colors="active")
        page.screenshot(path=str(output / "forced-colors-360.png"), full_page=True)
        job["state"] = "failed"
        job["result"] = None
        job["error"] = "Yahoo refused the request (HTTP 429)."
        page.reload()
        expect(page.locator("#cpPredError")).to_contain_text("HTTP 429")
        expect(page.locator("#cpPredResult")).to_be_hidden()
        page.screenshot(path=str(output / "error-360.png"), full_page=True)
        state["enabled"] = False
        state["job"] = None
        page.reload()
        expect(page.locator("#cpPredRun")).to_be_disabled()
        expect(page.locator("#cpPredConfig")).to_contain_text("disabled")
        assert not errors, errors
        browser.close()
    server.shutdown()
    print(f"Capital forecast UI passed: empty/running/reload/completed/export/error/disabled, 1440/768/390/360, reduced motion + forced colors. Screenshots: {output}")

if __name__ == "__main__":
    main()
