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
use super::{Bundle, Decision, Finding, Kind, LARGE_FILE_BYTES, Report, Severity, Trust, Verdict};
use regex::Regex;
use std::sync::LazyLock;

pub const SCANNER_VERSION: &str = "sail-guard-1";

/// At most one finding per rule per line, and this many per rule per file.
const PER_FILE_CAP: usize = 20;
/// A bundle with more files than this gets one `too_many_files` finding.
const MANY_FILES: usize = 500;

enum Matcher {
    Regex(Regex),
    /// Zero-width and tag characters, with the file-start BOM exception.
    Invisible,
    /// Package managers called without a version pin.
    UnpinnedInstall,
    /// `git clone` or a `curl`/`wget` download that is not piped into a shell.
    RemoteFetch,
    /// No line matching; used for whole-file structural findings.
    Never,
}

struct Rule {
    id: &'static str,
    severity: Severity,
    category: &'static str,
    matcher: Matcher,
    /// Blobs under `assets/` are expected to be large and opaque.
    skip_assets: bool,
    /// Only the first 8 characters of the match may be shown.
    redact: bool,
}

impl Rule {
    fn insensitive(
        id: &'static str,
        severity: Severity,
        category: &'static str,
        pattern: &str,
    ) -> Self {
        // Every pattern below is a static literal; a bad one is a build-time defect.
        let regex = Regex::new(&format!("(?i){pattern}")).expect("static scanner pattern");
        Self::with_regex(id, severity, category, regex)
    }

    fn sensitive(
        id: &'static str,
        severity: Severity,
        category: &'static str,
        pattern: &str,
    ) -> Self {
        let regex = Regex::new(pattern).expect("static scanner pattern");
        Self::with_regex(id, severity, category, regex)
    }

    fn with_regex(
        id: &'static str,
        severity: Severity,
        category: &'static str,
        regex: Regex,
    ) -> Self {
        Self {
            id,
            severity,
            category,
            matcher: Matcher::Regex(regex),
            skip_assets: false,
            redact: false,
        }
    }

    fn custom(
        id: &'static str,
        severity: Severity,
        category: &'static str,
        matcher: Matcher,
    ) -> Self {
        Self {
            id,
            severity,
            category,
            matcher,
            skip_assets: false,
            redact: false,
        }
    }

    fn assets(mut self) -> Self {
        self.skip_assets = true;
        self
    }

    fn redacted(mut self) -> Self {
        self.redact = true;
        self
    }
}

