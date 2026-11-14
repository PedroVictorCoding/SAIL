# Hermes Agent plugins and Agent Skills in SAIL

**Status:** implemented: skills, sandboxed plugins, the Market and proposed skills, phase P4 in
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
in the web interface, under **SAIL Agents › Skills & plugins** (`#agents?section=skills`).
That section lists each plugin with its status, its file hash and what it asks for. It
approves the reviewed hash with or without network access and secrets, shows what an
enabled plugin registered, and revokes approvals. It also lists and reads skills,
including folders that were rejected and why. The approval request carries the hash the
owner reviewed. If the files changed in the meantime, the request is refused, so the
owner never approves code they did not see. The page uses the owner-token API
`/api/hermes` (`crates/sail-dialogue/src/hermes_http.rs`).

The same actions are available from a terminal on the host. Build the command with
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
| `register_hook(name, callback)` | Listed by `describe`. Four hooks can run, once you turn them on (see [Hooks](#hooks)). |
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

### Hooks

883 of the 2,309 catalog plugins register hooks: code Hermes calls around each model
call or tool call. SAIL runs four of them, **only for hooks you turn on**, per plugin,
in the plugin's detail on **Skills & plugins** (all are off by default):

| Hook | What the plugin sees | What it can do |
| --- | --- | --- |
| `pre_llm_call` | Your message for this turn, whether it is the first turn | Add up to 2,000 characters of context. It is scanned first; anything that looks like an injected instruction, an exfiltration link or hidden Unicode is dropped. The model is told it is data, not instructions. |
| `pre_tool_call` | The tool's name and arguments | Block the call, with a short message the model sees. |
| `post_tool_call` | The tool's name, arguments, success and a 1,000-character preview of the result | Nothing (it only observes). |
| `post_llm_call` | Your message and SAIL's reply | Nothing (it only observes). |

Plugins never see the conversation history or memory. Each hook call runs in the same
sandbox as tools, with a 3-second budget; a slow or failing plugin is skipped and the
turn continues. Other hooks (`on_session_start`, gateway and middleware hooks) are listed
as "Not run in SAIL". The settings are kept in `$SAIL_DATA/plugins/hermes-hooks.json`.

### Python environments

A plugin that lists packages in `requirements.txt` (or `pyproject.toml`'s
`[project] dependencies`) gets its own virtual environment when you ask for it: **Build**
in the plugin's detail. The packages are part of the files you approved, so the plugin
must be approved first.

- The build runs in bubblewrap with network on, only during the build, and installs
  **wheels only** (`pip --only-binary=:all:`), so no package build scripts run.
- Lines SAIL can't install safely are refused and shown: editable installs, `-r`
  includes, options, URLs and VCS links, local paths, more than 50 requirements.
- What was installed (name, version, wheel SHA-256 and URL) is recorded in
  `$SAIL_DATA/plugins/hermes-pyenv/<id>/env.json`, and shown under the plugin.
- The plugin then runs with that environment's interpreter, mounted read-only. When its
  requirements change, the environment shows **Changed** and the plugin runs without it
  until you rebuild.

## Market

**SAIL Agents › Market** (`#agents?section=market`) installs skills and plugins from the
public catalogs that Hermes Agent publishes, so nothing has to be copied by hand. From a
terminal: `sail-hermes market refresh | search | stage | install | discard | installed |
uninstall`. The code is `crates/sail-flow/src/market/`.

| Catalog | Source | Contents |
| --- | --- | --- |
| Skills index | `hermes-agent.nousresearch.com/docs/api/skills-index.json` | About 104,000 skills: Hermes' optional skills, curated GitHub repositories, skills.sh, ClawHub, LobeHub and browse.sh |
| Plugin catalog | `hermes-agent.nousresearch.com/docs/api/plugin-catalog.json` | About 2,300 human-reviewed plugins, each pinned to an exact commit |

1. **Download the market.** SAIL downloads both catalogs (about 50 MB), keeps a compact
   copy in `$SAIL_DATA/market/catalog.json` and refreshes it when it is older than six
   hours. If a refresh fails, the cached copy stays.
2. **Search.** Search runs on the local copy, so no search text leaves the machine.
3. **Review.** SAIL downloads the item into `$SAIL_DATA/market/quarantine/` and shows its
   files, its content hash, a scan report and its `SKILL.md` or README. Files come from:
   - GitHub, at a fixed commit: a plugin's reviewed SHA, or for skills the head commit at
     the time of review;
   - ClawHub, where each file must match the SHA-256 that ClawHub publishes.

   No downloaded file is run. Skills from LobeHub and browse.sh, and plugins hosted
   outside GitHub, are listed but cannot be installed yet.
4. **Scan.** The scanner looks for exfiltration of secrets, prompt injection, destructive
   commands, `curl | sh` and other remote execution, reverse shells, persistence,
   obfuscation, embedded credentials and binaries. Its verdict and the item's trust level
   decide what happens next:

   | Trust | Safe | Caution | Dangerous |
   | --- | --- | --- | --- |
   | Hermes built-in | install | install | install after confirming |
   | Trusted | install | install | blocked |
   | Community | install | install after confirming | blocked |
   | Proposed by SAIL | install | install after confirming | blocked |

5. **Install.** SAIL hashes the quarantined files again and moves them into place:
   - a skill goes to `$SAIL_DATA/skills/<name>/`;
   - a plugin goes to `$SAIL_DATA/plugins/hermes/<id>/`.

   **An installed plugin is still quarantined.** Approve it under Skills & plugins, as
   described above, before any of its code runs. The hash shown at review is the one you
   approve.

`$SAIL_DATA/market/lock.json` records where each install came from. Uninstall removes
only what the market installed. If you edited the files after installing, you must
confirm the uninstall. Requests send only the item's public identifier and the
user agent `SAIL-market/1`. To raise GitHub's anonymous limit of 60 requests an hour,
set `SAIL_MARKET_GITHUB_TOKEN`.

## Skills proposed by SAIL

After a task that took several steps, the model can write the procedure down as a new
skill with `skill_propose`. The proposal is scanned like a community item and waits in
`$SAIL_DATA/skills-proposed/`. It appears under **Skills & plugins → Proposed by SAIL**
with SAIL's reason, and it becomes a skill only when you approve it. The model is told
that the skill is waiting for review, not that it was saved. A proposal never
overwrites an existing skill. From a terminal: `sail-hermes proposals` and `sail-hermes
proposal approve|reject <id>`.

## Not yet done

- Market: no update notifications, no custom GitHub taps, and no installs from GitLab, LobeHub or browse.sh.
