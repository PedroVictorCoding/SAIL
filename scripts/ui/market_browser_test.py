"""SAIL Agents > Market & Proposed skills browser check (round 20, Antigravity).

Run the agents_studio example first with SAIL_AGENTS_DEMO_PROPOSAL=1 and
SAIL_AGENTS_DEMO_TOKEN set, then run:

    python scripts/ui/market_browser_test.py <shots_dir>
"""
import json
import os
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

BASE = "http://127.0.0.1:18809"
TOKEN = os.environ["SAIL_AGENTS_DEMO_TOKEN"]
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/market-shots")
SHOTS.mkdir(parents=True, exist_ok=True)
ROOT = Path(__file__).resolve().parents[2]


def open_market(page):
    page.goto(BASE + "/#agents?section=market")
    expect(page.locator("#subsystemContent #marketCard")).to_be_visible()


with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    for idx, width in enumerate((1440, 768, 390, 360)):
        page = browser.new_page(viewport={"width": width, "height": 1000}, reduced_motion="reduce")
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.add_init_script(
            "localStorage.setItem('sail_owner_token', " + json.dumps(TOKEN) + ");"
            "localStorage.removeItem('sail.agents.section');"
        )
        page.route(
            "**/icons.svg",
            lambda r: r.fulfill(
                body=(ROOT / "assets/source/icons/sail-icons.svg").read_bytes(),
                content_type="image/svg+xml",
            ),
        )

        open_market(page)

        # The first visit starts the catalog download by itself (contract: GET starts a
        # background refresh when stale). On the fake network it can finish before the
        # first-run prompt renders, so use the Download button only if it is still shown.
        if idx == 0 and page.locator("#marketEmptyFirstRun").is_visible():
            page.locator("#marketDownloadBtn").click()
        expect(page.locator("#marketCatalogLine")).to_contain_text("5 skills", timeout=15000)
        expect(page.locator("#marketCatalogLine")).to_contain_text("2 plugins")

        # Allow: pdf -> Review -> Install -> Installed -> Skills & plugins.
        page.locator("#marketQuery").fill("pdf")
        pdf_row = page.locator('[data-market-id="skills-sh/demo/skills/pdf"]')
        expect(pdf_row).to_be_visible()
        pdf_row.get_by_role("button", name="Review").click()
        expect(pdf_row.locator(".market-stage-panel")).to_contain_text("SKILL.md", timeout=10000)
        pdf_row.get_by_role("button", name="Install").click()
        expect(pdf_row).to_contain_text("Installed", timeout=10000)
        page.screenshot(path=str(SHOTS / f"market-{width}.png"), full_page=True)

        # Verify pdf appears in Skills & plugins.
        page.locator("#section-skills").click()
        expect(page.locator("#subsystemContent #hermesCard")).to_be_visible()
        expect(page.locator('#hermesSkillList [data-skill="pdf"]')).to_be_visible()

        # Proposals (staged once at demo startup when SAIL_AGENTS_DEMO_PROPOSAL=1).
        if idx == 0:
            prop = page.locator('[data-proposal-name="weekly-report"]')
            expect(prop).to_be_visible()
            expect(prop).to_contain_text("SAIL's reason")
            prop.get_by_role("button", name="Approve").click()
            expect(page.locator('#hermesSkillList [data-skill="weekly-report"]')).to_be_visible(timeout=10000)
        page.screenshot(path=str(SHOTS / f"agents-skills-{width}.png"), full_page=True)

        # Back to Market.
        page.locator("#section-market").click()
        expect(page.locator("#subsystemContent #marketCard")).to_be_visible()

        # Needs ack: Dictionary (ClawHub) -> Install anyway disabled until checkbox ticked.
        page.locator("#marketQuery").fill("Dictionary")
        dict_row = page.locator('[data-market-id="english-dictionary"]')
        expect(dict_row).to_be_visible()
        dict_row.get_by_role("button", name="Review").click()
        ack_btn = dict_row.get_by_role("button", name="Install anyway")
        expect(ack_btn).to_be_disabled(timeout=10000)
        dict_row.get_by_label("I have read the findings and want to install it anyway").check()
        expect(ack_btn).to_be_enabled()
        dict_row.get_by_role("button", name="Discard").click()

        # Block: speedup -> no install button, reason shown.
        page.locator("#marketQuery").fill("speedup")
        blk_row = page.locator('[data-market-id="demo/skills/skills/speedup"]')
        expect(blk_row).to_be_visible()
        blk_row.get_by_role("button", name="Review").click()
        expect(blk_row.locator(".market-stage-panel")).to_contain_text("dangerous", ignore_case=True, timeout=10000)
        assert blk_row.get_by_role("button", name="Install").count() == 0
        assert blk_row.get_by_role("button", name="Install anyway").count() == 0
        blk_row.get_by_role("button", name="Discard").click()

        # Unsupported: persona (LobeHub) -> Review disabled with reason.
        page.locator("#marketQuery").fill("persona")
        unsup_row = page.locator('[data-market-id="lobehub/persona"]')
        expect(unsup_row).to_be_visible()
        expect(unsup_row.get_by_role("button", name="Review")).to_be_disabled()
        expect(unsup_row.locator(".market-unsupported-reason")).not_to_be_empty()

        # Plugin: greeter -> install -> quarantine message -> Skills & plugins shows Not approved.
        page.locator("#marketQuery").fill("greeter")
        plug_row = page.locator('[data-market-id="greeter"]')
        expect(plug_row).to_be_visible()
        plug_row.get_by_role("button", name="Review").click()
        expect(plug_row.locator(".market-stage-panel")).to_contain_text("README", timeout=10000)
        plug_row.get_by_role("button", name="Install").click()
        expect(plug_row).to_contain_text("quarantined", timeout=10000)
        plug_row.get_by_role("button", name="Open Skills & plugins").click()
        expect(page.locator("#subsystemContent #hermesCard")).to_be_visible()
        expect(page.locator('[data-plugin="greeter"]')).to_contain_text("Not approved")

        # Back to Market: Uninstall pdf and greeter.
        page.locator("#section-market").click()
        page.locator("#marketQuery").fill("")
        inst_details = page.locator("#marketInstalledSection")
        if not inst_details.get_attribute("open"):
            page.locator("#marketInstalledSummary").click()
        for key in ("skill:pdf", "plugin:greeter"):
            item = page.locator(f'[data-installed-key="{key}"]')
            expect(item).to_be_visible()
            item.get_by_role("button", name="Uninstall").click()
            expect(item).to_have_count(0, timeout=10000)

        # One forced 503 on refresh shows error in #marketStatus.
        page.route(
            "**/api/hermes/market/refresh",
            lambda r: r.fulfill(status=503, content_type="application/json", body='{"error":"Market temporarily unavailable"}'),
        )
        page.locator("#marketRefresh").click()
        expect(page.locator("#marketStatus")).to_contain_text("Market temporarily unavailable")
        page.unroute("**/api/hermes/market/refresh")

        # Hygiene: no pageerrors, no horizontal overflow.
        assert not errors, f"pageerror at {width}: {errors}"
        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), f"horizontal overflow at {width}"

        # Forced colors: controls stay visible.
        page.emulate_media(forced_colors="active")
        expect(page.locator("#marketRefresh")).to_be_visible()
        expect(page.locator("#marketQuery")).to_be_visible()
        page.close()
    browser.close()
print("market_browser_test.py passed at all widths.")
