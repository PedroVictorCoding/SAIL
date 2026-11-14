"""SAIL Agents workspace browser check: Flow, Forge and Hermes skills/plugins as sections.

Run the agents_studio example first, with SAIL_DATA and HERMES_HOME set to the folders
this script prepares (pass them as arguments) and the same SAIL_AGENTS_DEMO_TOKEN. No
API response is mocked: the plugin is approved and described through the real sandbox.

    python agents_browser_test.py <hermes_home> <shots_dir>
"""
import json
import os
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

BASE = "http://127.0.0.1:18809"
TOKEN = os.environ["SAIL_AGENTS_DEMO_TOKEN"]
HERMES = Path(sys.argv[1])
SHOTS = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("/tmp/agents-shots")
SHOTS.mkdir(parents=True, exist_ok=True)
ROOT = Path(__file__).resolve().parents[2]
APPROVALS = Path(os.environ["SAIL_DATA"]) / "plugins" / "hermes-approvals.json"


def fixture():
    plugin = HERMES / "plugins" / "greeter"
    (plugin / "skills" / "greeting-etiquette").mkdir(parents=True, exist_ok=True)
    (plugin / "plugin.yaml").write_text("name: greeter\nversion: 1.2.0\ndescription: Greets people politely\nrequires_env: [GREETER_KEY]\n")
    (plugin / "__init__.py").write_text(
        "import json\n"
        "def register(ctx):\n"
        "    ctx.register_tool(name='greet', toolset='greeter', schema={'description': 'Greet someone', 'parameters': {'type': 'object'}},\n"
        "                      handler=lambda args, **kw: json.dumps({'hi': args.get('who')}))\n"
        "    ctx.register_command('wave', lambda raw: 'wave', description='Wave')\n"
        "    ctx.register_skill('greeting-etiquette', 'skills/greeting-etiquette/SKILL.md')\n"
        "    ctx.register_hook('post_tool_call', lambda **kw: None)\n")
    (plugin / "skills" / "greeting-etiquette" / "SKILL.md").write_text(
        "---\nname: greeting-etiquette\ndescription: How to greet people\n---\nSay hello first.\n")
    skill = HERMES / "skills" / "research" / "arxiv"
    (skill / "references").mkdir(parents=True, exist_ok=True)
    (skill / "SKILL.md").write_text("---\nname: arxiv\ndescription: Search papers on arXiv\nversion: 1.0.0\n---\n# arXiv\n\n<script>window.skillInjected=true</script> Use the API.\n")
    (skill / "references" / "api.md").write_text("GET /api/query\n")
    bad = HERMES / "skills" / "Bad_Name"
    bad.mkdir(parents=True, exist_ok=True)
    (bad / "SKILL.md").write_text("---\nname: Bad_Name\ndescription: x\n---\n")


