"""Fixture-based Capital early-retirement UI regression; no model or network claims.

Run with a Playwright-equipped Python (e.g. /home/main/SAIL/.venv-voice/bin/python).
"""
import http.server
import json
from pathlib import Path
import threading
from urllib.parse import parse_qs, urlparse

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[2] / "crates/sail-dialogue/web"
TOKEN = "capital-retirement-ui-owner-token-at-least-32-bytes"
OUTPUT = Path("/tmp/sail-capital-retirement-qa")


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def log_message(self, *_):
        pass


def projection_fixture():
    def year_row(year, net, value):
        return {
            "year": year,
            "gross": net * 1.1,
            "withheld": net * 0.1,
            "pt_tax": 0.0,
            "net": net,
            "reinvested": net,
            "contributed": 0.0,
            "cash": 0.0,
            "value": value,
            "yield_on_cost": None,
        }

    def scenario(name, factor):
        rows = [year_row(year, 8100 * factor + year * 120, 100000 + year * 9000) for year in range(1, 31)]
        return {
            "scenario": name,
            "years": rows,
            "monthly_net": [row["net"] / 12 for row in rows for _ in range(12)],
            "doubling_year": None,
        }

    return {
        "settings": {
            "years": 30,
            "reinvest_pct": 100.0,
            "allocation": "proportional",
            "top_n": 5,
            "monthly_contribution": 300.0,
            "reserve_pt_tax": False,
            "start": "2026-12-01",
        },
        "scenarios": [scenario("low", 0.8), scenario("base", 1.0), scenario("high", 1.2)],
        "holdings": [],
        "non_paying": [],
        "without_reinvestment": {
            "scenario": "base",
            "years": [],
            "monthly_net": [],
            "doubling_year": None,
        },
        "notes": ["Fixture projection: deterministic, no live data."],
    }


def plan_fixture():
    return {
        "assumptions": {
            "current_age": 35,
            "retire_age": 55,
            "target_monthly_net": 2500.0,
            "inflation_pct": 2.0,
            "withdrawal_rate_pct": 4.0,
        },
        "years_to_retirement": 20,
        "retirement_year": 20,
        "target_monthly_at_retirement": 3715.0,
        "target_annual_at_retirement": 44580.0,
        "required_capital": 1114500.0,
        "progress_pct": 31.2,
        "base_monthly_net_today": 780.0,
        "current_monthly_contribution": 300.0,
        "required_monthly_contribution": 850.0,
        "scenarios": [
            {
                "scenario": "low",
                "monthly_net_today": 900.0,
                "monthly_net_at_retirement": 1336.0,
                "annual_net_at_retirement": 16032.0,
                "value_at_retirement": 300000.0,
                "income_reached_year": None,
                "income_reached_age": None,
                "funded_by_income": False,
                "funded_by_capital": False,
            },
            {
                "scenario": "base",
                "monthly_net_today": 1200.0,
                "monthly_net_at_retirement": 1782.0,
                "annual_net_at_retirement": 21384.0,
                "value_at_retirement": 380000.0,
                "income_reached_year": 18,
                "income_reached_age": 53,
                "funded_by_income": True,
                "funded_by_capital": False,
            },
            {
                "scenario": "high",
                "monthly_net_today": 1500.0,
                "monthly_net_at_retirement": 2228.0,
                "annual_net_at_retirement": 26736.0,
                "value_at_retirement": 470000.0,
                "income_reached_year": 15,
                "income_reached_age": 50,
                "funded_by_income": True,
                "funded_by_capital": False,
            },
        ],
        "notes": [
            "Fixture: about 850 a month of extra investing reaches the target by age 55.",
            "This is a planning estimate, not financial advice.",
        ],
    }


BRIEF = {
    "headline": "Close the gap with the allowance before changing the mix",
    "summary": "Your base projection covers about a third of the target today. Raising the allowance does more than a riskier allocation, and it keeps the dividend test honest.",
    "strategies": [
        {
            "name": "Raise the monthly allowance first",
            "why": "The base projection needs about 850 a month to reach the income target by 55.",
            "actions": ["Set the allowance to 850 and re-run the projection.", "Check the year-20 net income."],
            "tradeoffs": ["Less cash today.", "The date can still slip if payouts are cut."],
            "citations": [1, 2],
        },
        {
            "name": "Keep the tax reserve on",
            "why": "Portuguese dividends are taxed at a flat 28%, which the projection can set aside.",
            "actions": ["Turn on the 28% reserve when comparing income."],
            "tradeoffs": ["Lower net income in the projection."],
            "citations": [2],
        },
    ],
    "risks": ["A dividend cut delays the date.", "Sequence risk is not simulated."],
    "next_steps": ["Re-run the projection with the 850 allowance."],
    "assumptions": ["2% inflation", "4% withdrawal rate"],
    "sources": [
        {
            "number": 1,
            "title": "Safe withdrawal rate (Wikipedia)",
            "url": "https://en.wikipedia.org/wiki/Safe_withdrawal_rate",
            "retrieved_at": "2026-11-14T12:00:00Z",
            "cited": True,
        },
        {
            "number": 2,
            "title": "Portuguese IRS Code, article 72 (Autoridade Tributária)",
            "url": "https://info.portaldasfinancas.gov.pt/pt/informacao_fiscal/codigos_tributarios/cirs_rep/Pages/irs72.aspx",
            "retrieved_at": "2026-11-14T12:00:00Z",
            "cited": True,
        },
    ],
    "model": "fixture-model",
    "generated_at": "2026-11-14T12:01:00Z",
    "structured": True,
    "notes": ["Fixture brief, from the UI test."],
}