/// One table for the whole scanner: adding a regex rule is one line.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    use Severity::{Critical, High, Medium};
    vec![
        // exfiltration
        Rule::insensitive(
            "env_exfil_http",
            Critical,
            "exfiltration",
            r"(?:(?:curl|wget|fetch\s*\(|requests\.|httpx\.).*(?:\$[A-Za-z_][A-Za-z0-9_]*|\$\{[^}]*\}|os\.environ|process\.env)|(?:\$[A-Za-z_][A-Za-z0-9_]*|\$\{[^}]*\}|os\.environ|process\.env).*(?:curl|wget|fetch\s*\(|requests\.|httpx\.))",
        ),
        Rule::insensitive(
            "read_secrets_file",
            Critical,
            "exfiltration",
            r"(?:~/\.ssh/|\bid_rsa\b|\bid_ed25519\b|~/\.aws/credentials|\.netrc\b|~/\.hermes/\.env|~/\.config/gh/hosts\.yml|\.git-credentials)",
        ),
        Rule::insensitive(
            "dump_all_env",
            High,
            "exfiltration",
            r"(?:(?:^|[;&|]\s*)(?:printenv|env)\b|dict\s*\(\s*os\.environ\s*\)|json\.dumps\s*\(\s*os\.environ|JSON\.stringify\s*\(\s*process\.env)",
        ),
        Rule::sensitive(
            "md_image_exfil",
            High,
            "exfiltration",
            r"!?\[[^\]]*\]\([^)\s]*\?[^)\s]*(?:\{\{|\$\{|\$[A-Z])",
        ),
        Rule::insensitive(
            "dns_exfil",
            Critical,
            "exfiltration",
            r"\b(?:nslookup|dig|host)\b[^\n]*\$[A-Za-z0-9_{]",
        ),
        // injection
        Rule::insensitive(
            "prompt_injection_ignore",
            Critical,
            "injection",
            r"\bignore\s+(?:all\s+)?(?:previous|prior|above)\s+(?:instructions|rules|prompts)\b",
        ),
        Rule::insensitive(
            "sys_prompt_override",
            Critical,
            "injection",
            r"(?:\b(?:new|updated|override)\s+system\s+prompt\b|you\s+are\s+now\s+(?:in\s+)?(?:developer|dan|jailbreak)\s+mode)",
        ),
        Rule::insensitive(
            "bypass_restrictions",
            Critical,
            "injection",
            r"\b(?:bypass|disable|ignore)\s+(?:your\s+|the\s+)?(?:safety|guardrails?|restrictions?|content\s+policy)",
        ),
        Rule::insensitive(
            "deception_hide",
            High,
            "injection",
            r"\b(?:do\s+not|don't|never)\s+(?:tell|inform|reveal\s+to|mention\s+to)\s+the\s+user\b",
        ),
        Rule::insensitive(
            "leak_system_prompt",
            High,
            "injection",
            r"\b(?:reveal|print|output|repeat)\s+(?:your|the)\s+(?:system\s+prompt|instructions)\b",
        ),
        Rule::insensitive(
            "html_comment_injection",
            High,
            "injection",
            r"<!--[^>]*(?:ignore|instruction|system|assistant|you\s+must)[^>]*-->",
        ),
        Rule::custom(
            "invisible_unicode",
            High,
            "injection",
            Matcher::Invisible,
        ),
        // destructive
        Rule::insensitive(
            "destructive_rm",
            Critical,
            "destructive",
            r"\brm\s+(?:(?:-[A-Za-z]*[rR][A-Za-z]*f[A-Za-z]*|-[A-Za-z]*f[A-Za-z]*[rR][A-Za-z]*)|(?:-[rR]\s+-f|-f\s+-[rR])|(?:--recursive\s+--force|--force\s+--recursive))\b[^\n]*?(?:^|\s)(?:/\*?|~/?|\$HOME\b|/home\b|/etc\b|/usr\b)(?:\s|$|/)",
        ),
        Rule::insensitive(
            "disk_overwrite",
            Critical,
            "destructive",
            r"(?:\bdd\b[^\n]*\bof=/dev/|\bmkfs(?:\.\w+)?\b|>\s*/dev/(?:sd|nvme))",
        ),
        Rule::insensitive("python_rmtree", High, "destructive", r"\bshutil\.rmtree\s*\("),
        Rule::insensitive(
            "insecure_perms",
            Medium,
            "destructive",
            r"\bchmod\s+(?:-R\s+)?777\b|\bchmod\s+a\+w\b",
        ),
        // execution
        Rule::insensitive(
            "curl_pipe_shell",
            Critical,
            "execution",
            r"\b(?:curl|wget)\b[^\n|]*\|\s*(?:sudo\s+)?(?:sh|bash|zsh|python3?|perl|ruby|node)\b",
        ),
        Rule::insensitive(
            "python_os_system",
            High,
            "execution",
            r"(?:\bos\.system\s*\(|\bsubprocess\.[^\n]*\bshell\s*=\s*True)",
        ),
        Rule::sensitive(
            "node_child_process",
            High,
            "execution",
            r#"(?i)(?:require\s*\(\s*['"]child_process['"]|from\s+['"]child_process['"]|\bchild_process\b|\bexecSync\s*\()"#,
        ),
        Rule::sensitive(
            "eval_string",
            High,
            "execution",
            r#"(?i)(?:\b(?:eval|exec)\s*\(\s*['"`]|\bbase64\b|\batob\s*\(|\bbytes\.fromhex\b|\bcodecs\.decode\b|\bcompile\s*\()"#,
        ),
        Rule::custom(
            "unpinned_install",
            Medium,
            "execution",
            Matcher::UnpinnedInstall,
        ),
        Rule::custom("remote_fetch", Medium, "execution", Matcher::RemoteFetch),
        // network
        Rule::insensitive(
            "reverse_shell",
            Critical,
            "network",
            r"(?:/dev/tcp/|\bnc\s+-e\b|\bncat\s+-e\b|bash\s+-i\s*>&|\bsocket\.socket\s*\([^\n]*(?:\bsubprocess\b|\bpty\.spawn\b)|\bmkfifo\b[^\n]*\bnc\b)",
        ),
        Rule::insensitive(
            "tunnel_service",
            High,
            "network",
            r"\b(?:ngrok|cloudflared\s+tunnel|localtunnel|serveo\.net|localhost\.run)\b",
        ),
        Rule::insensitive(
            "exfil_service",
            High,
            "network",
            r"(?:webhook\.site|requestbin|pipedream\.net|interact\.sh|burpcollaborator|transfer\.sh|pastebin\.com/api)",
        ),
        // persistence
        Rule::insensitive(
            "ssh_backdoor",
            Critical,
            "persistence",
            r"(?:authorized_keys[^\n]*(?:>>?|\btee\b)|(?:>>?|\btee\b)[^\n]*authorized_keys)",
        ),
        Rule::sensitive(
            "sudoers_mod",
            Critical,
            "persistence",
            r"(?:/etc/sudoers|NOPASSWD)",
        ),
        Rule::insensitive(
            "agent_config_mod",
            Critical,
            "persistence",
            r"(?:(?:>>?|\btee\b)[^\n]*(?:~/\.hermes/config|\.claude/settings|\.codex/config|~/\.bashrc|~/\.profile|~/\.zshrc)|(?:~/\.hermes/config|\.claude/settings|\.codex/config|~/\.bashrc|~/\.profile|~/\.zshrc)[^\n]*(?:>>?|\btee\b)|\bcrontab\s+-)",
        ),
        // obfuscation
        Rule::insensitive(
            "base64_decode_pipe",
            High,
            "obfuscation",
            r"\bbase64\s+(?:-d|--decode)\b[^\n]*\|\s*(?:sh|bash|zsh|python3?|perl|ruby|node)\b",
        ),
        Rule::sensitive(
            "hex_blob",
            Medium,
            "obfuscation",
            r"(?:[A-Za-z0-9+/]{400,}|[A-Fa-f0-9]{400,})",
        )
        .assets(),
        // credentials
        Rule::sensitive(
            "embedded_private_key",
            Critical,
            "credentials",
            r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----",
        ),
        Rule::sensitive(
            "leaked_token",
            Critical,
            "credentials",
            r"(?:\b(?:ghp_|gho_|ghs_|ghu_)[A-Za-z0-9]{36}\b|\bgithub_pat_[A-Za-z0-9_]{22,}\b|\bsk-ant-[A-Za-z0-9_-]{20,}\b|\bsk-[A-Za-z0-9]{40,}\b|\bAKIA[A-Z0-9]{16}\b|\bglpat-[A-Za-z0-9_-]{20}\b)",
        )
        .redacted(),
    ]
});

