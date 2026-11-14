"""Tests for host.py's describe and hook operations. Stdlib only; run with -I."""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

HOST = Path(__file__).resolve().with_name("host.py")
PYTHON = "/usr/bin/python3"

PLUGIN = '''
def register(ctx):
    def pre_llm(**kwargs):
        return "Be concise."

    def pre_tool(**kwargs):
        return {"action": "block", "message": "no"}

    def post_tool(**kwargs):
        raise ValueError("boom")

    ctx.register_hook("pre_llm_call", pre_llm)
    ctx.register_hook("pre_tool_call", pre_tool)
    ctx.register_hook("post_tool_call", post_tool)
    ctx.register_tool(
        name="ping",
        schema={"description": "Ping", "parameters": {"type": "object"}},
        handler=lambda args: "pong",
    )
    ctx.register_command("hello", lambda raw: "hi " + raw, description="greet")
    print("this goes to stderr, not stdout")
'''


class HostTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.plugin_dir = Path(self.temp.name) / "demo"
        self.plugin_dir.mkdir()
        (self.plugin_dir / "__init__.py").write_text(PLUGIN, encoding="utf-8")
        (self.plugin_dir / "plugin.yaml").write_text("name: demo\n", encoding="utf-8")

    def tearDown(self):
        self.temp.cleanup()

    def run_host(self, request):
        completed = subprocess.run(
            [PYTHON, "-I", str(HOST)],
            input=json.dumps(request),
            text=True,
            capture_output=True,
            timeout=20,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        return json.loads(completed.stdout)

    def request(self, **extra):
        return {
            "plugin_id": "demo",
            "plugin_dir": str(self.plugin_dir),
            "settings": {},
            **extra,
        }

    def test_describe_lists_sorted_hooks_and_tools(self):
        reply = self.run_host(self.request(op="describe"))
        self.assertTrue(reply["ok"])
        self.assertEqual(
            reply["hooks"], ["post_tool_call", "pre_llm_call", "pre_tool_call"]
        )
        self.assertEqual([tool["name"] for tool in reply["tools"]], ["ping"])
        self.assertEqual([command["name"] for command in reply["commands"]], ["hello"])

    def test_pre_llm_hook_returns_text(self):
        reply = self.run_host(
            self.request(
                op="hook",
                payload={
                    "hook": "pre_llm_call",
                    "session_id": "s1",
                    "user_message": "hello",
                    "is_first_turn": True,
                    "platform": "web",
                },
            )
        )
        self.assertEqual(reply, {"ok": True, "results": ["Be concise."]})

    def test_pre_tool_hook_keeps_structured_block(self):
        reply = self.run_host(
            self.request(
                op="hook",
                payload={
                    "hook": "pre_tool_call",
                    "session_id": "s1",
                    "tool_name": "shell",
                    "args": {"command": "ls"},
                },
            )
        )
        self.assertEqual(
            reply,
            {"ok": True, "results": [{"action": "block", "message": "no"}]},
        )

    def test_a_raising_callback_becomes_an_error_slot(self):
        reply = self.run_host(
            self.request(
                op="hook",
                payload={
                    "hook": "post_tool_call",
                    "session_id": "s1",
                    "tool_name": "shell",
                    "args": {},
                    "ok": True,
                    "result_preview": "done",
                },
            )
        )
        self.assertTrue(reply["ok"])
        self.assertEqual(reply["results"][0]["error"], "ValueError: boom")

    def test_unknown_hook_is_an_error(self):
        reply = self.run_host(
            self.request(op="hook", payload={"hook": "not_a_hook", "session_id": "s1"})
        )
        self.assertFalse(reply["ok"])
        self.assertIn("unknown hook", reply["error"])


if __name__ == "__main__":
    unittest.main()
