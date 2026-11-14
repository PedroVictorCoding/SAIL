"""SAIL Agents > Schedule & Plugin Hooks/Env browser test (round 21, Antigravity).

Run the agents_studio example first (on 127.0.0.1:18809) with SAIL_AGENTS_DEMO_TOKEN set.
All round-21 API responses are mocked via page.route() from scripts/ui/fixtures/schedule/*.json.

    python scripts/ui/schedule_browser_test.py [shots_dir]
"""
import copy
import json
import os
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

BASE = os.environ.get("SAIL_AGENTS_BASE_URL", "http://127.0.0.1:18809")
TOKEN = os.environ["SAIL_AGENTS_DEMO_TOKEN"]
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/r21-schedule-shots")
SHOTS.mkdir(parents=True, exist_ok=True)
ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "schedule"


def load_fixture(name):
    return json.loads((FIXTURES / name).read_text())


def install_mocks(page):
    state = {
        "schedule": load_fixture("list.json"),
        "runs": load_fixture("runs.json"),
        "preview_ok": load_fixture("preview_ok.json"),
        "preview_err": load_fixture("preview_err.json"),
        "plugins": load_fixture("plugins.json"),
        "plugin_polls": 0,
    }

    def handle_schedule(route):
        req = route.request
        path = req.url.split("?")[0].split("/api/agents/schedule", 1)[1]
        method = req.method

        if path in ("", "/") and method == "GET":
            return route.fulfill(status=200, json=state["schedule"])

        if path == "/preview" and method == "POST":
            body = json.loads(req.post_data or "{}")
            when = (body.get("when") or "").strip().lower()
            if "evry" in when or not when:
                return route.fulfill(status=400, json=state["preview_err"])
            return route.fulfill(status=200, json=state["preview_ok"])

        if path in ("", "/") and method == "POST":
            body = json.loads(req.post_data or "{}")
            new_job = {
                "id": "44444444444444444444444444444444",
                "name": body["name"],
                "when_text": body["when"],
                "tz": body["tz"],
                "anchor": 1794660100,
                "action": body["action"],
                "deliver": body["deliver"],
                "state": "active",
                "created_by": "owner",
                "created_at": 1794660100,
                "last_slot": null_val(),
                "failure_streak": 0,
                "reason": None,
                "describe": f"{body['when']} ({body['tz']})",
                "next_run": "2026-11-16T09:00:00Z",
                "last_run": None,
            }
            state["schedule"]["jobs"].append(new_job)
            return route.fulfill(status=200, json={"job": new_job})

        parts = [p for p in path.split("/") if p]
        if len(parts) == 2 and parts[1] == "runs" and method == "GET":
            return route.fulfill(status=200, json=state["runs"])

        if len(parts) == 2 and method == "POST":
            job_id, action = parts
            for j in state["schedule"]["jobs"]:
                if j["id"] == job_id:
                    if action == "approve":
                        j["state"] = "active"
                    elif action == "pause":
                        j["state"] = "paused"
                    elif action == "resume":
                        j["state"] = "active"
                        j["failure_streak"] = 0
                    elif action == "reject":
                        state["schedule"]["jobs"] = [x for x in state["schedule"]["jobs"] if x["id"] != job_id]
                        return route.fulfill(status=200, json={"ok": True})
                    return route.fulfill(status=200, json={"job": j})

        if len(parts) == 1 and method == "DELETE":
            job_id = parts[0]
            state["schedule"]["jobs"] = [x for x in state["schedule"]["jobs"] if x["id"] != job_id]
            return route.fulfill(status=200, json={"ok": True})

        return route.fulfill(status=404, json={"error": f"Unhandled mock route: {method} {path}"})

    def handle_hermes(route):
        req = route.request
        path = req.url.split("?")[0].split("/api/hermes", 1)[1]
        method = req.method

        if path == "/plugins" and method == "GET":
            p = state["plugins"]["plugins"][0]
            if p["environment"]["state"] == "building":
                state["plugin_polls"] += 1
                if state["plugin_polls"] >= 2:
                    p["environment"]["state"] = "ready"
                    p["environment"]["locked"] = [{"name": "httpx", "version": "0.27.2"}]
            return route.fulfill(status=200, json=state["plugins"])

        if path.startswith("/plugins/") and path.endswith("/hooks") and method == "POST":
            body = json.loads(req.post_data or "{}")
            p = state["plugins"]["plugins"][0]
            for h in p["hooks"]:
                if h["name"] == body.get("hook"):
                    h["enabled"] = bool(body.get("enabled"))
            return route.fulfill(status=200, json=p)

        if path.startswith("/plugins/") and path.endswith("/environment") and method == "POST":
            body = json.loads(req.post_data or "{}")
            p = state["plugins"]["plugins"][0]
            if body.get("action") == "build":
                state["plugin_polls"] = 0
                p["environment"]["state"] = "building"
                p["environment"]["locked"] = []
                return route.fulfill(status=202, json=p)
            if body.get("action") == "remove":
                p["environment"]["state"] = "needed"
                p["environment"]["locked"] = []
                return route.fulfill(status=202, json=p)

        if path == "/skills":
            return route.fulfill(status=200, json={"skills": [{"name": "arxiv", "description": "Search papers on arXiv", "source": "hermes"}], "rejected": [], "roots": []})
        if path == "/proposals":
            return route.fulfill(status=200, json={"proposals": []})

        return route.fulfill(status=404, json={"error": f"Unhandled hermes mock: {method} {path}"})

    page.route("**/api/agents/schedule**", handle_schedule)
    page.route("**/api/hermes/**", handle_hermes)
    page.route("**/api/flow/workflows", lambda r: r.fulfill(status=200, json={"workflows": [{"id": "wf-1", "name": "Daily trim"}], "runs": []}))


