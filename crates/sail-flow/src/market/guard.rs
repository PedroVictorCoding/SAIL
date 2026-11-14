//! Install-time scanner and install policy (round 20, **Kilo's slice**). Pure: no I/O,
//! no clock. **This module doc is the spec.** The rule set follows Hermes' "skills guard"
//! (`tools/skills_guard.py`) so the owner sees the findings Hermes users see; the
//! patterns are SAIL's own.
//!
//! # Scan ([`scan`])
//!
//! Every file of the bundle is scanned. A file is **text** when it is valid UTF-8 and has
//! no NUL byte in its first 8 KiB; other files get only the structural rules. Text is
//! scanned line by line (1-based line numbers); a rule fires at most **once per line**
//! and at most **20 times per file** (later matches are dropped). Matching is
//! case-insensitive unless the rule says otherwise. Excerpts: the matched text (not the
//! whole line), at most 160 characters, control characters other than tab → `�`.
//!
//! ## Rules (id · severity · category · what fires it)
//!
//! **exfiltration**
//! * `env_exfil_http` · Critical — a `curl`/`wget`/`fetch(`/`requests.`/`httpx.` call on the
//!   same line as an environment variable reference (`$VAR`, `${VAR}`, `os.environ`,
//!   `process.env`).
//! * `read_secrets_file` · Critical — reads of `~/.ssh/`, `id_rsa`, `id_ed25519`,
//!   `~/.aws/credentials`, `.netrc`, `~/.hermes/.env`, `~/.config/gh/hosts.yml`, `.git-credentials`.
//! * `dump_all_env` · High — `printenv` or `env` as a whole command, `os.environ` passed
//!   whole (`dict(os.environ)`, `json.dumps(os.environ`), `process.env` serialized
//!   (`JSON.stringify(process.env`).
//! * `md_image_exfil` · High — a Markdown image or link whose URL has a query string
//!   containing `{{`, `${`, or `$` followed by a capital letter (templated secret).
//! * `dns_exfil` · Critical — `nslookup`/`dig`/`host` with `$` substitution in the name.
//!
//! **injection** (text aimed at the model)
//! * `prompt_injection_ignore` · Critical — "ignore (all )?(previous|prior|above)
//!   (instructions|rules|prompts)".
//! * `sys_prompt_override` · Critical — "(new|updated|override) system prompt", "you are
//!   now (in )?(developer|dan|jailbreak) mode".
//! * `bypass_restrictions` · Critical — "(bypass|disable|ignore) (your |the )?(safety|
//!   guardrails|restrictions|content policy)".
//! * `deception_hide` · High — "(do not|don't|never) (tell|inform|reveal to|mention to)
//!   the user".
//! * `leak_system_prompt` · High — "(reveal|print|output|repeat) (your|the) (system
//!   prompt|instructions)".
//! * `html_comment_injection` · High — an HTML comment `<!-- ... -->` on one line that
//!   contains any of: ignore, instruction, system, assistant, you must.
//! * `invisible_unicode` · High — any of U+200B–U+200F, U+202A–U+202E, U+2060–U+2064,
//!   U+FEFF (not at file start), U+E0000–U+E007F (tags). Case-sensitive by nature.
//!   Excerpt: `U+XXXX`.
//!
//! **destructive**
//! * `destructive_rm` · Critical — `rm -rf` (any flag order, `-fr`, `--recursive --force`)
//!   targeting `/`, `/*`, `~`, `~/`, `$HOME`, `/home`, `/etc`, `/usr`.
//! * `disk_overwrite` · Critical — `dd ... of=/dev/`, `mkfs`, `> /dev/sd`, `> /dev/nvme`.
//! * `python_rmtree` · High — `shutil.rmtree(`.
//! * `insecure_perms` · Medium — `chmod (-R )?777`, `chmod a+w`.
//!
//! **execution** (supply chain)
//! * `curl_pipe_shell` · Critical — `curl`/`wget` piped (`|`) into `sh`, `bash`, `zsh`,
//!   `python`, `python3`, `perl`, `ruby`, `node`, optionally through `sudo`.
//! * `python_os_system` · High — `os.system(`, `subprocess.` with `shell=True` on the same line.
//! * `node_child_process` · High — `child_process` (require/import) or `execSync(`.
//! * `eval_string` · High — `eval(` or `exec(` whose argument starts with a string,
//!   `base64`, `atob(`, `bytes.fromhex`, `codecs.decode`, or `compile(`.
//! * `unpinned_install` · Medium — `pip install <pkg>` / `npm install <pkg>` / `npx <pkg>`
//!   where the package has no version pin (`==`, `@x.y`) and is not a local path or `-r`.
//! * `remote_fetch` · Medium — `git clone` or `curl`/`wget` of a URL (not piped).
//!
//! **network**
//! * `reverse_shell` · Critical — `/dev/tcp/`, `nc -e`, `ncat -e`, `bash -i >&`,
//!   `socket.socket(` together with `subprocess`/`pty.spawn` on the same line, `mkfifo`
//!   with `nc`.
//! * `tunnel_service` · High — `ngrok`, `cloudflared tunnel`, `localtunnel`, `serveo.net`,
//!   `localhost.run`.
//! * `exfil_service` · High — `webhook.site`, `requestbin`, `pipedream.net`, `interact.sh`,
//!   `burpcollaborator`, `transfer.sh`, `pastebin.com/api`.
//!
//! **persistence**
//! * `ssh_backdoor` · Critical — writes to `authorized_keys` (`>>`, `tee`, `echo ... >`).
//! * `sudoers_mod` · Critical — `/etc/sudoers` or `NOPASSWD` (case-sensitive).
//! * `agent_config_mod` · Critical — writes to `~/.hermes/config`, `.claude/settings`,
//!   `.codex/config`, `~/.bashrc`, `~/.profile`, `~/.zshrc`, `crontab -`.
//!
//! **obfuscation**
//! * `base64_decode_pipe` · High — `base64 -d`/`--decode` piped into a shell or
//!   interpreter.
//! * `hex_blob` · Medium — a single run of 400+ base64 or hex characters in a text file
//!   that is not under `assets/`.
//!
//! **credentials** (case-sensitive patterns)
//! * `embedded_private_key` · Critical — `-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----`.
//! * `leaked_token` · Critical — `ghp_`/`gho_`/`ghs_`/`ghu_` + 36 alphanumerics;
//!   `github_pat_` + 22+; `sk-ant-` + 20+; `sk-` + 40+ alphanumerics; `AKIA` + 16
//!   uppercase/digits; `glpat-` + 20. The excerpt shows only the first 8 characters
//!   followed by `…` — never the whole secret.
//!
//! **structure** (whole-file, line 0)
//! * `binary_file` · Medium — a non-text file outside `assets/` (High if its name ends in
//!   `.so`, `.dylib`, `.dll`, `.exe`, `.bin`, `.pyc`, or it starts with `\x7fELF`/`MZ`).
//! * `oversized_file` · Medium — larger than [`super::LARGE_FILE_BYTES`].
//! * `too_many_files` · Medium — more than 500 files (once, file = "").
//! * `missing_entrypoint` · High — a skill bundle without `SKILL.md` at its root, or a
//!   plugin bundle with neither `plugin.yaml` nor `plugin.json` at its root. (The caller
//!   passes the kind.)
//!
//! ## Verdict
//!
//! `Dangerous` if any Critical finding, `Caution` if any High (and no Critical), else
//! `Safe` (Medium and Low findings are listed but do not change the verdict). Findings
//! are sorted by severity descending, then file, line, rule. `scanner` is
//! [`SCANNER_VERSION`]. The same bundle always yields an identical report.
//!
//! # Policy ([`decide`])
//!
//! | trust \ verdict | safe | caution | dangerous |
//! | --- | --- | --- | --- |
//! | Builtin | Allow | Allow | NeedsAck |
//! | Trusted | Allow | Allow | Block |
//! | Community | Allow | NeedsAck | Block |
//! | AgentCreated | Allow | NeedsAck | Block |
//!
//! (Hermes blocks community `caution`; SAIL lets the owner install past it with an
//! explicit acknowledgement, because installing runs nothing and plugins still need the
//! hash-bound approval. `dangerous` is never installable except for builtin skills that
//! ship with Hermes itself, and then only with acknowledgement.) The returned reason is
//! empty for `Allow`, otherwise one sentence naming the verdict, the trust level and the
//! top finding's rule, e.g. "Community item with caution findings (python_os_system):
//! confirm to install."
use super::{Bundle, Decision, Kind, Report, Trust, Verdict};

pub const SCANNER_VERSION: &str = "sail-guard-1";

pub fn scan(bundle: &Bundle, kind: Kind) -> Report {
    let _ = (bundle, kind);
    unimplemented!("Kilo, round 20")
}

pub fn decide(trust: Trust, report: &Report) -> (Decision, String) {
    let _ = (trust, report);
    unimplemented!("Kilo, round 20")
}

/// Verdict from findings alone (see "Verdict").
pub fn verdict_of(report_findings: &[super::Finding]) -> Verdict {
    let _ = report_findings;
    unimplemented!("Kilo, round 20")
}