fixture()
with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    for width in (1440, 768, 390, 360):
        APPROVALS.unlink(missing_ok=True)
        page = browser.new_page(viewport={"width": width, "height": 1000}, reduced_motion="reduce")
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.add_init_script("localStorage.setItem('sail_owner_token', " + json.dumps(TOKEN) + "); localStorage.removeItem('sail.agents.section')")
        page.route("**/icons.svg", lambda r: r.fulfill(body=(ROOT / "assets/source/icons/sail-icons.svg").read_bytes(), content_type="image/svg+xml"))

        # Old addresses land on the matching section.
        page.goto(BASE + "/#forge")
        expect(page.locator("#subsystemContent #forgeCard")).to_be_visible()
        assert page.evaluate("location.hash") == "#agents?section=coding"
        expect(page.locator("#subsystemTitle")).to_have_text("SAIL Agents")
        expect(page.locator("#tmCrumb")).to_have_text("SAIL Agents")
        tabs = page.locator("#subsystemSections [role=tab]")
        expect(tabs).to_have_text(["Workflows", "Coding", "Skills & plugins", "Market", "Schedule", "Messaging"])
        expect(page.locator("#section-coding")).to_have_attribute("aria-selected", "true")
        assert page.locator("#subsystemSettings").is_visible()  # Forge preferences

        # Keyboard: arrows move between sections.
        page.locator("#section-coding").focus()
        page.keyboard.press("ArrowLeft")
        expect(page.locator("#subsystemContent #flowCard")).to_be_visible()
        assert not page.locator("#forgeCard").is_visible()
        expect(page.locator("#section-workflows")).to_be_focused()
        assert page.evaluate("location.hash") == "#agents?section=workflows"
        expect(page.locator("#flowStatus")).not_to_have_text("Open Flow to load workflows.")
        page.keyboard.press("End")
        expect(page.locator("#subsystemContent #gatewayCard")).to_be_visible()
        page.keyboard.press("ArrowLeft")
        expect(page.locator("#subsystemContent #scheduleCard")).to_be_visible()
        page.keyboard.press("ArrowLeft")
        expect(page.locator("#subsystemContent #marketCard")).to_be_visible()
        page.keyboard.press("ArrowLeft")
        expect(page.locator("#subsystemContent #hermesCard")).to_be_visible()

        # Skills: Hermes home skills, rejected folders, safe text, files.
        expect(page.locator('#hermesSkillList [data-skill="arxiv"]')).to_be_visible()
        expect(page.locator("#hermesRejected summary")).to_have_text("1 skill folder not loaded")
        page.locator('[data-skill="arxiv"]').click()
        expect(page.locator("#hermesSkillBody")).to_contain_text("<script>window.skillInjected=true</script>")
        assert not page.evaluate("Boolean(window.skillInjected)")
        page.get_by_role("button", name="references/api.md").click()
        expect(page.locator("#hermesSkillBody")).to_have_text("GET /api/query\n")
        page.locator("#hermesSkillQuery").fill("nothing-matches")
        expect(page.locator("#hermesSkillList")).to_contain_text("No skill matches")
        page.locator("#hermesSkillQuery").fill("")

        # Plugin: quarantined until approved, then described by the real sandbox.
        plugin = page.locator('[data-plugin="greeter"]')
        expect(plugin).to_contain_text("Not approved")
        expect(plugin.get_by_label("Secrets it may read", exact=False)).to_have_value("GREETER_KEY")
        page.screenshot(path=str(SHOTS / f"agents-skills-{width}.png"), full_page=True)
        plugin.get_by_role("button", name="Approve and enable").click()
        expect(plugin).to_contain_text("Enabled", timeout=20000)
        expect(plugin).to_contain_text("greeter.greet")
        expect(plugin).to_contain_text("greeter./wave")
        expect(plugin).to_contain_text("post_tool_call")
        expect(plugin).to_contain_text("Runs in SAIL")
        expect(plugin).to_contain_text("no network")
        expect(page.locator('#hermesSkillList [data-skill="greeter:greeting-etiquette"]')).to_be_visible()
        assert json.loads(APPROVALS.read_text())["plugins"]["greeter"]["env"] == ["GREETER_KEY"]
        assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), width
        page.screenshot(path=str(SHOTS / f"agents-plugins-{width}.png"), full_page=True)
        plugin.get_by_role("button", name="Revoke approval").click()
        expect(plugin).to_contain_text("Not approved")

        # The section is remembered, and the map's Flow station opens Workflows.
        page.reload()
        page.evaluate("window.sailNavigate('pulse')")
        page.evaluate("window.sailNavigate('agents')")
        expect(page.locator("#subsystemContent #hermesCard")).to_be_visible()
        page.evaluate("window.sailNavigate('flow')")
        expect(page.locator("#subsystemContent #flowCard")).to_be_visible()

        # Service failures are shown, not hidden.
        page.evaluate("window.sailNavigate('agents', 'skills')")
        page.route("**/api/hermes/**", lambda r: r.fulfill(status=503, json={"error": "Plugin store unavailable"}))
        page.locator("#hermesRefresh").click()
        expect(page.locator("#hermesStatus")).to_have_text("Plugin store unavailable")
        page.emulate_media(forced_colors="active")
        assert page.locator("#hermesRefresh").is_visible()
        assert not errors, errors
        print(f"PASS {width}px: sections, legacy routes, keyboard, skills, plugin approve/describe/revoke, errors")
        page.close()
    browser.close()