static GIT_CLONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bgit\s+clone\b").expect("static scanner pattern"));
static CURL_WGET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:curl|wget)\b").expect("static scanner pattern"));

fn in_assets(path: &str) -> bool {
    path.starts_with("assets/") || path.contains("/assets/")
}

fn is_text(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok() && !bytes.iter().take(8 * 1024).any(|byte| *byte == 0)
}

fn is_invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0xFEFF
            | 0xE0000..=0xE007F
    )
}

fn invisible_in(line: &str, file_start: bool) -> Option<char> {
    line.chars()
        .enumerate()
        .find(|(index, c)| !(file_start && *index == 0 && *c == '\u{feff}') && is_invisible(*c))
        .map(|(_, c)| c)
}

fn excerpt(rule: &Rule, matched: &str) -> String {
    if rule.redact {
        let mut out: String = matched.chars().take(8).collect();
        out.push('…');
        return out;
    }
    let clipped: String = matched.chars().take(160).collect();
    clipped
        .chars()
        .map(|c| {
            if c.is_control() && c != '\t' {
                '�'
            } else {
                c
            }
        })
        .collect()
}

fn finding(rule: &Rule, file: &str, line: usize, matched: &str) -> Finding {
    Finding {
        rule: rule.id.to_string(),
        severity: rule.severity,
        category: rule.category.to_string(),
        file: file.to_string(),
        line,
        excerpt: excerpt(rule, matched),
    }
}

