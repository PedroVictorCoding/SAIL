# Hermes Agent plugins and Agent Skills in SAIL

**Status:** implemented, first slice of phase P4 in
[External models, gateway, skills and scheduling](SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md#5-skills-flow--skillsmith)
· **Code:** `crates/sail-flow/src/skills.rs`, `crates/sail-flow/src/hermes/`,
`crates/sail-dialogue/src/hermes_tools.rs`

SAIL can use the owner's [Hermes Agent](https://github.com/NousResearch/hermes-agent)
skills and plugins. Skills are text instructions. Plugins are Python code, so SAIL runs
them only after the owner approves them, and only inside a sandbox.

## Skills

A skill is a folder with a `SKILL.md` file, as defined by the
[Agent Skills specification](https://agentskills.io/specification). SAIL looks in these
places, in this order. When two skills have the same name, the first one found is used.

1. `$SAIL_DATA/skills/<name>/SKILL.md`
2. `$HERMES_HOME/skills/` (default `~/.hermes/skills/`). A skill can sit directly in this
   folder or inside one category folder, e.g. `research/arxiv/SKILL.md`.
3. Skills shipped by an enabled plugin, named `<plugin>:<skill>`.

A skill must satisfy the specification to load:

- `name` is 1–64 lowercase letters, digits and hyphens, and matches the folder name.
- `description` is 1–1024 characters.

Fields SAIL does not use (`version`, `metadata.hermes`, ...) are ignored, so Hermes
skills import unchanged. A skill that fails these checks is listed with the reason for
the rejection.

The model sees skills in three steps:

1. `skill_list` returns each skill's name and description.
2. `skill_view` returns a skill's instructions and the list of its files.
3. `skill_view` with `file` returns one of those files.

Files are read as bounded text. A request cannot leave the skill folder or follow a
symbolic link. `allowed-tools` is shown to the model but grants nothing: Guardian still
decides every tool call.

## Plugins

### Where SAIL finds plugins

- `$SAIL_DATA/plugins/hermes/<id>/`
- `$HERMES_HOME/plugins/<id>/` (one category folder is allowed, as in Hermes)

A plugin folder holds `plugin.yaml` and an `__init__.py` that defines `register(ctx)`.
A Hermes portable package holds `plugin.json` and `skills/` and contains no Python.

### Approving a plugin

A plugin starts **quarantined**: SAIL has not run any of its code. The owner enables it
from a terminal on the host. Build the command with
`cargo build --release -p sail-flow --bin sail-hermes`; it uses the same `SAIL_DATA` and
`HERMES_HOME` as the daemon.

```sh
sail-hermes plugins                       # status of every plugin found
sail-hermes approve calc                  # no network, no secrets
sail-hermes approve weather --network --env WEATHER_API_KEY --set units=metric
sail-hermes describe calc                 # what register(ctx) registered
sail-hermes call calc.calculate '{"expression":"6*7"}'
sail-hermes call calc./double 21          # a plugin /command
sail-hermes revoke calc
sail-hermes skills [query]  |  sail-hermes skill <name> [file]
```

An approval records the SHA-256 hash of the plugin's files, in
`$SAIL_DATA/plugins/hermes-approvals.json`. If any file changes, the plugin's status
becomes **changed** and it stops running until the owner approves it again. SAIL also
re-hashes the files immediately before each run.

Secrets go in `$SAIL_DATA/plugins/hermes-env/<id>.env`, one `KEY=value` per line. A key
reaches the plugin only if it was listed with `--env` when the plugin was approved.
`--set key=value` sets the values that `ctx.get_config(key)` returns.

The daemon reads approvals on every call, so it does not need a restart.

### The sandbox

Each request starts a new `bwrap` process. If bubblewrap is missing, the request fails;
SAIL never falls back to running a plugin without the sandbox. Inside the sandbox:

- **Namespaces and privileges:** all namespaces are separate. All capabilities are
  dropped, and the no-new-privileges flag is set.
- **Network:** no network unless the plugin was approved with `--network`.
- **Files:** `/usr` and the system libraries are read-only, and the plugin folder is
  read-only at `/opt/hermes/plugins/<id>`. Its writable home is
  `$SAIL_DATA/plugins/hermes-data/<id>`, mounted at `/home/plugin`. `/tmp` is empty. No
  other host file is visible, including the owner's home, the SAIL data and other plugins.
- **Environment:** cleared. Only `HOME`, `HERMES_HOME`, `PATH`, `LANG` and the approved
  variables are set.
- **Limits:** 120 s of CPU, 4 GiB of address space, 256 MiB per written file, 512 open
  files. Each request has a 60 s wall-clock limit and at most 1 MiB of output.

`/usr/bin/python3 -I` runs the host script, `host.py`. Plugins can import packages
installed system-wide, but not packages from the owner's user site or from a
virtualenv.

### What the host implements

The host implements this part of Hermes' `PluginContext`:

| Supported | Behaviour |
| --- | --- |
| `register_tool(name, toolset, schema, handler, check_fn, override)` | The tool becomes `<plugin>.<tool>`. `check_fn` marks it unavailable. |
| `register_command(name, handler, description, args_hint)` | Callable as `<plugin>./<name>` with raw text. |
| `register_skill(name, path)` | Added to the skill index as `<plugin>:<name>`. |
| `register_hook(...)` | Recorded and listed by `describe`. The hook is **not run**. |
| `dispatch_tool`, `get_config`, `set_config`, `state.get/set/delete`, `plugin_data_dir`, `profile_name`, `plugin_id`, `has_capability` (always false), `has_plugin` (true only for itself), `redact_text`, `session_cwd`, `session_surface` | As in Hermes. State is a JSON file in the plugin's home, at most 10 MiB. |
| `hermes_constants.get_hermes_home()`, `plugins.<id>` and `hermes_plugins.<id>` imports | Shims for helpers that plugins commonly import. |

Other `register_*` calls do nothing and are listed under `unsupported` in `describe`.
These include platforms, model, memory and other providers, middleware and CLI
commands. Calls to `ctx.llm`, `ctx.call_mcp` and messaging raise `NotImplementedError`.

Some plugins are refused outright: those with `kind: platform`, `backend`, `exclusive`,
`model-provider` or `memory`, and plugins that contain symbolic links. Plugins that
import other Hermes internals (`hermes_cli.*`, `agent.*`, `tools.*`) fail with that
import error. This is true of most plugins bundled with Hermes, because they extend
Hermes itself.

### How the model uses plugins

- `hermes_tools` lists the tools and /commands of enabled plugins, with their
  parameters. To build the list, it runs each plugin's `register(ctx)` in the sandbox
  once per approved hash.
- `hermes_call` runs one tool or command. Guardian treats it as a high-risk invocation,
  so **each call asks the owner to approve the exact plugin, tool and arguments**. The
  result is returned to the model labelled as untrusted data.

## Not yet done

- No web UI. Plugins are approved from the `sail-hermes` terminal command.
- Plugin hooks are not run inside SAIL's turn loop.
- Self-created skills are not yet staged for approval.
- The `/<skill>` command is not available in the gateway (P2, not yet built).
- Plugins cannot use a per-plugin Python environment.