def snapshot(status, error=None, brief=None):
    return {
        "status": status,
        "started_at": "2026-11-14T12:00:00Z" if status != "idle" else None,
        "finished_at": "2026-11-14T12:01:00Z" if status in ("done", "failed") else None,
        "last_error": error,
        "model": "fixture-model" if brief else None,
        "brief": brief,
        "sources": [
            {
                "title": "Safe withdrawal rate (Wikipedia)",
                "url": "https://en.wikipedia.org/wiki/Safe_withdrawal_rate",
                "ok": True,
                "chars": 4200,
            }
        ]
        if status != "idle"
        else [],
    }


def main():
    OUTPUT.mkdir(exist_ok=True)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    state = {"research": snapshot("idle"), "plan": plan_fixture()}
    requests = []

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1100})
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))

        def api(route):
            request = route.request
            url = urlparse(request.url)
            path = url.path.split("/api/", 1)[1] if "/api/" in url.path else ""
            query = parse_qs(url.query)
            requests.append((request.method, path, query))
            if path == "guardian/token":
                return route.fulfill(json={"token": TOKEN})
            if request.headers.get("authorization") != f"Bearer {TOKEN}":
                return route.fulfill(status=401, json={"error": "Owner authentication required"})
            if path == "capital/accounts":
                return route.fulfill(
                    json=[{"account": "xtb:fixture-1234", "currency": "EUR", "entries": 42}]
                )
            if path == "capital/dividends/projection":
                return route.fulfill(json=projection_fixture())
            if path == "capital/dividends/retirement":
                # Echo the requested target so a control change is observable.
                plan = json.loads(json.dumps(state["plan"]))
                if "target_monthly" in query:
                    plan["assumptions"]["target_monthly_net"] = float(query["target_monthly"][0])
                return route.fulfill(json=plan)
            if path == "capital/dividends/retirement/research":
                if request.method == "POST":
                    state["research"] = snapshot("running")
                    return route.fulfill(
                        status=202, json={"started": True, "research": state["research"]}
                    )
                return route.fulfill(json=state["research"])
            if path == "capital/insights":
                return route.fulfill(json={})
            if path == "capital/portfolio":
                return route.fulfill(json={"account": "xtb:fixture-1234", "positions": [], "cash": []})
            return route.fulfill(json={})

        page.route("**/api/**", api)
        page.goto(f"http://127.0.0.1:{server.server_port}/capital.html#income")
        expect(page.locator("#cpPanel-income")).to_be_visible()
        expect(page.locator("#cpRetirementPanel")).to_be_visible()
        expect(page.locator("#cpRetireTileYear")).to_contain_text("Year 20")
        expect(page.locator("#cpRetireScenarioBody tr")).to_have_count(3)
        expect(page.locator("#cpRetireTileContribution")).to_contain_text("850")
        expect(page.locator("#cpRetireResearchState")).to_contain_text("Not run yet")
        page.screenshot(path=str(OUTPUT / "plan-1440.png"), full_page=True)

        # Changing the target re-runs the plan with the new value.
        page.locator("#cpRetireTarget").fill("3000")
        page.wait_for_timeout(800)
        retirement_calls = [r for r in requests if r[1] == "capital/dividends/retirement"]
        assert retirement_calls, requests
        assert retirement_calls[-1][2]["target_monthly"][0] == "3000", retirement_calls[-1]

        # The research run is a POST with the same settings; then the fixture completes.
        page.locator("#cpRetireResearchBtn").click()
        expect(page.locator("#cpRetireResearchState")).to_contain_text("Researching")
        expect(page.locator("#cpRetireResearchBtn")).to_be_disabled()
        posts = [r for r in requests if r[1].endswith("research") and r[0] == "POST"]
        assert len(posts) == 1, requests
        assert posts[0][2]["current_age"][0] == "35"
        assert posts[0][2]["retire_age"][0] == "55"
        assert posts[0][2]["target_monthly"][0] == "3000"
        state["research"] = snapshot("done", brief=BRIEF)
        expect(page.locator("#cpRetireResearchBody")).to_contain_text(
            "Close the gap with the allowance", timeout=10000
        )
        expect(page.locator("#cpRetireResearchState")).to_contain_text("Ready")
        expect(page.locator(".cp-retire-strategy")).to_have_count(2)
        expect(page.locator(".cp-retire-cite").first).to_have_attribute(
            "href", "https://en.wikipedia.org/wiki/Safe_withdrawal_rate"
        )
        expect(page.locator("#cpRetireResearchBtn")).to_be_enabled()

        for width in [1440, 768, 390, 360]:
            page.set_viewport_size({"width": width, "height": 1100})
            assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), width
            expect(page.locator("#cpRetirementPanel")).to_be_visible()
            page.screenshot(path=str(OUTPUT / f"brief-{width}.png"), full_page=True)

        page.emulate_media(forced_colors="active")
        page.screenshot(path=str(OUTPUT / "brief-forced-colors-360.png"), full_page=True)
        page.emulate_media()

        # A failed run is shown with its reason, and the previous brief stays readable.
        state["research"] = snapshot("failed", error="No research source could be fetched.", brief=BRIEF)
        page.reload()
        expect(page.locator("#cpRetireResearchState")).to_contain_text("Last run failed")
        expect(page.locator("#cpRetireResearchBody")).to_contain_text("No research source")
        expect(page.locator("#cpRetireResearchBody")).to_contain_text("Close the gap")

        assert not errors, errors
        browser.close()
    server.shutdown()
    print(
        "Capital retirement UI passed: plan, control change, research run, brief, 1440/768/390/360, forced colors, failure."
    )


if __name__ == "__main__":
    main()
