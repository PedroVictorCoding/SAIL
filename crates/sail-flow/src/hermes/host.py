"""SAIL host for Hermes Agent plugins.

Runs inside SAIL's bubblewrap sandbox, one process per request. It loads one plugin
package, calls its ``register(ctx)`` with a PluginContext that implements the subset of
the Hermes plugin API SAIL supports, performs one operation and prints one JSON object
on the original stdout. Everything the plugin prints goes to stderr.

Request (stdin, JSON):
    {"op": "describe" | "call" | "command",
     "plugin_id": str, "plugin_dir": str, "settings": {...},
     "tool": str, "args": {...},          # op == "call"
     "command": str, "raw_args": str}     # op == "command"

The sandbox, not this file, is the security boundary.
"""

import importlib.machinery
import importlib.util
import inspect
import json
import os
import sys
import traceback

MAX_RESULT = 64 * 1024
STATE_LIMIT = 10 * 1024 * 1024
OUT = sys.stdout
sys.stdout = sys.stderr


def emit(obj):
    OUT.write(json.dumps(obj, ensure_ascii=False, default=str))
    OUT.write("\n")
    OUT.flush()


class State:
    """``ctx.state``: a small JSON store kept in the plugin's private data directory."""

    def __init__(self, path):
        self._path = path
        try:
            with open(path, encoding="utf-8") as handle:
                self._data = json.load(handle)
            if not isinstance(self._data, dict):
                self._data = {}
        except (OSError, ValueError):
            self._data = {}

    def get(self, key, default=None):
        return self._data.get(key, default)

    def set(self, key, value):
        data = dict(self._data)
        data[key] = value
        self._write(data)

    def delete(self, key):
        if key in self._data:
            data = dict(self._data)
            del data[key]
            self._write(data)

    def _write(self, data):
        encoded = json.dumps(data)
        if len(encoded) > STATE_LIMIT:
            raise ValueError("plugin state exceeds 10 MiB")
        tmp = self._path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as handle:
            handle.write(encoded)
        os.replace(tmp, self._path)
        self._data = data


class PluginContext:
    """The part of Hermes' ``PluginContext`` that SAIL implements.

    Registrations SAIL cannot honour (platforms, providers, middleware, ...) are recorded
    and reported, so ``register`` still succeeds. Runtime services SAIL does not provide
    (``ctx.llm``, ``ctx.call_mcp``, messaging) raise ``NotImplementedError`` when used.
    """

    def __init__(self, plugin_id, plugin_dir, settings, data_dir):
        self.plugin_id = plugin_id
        self.profile_name = "sail"
        self._dir = plugin_dir
        self._settings = dict(settings or {})
        self._data_dir = data_dir
        self.tools = {}
        self.hooks = []
        self.commands = {}
        self.skills = {}
        self.unsupported = []
        self.state = State(os.path.join(data_dir, "state.json"))

    # Tools, hooks, commands -------------------------------------------------------
    def register_tool(self, name=None, toolset=None, schema=None, handler=None,
                      check_fn=None, override=False, **extra):
        schema = dict(schema or {})
        name = name or schema.get("name")
        if not name or handler is None:
            raise ValueError("register_tool needs a name and a handler")
        available = True
        if check_fn is not None:
            try:
                available = bool(check_fn())
            except Exception:  # noqa: BLE001 - a failing check means unavailable
                available = False
        self.tools[name] = {
            "name": name,
            "toolset": toolset or self.plugin_id,
            "description": str(schema.get("description") or extra.get("description") or ""),
            "parameters": schema.get("parameters") or {"type": "object", "properties": {}},
            "available": available,
            "override": bool(override),
            "handler": handler,
        }

    def register_hook(self, hook_name, callback, hold_stream=False):
        self.hooks.append(str(hook_name))

    def register_command(self, name, handler, description="", args_hint=""):
        self.commands[str(name).lstrip("/")] = {
            "description": description,
            "args_hint": args_hint,
            "handler": handler,
        }

    def register_skill(self, name, skill_md_path):
        path = os.path.realpath(os.path.join(self._dir, skill_md_path))
        root = os.path.realpath(self._dir)
        if os.path.commonpath([path, root]) != root:
            raise ValueError("plugin skills must live inside the plugin directory")
        self.skills[str(name)] = os.path.relpath(path, root)

    def dispatch_tool(self, name, args, *, parent_agent=None):
        tool = self.tools.get(name)
        if tool is None:
            return json.dumps({"error": f"tool {name} is not provided by this plugin"})
        return run_handler(tool["handler"], args)

    # Config and state -------------------------------------------------------------
    def get_config(self, key, default=None):
        return self._settings.get(key, default)

    def set_config(self, key, value):
        self._settings[key] = value

    def plugin_data_dir(self):
        return self._data_dir

    def has_capability(self, capability_id):
        return False

    def has_plugin(self, name):
        return name == self.plugin_id

    def redact_text(self, text, force=False):
        return text

    def session_cwd(self):
        return self._data_dir

    def session_surface(self):
        return "sail"

    def current_cron_execution(self):
        return None

    def __getattr__(self, name):
        if name.startswith("register_"):
            def record(*_args, **_kwargs):
                self.unsupported.append(name)
            return record
        if name.startswith("_"):
            raise AttributeError(name)

        def unavailable(*_args, **_kwargs):
            raise NotImplementedError(f"SAIL's Hermes plugin host does not provide ctx.{name}")
        return unavailable