def null_val():
    return None


with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    for width in (1440, 768, 390, 360):
        page = browser.new_page(viewport={"width": width, "height": 1000}, reduced_motion="reduce")
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.add_init_script(
            "localStorage.setItem('sail_owner_token', " + json.dumps(TOKEN) + ");"
            "localStorage.removeItem('sail.agents.section');"
        )
        page.route("**/icons.svg", lambda r: r.fulfill(
            body=(ROOT / "assets/source/icons/sail-icons.svg").read_bytes(),
            content_type="image/svg+xml",
        ))
        install_mocks(page)

        # 1. Open Schedule section and verify status strip, proposals, and job list.
        page.goto(BASE + "/#schedule")
        expect(page.locator("#subsystemContent #scheduleCard")).to_be_visible()
        assert page.evaluate("location.hash") == "#agents?section=schedule"
        expect(page.locator("#scheduleStrip")).to_contain_text("Quiet hours now — results will wait until")
        expect(page.locator("#scheduleProposalsWrap")).to_be_visible()
        expect(page.locator('#scheduleProposalList [data-job="11111111111111111111111111111111"]')).to_contain_text("Weekly repo digest")
        expect(page.locator('#scheduleJobList [data-job="22222222222222222222222222222222"]')).to_contain_text("Morning briefing")
        expect(page.locator('#scheduleJobList [data-job="33333333333333333333333333333333"]')).to_contain_text("Paused after 3 failed runs")

        # 2. Live preview: error then success.
        page.locator("#scheduleWhen").fill("evry day")
        expect(page.locator("#schedulePreview")).to_contain_text('I don\'t understand "evry day"')
        page.locator("#scheduleWhen").fill("weekdays at 9am")
        expect(page.locator("#schedulePreview")).to_contain_text("weekdays at 09:00 (Europe/Lisbon)")

        # 3. Create a job.
        page.locator("#scheduleName").fill("Standup notes")
        page.locator("#schedulePromptText").fill("Prepare standup notes.")
        page.locator("#scheduleCreateBtn").click()
        expect(page.locator('#scheduleJobList [data-job="44444444444444444444444444444444"]')).to_contain_text("Standup notes")

        # 4. Approve a proposed job.
        page.locator('#scheduleProposalList [data-job="11111111111111111111111111111111"]').get_by_role("button", name="Approve").click()
        expect(page.locator('#scheduleJobList [data-job="11111111111111111111111111111111"]')).to_contain_text("Active")
        expect(page.locator("#scheduleProposalsWrap")).to_be_hidden()

        # 5. Open job detail, check run history, pause and resume.
        page.locator('#scheduleJobList [data-job="22222222222222222222222222222222"] .schedule-job-select').click()
        expect(page.locator("#scheduleDetail")).to_be_visible()
        expect(page.locator("#scheduleRunList")).to_contain_text("Held for quiet hours")
        expect(page.locator("#scheduleRunList")).to_contain_text("missed 2 runs")

        page.locator("#scheduleDetailActions").get_by_role("button", name="Pause").click()
        expect(page.locator('#scheduleJobList [data-job="22222222222222222222222222222222"]')).to_contain_text("Paused")
        page.locator("#scheduleDetailActions").get_by_role("button", name="Resume").click()
        expect(page.locator('#scheduleJobList [data-job="22222222222222222222222222222222"]')).to_contain_text("Active")

        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), width
        page.screenshot(path=str(SHOTS / f"schedule-{width}.png"), full_page=True)

        # 6. Switch to Skills & plugins and verify hooks toggle + environment build polling.
        page.locator("#section-skills").click()
        expect(page.locator("#subsystemContent #hermesCard")).to_be_visible()
        plugin = page.locator('[data-plugin="greeter"]')
        expect(plugin).to_contain_text("Sees your message for this turn, not your history.")
        expect(plugin.locator('[data-hook="custom_event"]')).to_contain_text("Not run in SAIL")
        hook_toggle = plugin.locator('[data-hook="pre_llm_call"] input[type="checkbox"]')
        assert not hook_toggle.is_checked()
        hook_toggle.click()
        expect(hook_toggle).to_be_checked()

        plugin.get_by_role("button", name="Build environment").click()
        expect(plugin).to_contain_text("Building…")
        expect(plugin).to_contain_text("httpx==0.27.2", timeout=10000)

        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), width
        assert not errors, errors
        print(f"PASS {width}px: schedule list/preview/create/approve/pause/resume/runs + plugin hooks/env polling")
        page.close()
    browser.close()
