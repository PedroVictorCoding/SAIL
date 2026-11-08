"""Real Flow editor/API browser check. Run the flow_studio example first.

SAIL_FLOW_DEMO_TOKEN must match the loopback demo's token. No Flow responses are mocked.
Other SAIL services and the voice socket are unavailable in this isolated demo.
"""
import os
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

BASE = "http://127.0.0.1:18808"
TOKEN = os.environ["SAIL_FLOW_DEMO_TOKEN"]
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/flow-shots")
SHOTS.mkdir(parents=True, exist_ok=True)
ROOT = Path(__file__).resolve().parents[2]

with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    for width in (1440, 768, 390, 360):
        page = browser.new_page(viewport={"width": width, "height": 1000}, reduced_motion="reduce")
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.add_init_script("localStorage.setItem('sail_owner_token', " + __import__('json').dumps(TOKEN) + ")")
        page.route("**/icons.svg", lambda r: r.fulfill(body=(ROOT / "assets/source/icons/sail-icons.svg").read_bytes(), content_type="image/svg+xml"))
        page.goto(BASE + "/#flow")
        expect(page.locator("#flowGraph .flow-node").first).to_be_visible()
        expect(page.locator("#subsystemContent #flowCard")).to_be_visible()
        assert not page.locator("#pulseCard").is_visible()
        expect(page.locator("#tmCrumb")).to_have_text("Flow")
        page.locator("#flowNew").click()
        page.locator("#flowName").fill(f"Browser workflow {width}")
        page.locator('[data-node="trim"]').click()
        page.get_by_label("Operation", exact=True).select_option("contains")
        page.get_by_label("Node text", exact=True).fill("hello")
        page.locator('[data-node="result"]').click()
        page.get_by_label("Run on branch", exact=True).select_option("true")
        page.locator("#flowAdd").click()
        page.get_by_label("Operation", exact=True).select_option("output")
        page.get_by_label("Input node", exact=True).select_option("trim")
        page.get_by_label("Run on branch", exact=True).select_option("false")
        page.locator("#flowSave").click()
        expect(page.locator("#flowDraftState")).to_have_text("Saved version 1")
        page.locator("#flowInput").fill("hello <script>window.flowInjected=true</script>")
        page.locator("#flowRun").click()
        expect(page.locator("#flowRuns summary").first).to_contain_text("completed", timeout=15000)
        details = page.locator("#flowRuns details").first
        if not details.get_attribute("open") == "":
            details.locator("summary").click()
        expect(details).to_contain_text("node_1: Skipped")
        assert not page.evaluate("Boolean(window.flowInjected)")
        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), width
        page.screenshot(path=str(SHOTS / f"flow-results-{width}.png"), full_page=True)
        page.evaluate("document.querySelector('.tm-root').scrollTop = 0; document.querySelector('.flow-stage').scrollLeft = 0; document.querySelector('.tm-pane-main').scrollTop = 0; document.querySelector('#flowCard').scrollTop = 0; window.scrollTo(0,0)")
        page.screenshot(path=str(SHOTS / f"flow-{width}.png"), full_page=True)
        page.reload()
        expect(page.locator("#flowRuns summary").first).to_contain_text("completed")
        # Opening Flow from the rail must not redirect to Pulse.
        page.evaluate("window.sailNavigate('pulse')")
        expect(page.locator("#pulseCard")).to_be_visible()
        if width > 860:
            page.locator('[data-nav="flow"]').click()
        else:
            page.evaluate("window.sailToolsMap.open('flow')")
        expect(page.locator("#flowCard")).to_be_visible()
        assert page.evaluate("location.hash") == "#flow"
        assert not page.locator("#pulseCard").is_visible()
        # Explicit service failure must appear, without fake successful activity.
        page.route("**/api/flow/**", lambda r: r.fulfill(status=503, json={"error": "Flow storage unavailable"}))
        page.locator("#flowRefresh").click()
        expect(page.locator("#flowStatus")).to_have_text("Flow storage unavailable")
        page.emulate_media(forced_colors="active")
        assert page.locator("#flowSave").is_visible()
        # Isolated demo intentionally lacks voice/models; Flow must not throw JS errors.
        assert not errors, errors
        print(f"PASS {width}px: visual branch editing, real save/run, skipped branch, safe text, reload, route separation, unavailable state")
        page.close()
    browser.close()