def run_handler(handler, args):
    # Hermes calls handler(args, **kwargs); SAIL passes no extra keyword arguments.
    result = handler(args)
    if inspect.isawaitable(result):
        import asyncio
        result = asyncio.run(result)
    return result


def package(name):
    if name not in sys.modules:
        module = importlib.util.module_from_spec(
            importlib.machinery.ModuleSpec(name, None, is_package=True))
        module.__path__ = []
        sys.modules[name] = module
    return sys.modules[name]


def install_shims():
    """Stand-ins for the few Hermes core helpers that plugins commonly import."""
    try:
        importlib.import_module("hermes_constants")
    except ImportError:
        import pathlib
        import types
        shim = types.ModuleType("hermes_constants")
        shim.get_hermes_home = lambda: pathlib.Path(os.environ.get("HERMES_HOME", "/home/plugin"))
        sys.modules["hermes_constants"] = shim


def load(plugin_id, plugin_dir):
    entry = os.path.join(plugin_dir, "__init__.py")
    if not os.path.isfile(entry):
        return None
    install_shims()
    short = plugin_id.replace("-", "_")
    name = "plugins." + short
    package("plugins")
    spec = importlib.util.spec_from_file_location(
        name, entry, submodule_search_locations=[plugin_dir])
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    # Hermes loads user plugins as hermes_plugins.<name>; some refer to themselves so.
    package("hermes_plugins")
    sys.modules["hermes_plugins." + short] = module
    spec.loader.exec_module(module)
    register = getattr(module, "register", None)
    if not callable(register):
        raise RuntimeError("plugin has no register(ctx) function")
    return register


def bounded(text):
    if not isinstance(text, str):
        text = json.dumps(text, ensure_ascii=False, default=str)
    if len(text) > MAX_RESULT:
        return text[:MAX_RESULT], True
    return text, False


def main():
    request = json.loads(sys.stdin.read() or "{}")
    plugin_id = request["plugin_id"]
    plugin_dir = request["plugin_dir"]
    data_dir = os.environ.get("HOME", "/tmp")
    os.makedirs(data_dir, exist_ok=True)
    ctx = PluginContext(plugin_id, plugin_dir, request.get("settings"), data_dir)
    register = load(plugin_id, plugin_dir)
    if register is not None:
        register(ctx)
    op = request.get("op")
    if op == "describe":
        emit({
            "ok": True,
            "tools": [{k: v for k, v in t.items() if k != "handler"} for t in ctx.tools.values()],
            "hooks": sorted(set(ctx.hooks)),
            "commands": [{"name": n, "description": c["description"], "args_hint": c["args_hint"]}
                         for n, c in ctx.commands.items()],
            "skills": ctx.skills,
            "unsupported": sorted(set(ctx.unsupported)),
        })
    elif op == "call":
        tool = ctx.tools.get(request.get("tool"))
        if tool is None:
            emit({"ok": False, "error": f"unknown tool {request.get('tool')}"})
            return
        if not tool["available"]:
            emit({"ok": False, "error": "the plugin reports this tool is unavailable"})
            return
        text, truncated = bounded(run_handler(tool["handler"], request.get("args") or {}))
        emit({"ok": True, "result": text, "truncated": truncated})
    elif op == "command":
        command = ctx.commands.get(str(request.get("command", "")).lstrip("/"))
        if command is None:
            emit({"ok": False, "error": f"unknown command {request.get('command')}"})
            return
        text, truncated = bounded(command["handler"](request.get("raw_args") or ""))
        emit({"ok": True, "result": "" if text is None else text, "truncated": truncated})
    else:
        emit({"ok": False, "error": f"unknown op {op}"})


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001 - report every plugin failure as data
        traceback.print_exc()
        emit({"ok": False, "error": f"{type(error).__name__}: {error}"[:2000]})