fn is_local_install_target(token: &str) -> bool {
    token == "-r"
        || token.starts_with("--requirement")
        || token.starts_with('.')
        || token.starts_with('/')
        || token.starts_with('~')
        || token.starts_with("file:")
        || token.starts_with("git+")
        || token.contains("://")
}

fn token_has_pin(token: &str, npm: bool) -> bool {
    if npm {
        if let Some(rest) = token.strip_prefix('@') {
            rest.contains('@')
        } else {
            token.contains('@')
        }
    } else {
        token.contains("==")
    }
}

fn unpinned_install(line: &str) -> Option<String> {
    static COMMAND: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:pip3?\s+install|npm\s+(?:install|i)|npx)\b")
            .expect("static scanner pattern")
    });
    let command = COMMAND.find(line)?;
    let npm = !line[command.start()..]
        .to_ascii_lowercase()
        .starts_with("pip");
    let rest = line[command.end()..].trim_start();
    let mut packages = Vec::new();
    for raw in rest.split_whitespace() {
        let token = raw
            .trim_matches(['"', '\''])
            .trim_end_matches([';', ',', ')']);
        if matches!(token, "|" | "&" | "&&" | ";")
            || token.starts_with('|')
            || token.starts_with(';')
        {
            break;
        }
        if token.is_empty() || token.starts_with('-') {
            if token == "-r" || token.starts_with("--requirement") {
                return None;
            }
            continue;
        }
        if is_local_install_target(token) {
            continue;
        }
        packages.push(token);
    }
    if packages.is_empty() || packages.iter().all(|token| token_has_pin(token, npm)) {
        return None;
    }
    Some(line[command.start()..].chars().take(160).collect())
}

fn remote_fetch(line: &str) -> Option<String> {
    if let Some(clone) = GIT_CLONE.find(line) {
        return Some(clone.as_str().to_string());
    }
    let command = CURL_WGET.find(line)?;
    let rest = &line[command.end()..];
    if rest.contains('|') || !(rest.contains("http://") || rest.contains("https://")) {
        return None;
    }
    Some(rest.trim().chars().take(160).collect())
}

fn binary_description(path: &str) -> &str {
    path.rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("non-text")
}

fn binary_severity(path: &str, bytes: &[u8]) -> Severity {
    let lower = path.to_ascii_lowercase();
    let executable = [".so", ".dylib", ".dll", ".exe", ".bin", ".pyc"]
        .iter()
        .any(|extension| lower.ends_with(extension))
        || bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"MZ");
    if executable {
        Severity::High
    } else {
        Severity::Medium
    }
}

pub fn scan(bundle: &Bundle, kind: Kind) -> Report {
    let rules = &*RULES;
    let mut findings = Vec::new();
    let mut files_scanned = 0;
    if bundle.files.len() > MANY_FILES {
        let rule = Rule::custom(
            "too_many_files",
            Severity::Medium,
            "structure",
            Matcher::Never,
        );
        findings.push(finding(
            &rule,
            "",
            0,
            &format!("{} files", bundle.files.len()),
        ));
    }
    for (path, bytes) in &bundle.files {
        files_scanned += 1;
        if bytes.len() as u64 > LARGE_FILE_BYTES {
            let rule = Rule::custom(
                "oversized_file",
                Severity::Medium,
                "structure",
                Matcher::Never,
            );
            findings.push(finding(&rule, path, 0, &format!("{} bytes", bytes.len())));
        }
        if !is_text(bytes) {
            if !in_assets(path) {
                let rule = Rule::custom(
                    "binary_file",
                    binary_severity(path, bytes),
                    "structure",
                    Matcher::Never,
                );
                findings.push(finding(&rule, path, 0, binary_description(path)));
            }
            continue;
        }
        // `is_text` proved the bytes are UTF-8.
        let text = std::str::from_utf8(bytes).unwrap_or_default();
        let mut counts = vec![0usize; rules.len()];
        for (index, line) in text.split('\n').enumerate() {
            let line_number = index + 1;
            let line = line.strip_suffix('\r').unwrap_or(line);
            let file_start = index == 0;
            for (rule_index, rule) in rules.iter().enumerate() {
                if counts[rule_index] >= PER_FILE_CAP || (rule.skip_assets && in_assets(path)) {
                    continue;
                }
                let matched = match &rule.matcher {
                    Matcher::Regex(regex) => regex.find(line).map(|m| m.as_str().to_string()),
                    Matcher::Invisible => {
                        invisible_in(line, file_start).map(|c| format!("U+{:04X}", c as u32))
                    }
                    Matcher::UnpinnedInstall => unpinned_install(line),
                    Matcher::RemoteFetch => remote_fetch(line),
                    Matcher::Never => None,
                };
                if let Some(matched) = matched {
                    findings.push(finding(rule, path, line_number, &matched));
                    counts[rule_index] += 1;
                }
            }
        }
    }
    let has_entrypoint = match kind {
        Kind::Skill => bundle.files.contains_key("SKILL.md"),
        Kind::Plugin => {
            bundle.files.contains_key("plugin.yaml") || bundle.files.contains_key("plugin.json")
        }
    };
    if !has_entrypoint {
        let rule = Rule::custom(
            "missing_entrypoint",
            Severity::High,
            "structure",
            Matcher::Never,
        );
        let wanted = match kind {
            Kind::Skill => "SKILL.md",
            Kind::Plugin => "plugin.yaml or plugin.json",
        };
        findings.push(finding(&rule, "", 0, wanted));
    }
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.rule.cmp(&b.rule))
    });
    let verdict = verdict_of(&findings);
    Report {
        verdict,
        findings,
        files_scanned,
        scanner: SCANNER_VERSION.to_string(),
    }
}

pub fn decide(trust: Trust, report: &Report) -> (Decision, String) {
    let decision = match (trust, report.verdict) {
        (Trust::Builtin, Verdict::Dangerous) => Decision::NeedsAck,
        (Trust::Trusted, Verdict::Dangerous) | (Trust::AgentCreated, Verdict::Dangerous) => {
            Decision::Block
        }
        (Trust::Community, Verdict::Dangerous) => Decision::Block,
        (Trust::Community | Trust::AgentCreated, Verdict::Caution) => Decision::NeedsAck,
        _ => Decision::Allow,
    };
    let reason = match decision {
        Decision::Allow => String::new(),
        _ => {
            let trust = match trust {
                Trust::Builtin => "Builtin",
                Trust::Trusted => "Trusted",
                Trust::Community => "Community",
                Trust::AgentCreated => "Agent-created",
            };
            let verdict = match report.verdict {
                Verdict::Safe => "safe",
                Verdict::Caution => "caution",
                Verdict::Dangerous => "dangerous",
            };
            let top = report
                .findings
                .first()
                .map(|finding| format!(" ({})", finding.rule))
                .unwrap_or_default();
            let ending = if decision == Decision::Block {
                "cannot be installed."
            } else {
                "confirm to install."
            };
            format!("{trust} item with {verdict} findings{top}: {ending}")
        }
    };
    (decision, reason)
}

/// Verdict from findings alone (see "Verdict").
pub fn verdict_of(report_findings: &[super::Finding]) -> Verdict {
    if report_findings
        .iter()
        .any(|finding| finding.severity == Severity::Critical)
    {
        Verdict::Dangerous
    } else if report_findings
        .iter()
        .any(|finding| finding.severity == Severity::High)
    {
        Verdict::Caution
    } else {
        Verdict::Safe
    }
}
