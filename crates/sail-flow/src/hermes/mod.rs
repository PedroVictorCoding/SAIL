//! Hermes Agent plugins inside SAIL.
//!
//! Hermes plugins are Python packages (`plugin.yaml` + `__init__.py` with
//! `register(ctx)`). SAIL runs them the way the external-models design treats every
//! imported extension: quarantined until the owner approves the exact content, then
//! executed only in a bubblewrap sandbox, one short-lived process per request, with no
//! network unless granted, no host home, and only the environment the owner listed.
//!
//! * Discovery: `$SAIL_DATA/plugins/hermes/<id>/` and the owner's Hermes plugins in
//!   `$HERMES_HOME/plugins/<id>/` (one category level allowed, as in Hermes).
//! * Approval: `$SAIL_DATA/plugins/hermes-approvals.json` holds, per plugin, the SHA-256
//!   of its files when it was approved. Any change to the files disables it until it is
//!   approved again. Approving is an owner action (`sail-hermes approve`).
//! * Execution: [`Sandbox`] starts `host.py` under `bwrap`; see that file for the
//!   supported `PluginContext` subset. The model reaches plugin tools only through the
//!   Dialogue tool `hermes:call`, which Guardian gates with exact owner approval.
//! * Hooks and Python environments (round 21): see [`runtime`]. The owner turns on
//!   individual hooks; a plugin with requirements runs with its own venv once built.
//! * Not supported: gateway platforms, memory and model providers, other provider
//!   registrars, `ctx.llm`, `ctx.call_mcp` and messaging.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub mod runtime;

pub const HOST_PY: &str = include_str!("host.py");
const MAX_PLUGIN_FILES: usize = 2000;
const MAX_PLUGIN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_STDOUT: usize = 1024 * 1024;
const MAX_STDERR: usize = 16 * 1024;
/// Hermes plugin kinds SAIL cannot host: they replace parts of Hermes' own runtime.
const UNSUPPORTED_KINDS: [&str; 5] = [
    "platform",
    "exclusive",
    "backend",
    "model-provider",
    "memory",
];

/// The `plugin.yaml` fields SAIL reads. Unknown fields are ignored, as in Hermes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default, deserialize_with = "env_names")]
    pub requires_env: Vec<String>,
    #[serde(default)]
    pub provides_tools: Vec<String>,
}

/// `requires_env` is a list of names or of `{name, description, url, secret}` maps.
fn env_names<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let items = Option::<Vec<serde_norway::Value>>::deserialize(d)?.unwrap_or_default();
    Ok(items
        .into_iter()
        .filter_map(|v| match v {
            serde_norway::Value::String(s) => Some(s),
            serde_norway::Value::Mapping(m) => {
                m.get("name").and_then(|n| n.as_str()).map(String::from)
            }
            _ => None,
        })
        .collect())
}

/// What the owner approved for one plugin.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    /// SHA-256 of the plugin's files at approval time.
    pub digest: String,
    /// Whether the sandbox shares the host network.
    #[serde(default)]
    pub network: bool,
    /// Variables passed from the plugin's env file; nothing else reaches the plugin.
    #[serde(default)]
    pub env: Vec<String>,
    /// Values for `ctx.get_config`.
    #[serde(default)]
    pub settings: serde_json::Map<String, Value>,
    #[serde(default)]
    pub approved_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Approvals {
    #[serde(default)]
    pub plugins: BTreeMap<String, Approval>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum Status {
    /// Found, never approved: its code has not run.
    Quarantined,
    /// Approved earlier, but its files changed since.
    Changed,
    Enabled,
    Unsupported(String),
    Invalid(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Plugin {
    pub id: String,
    pub source: String,
    #[serde(skip)]
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub digest: String,
    /// Portable packages (`plugin.json`, skills only, no Python).
    pub skills_only: bool,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<Approval>,
}

/// What `register(ctx)` registered, reported by the sandboxed host.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Description {
    #[serde(default)]
    pub tools: Vec<ToolInfo>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub commands: Vec<CommandInfo>,
    /// Skill name → `SKILL.md` path relative to the plugin folder.
    #[serde(default)]
    pub skills: BTreeMap<String, String>,
    #[serde(default)]
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub toolset: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default = "yes")]
    pub available: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CommandInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub args_hint: String,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !id.starts_with(['-', '_'])
}

/// SHA-256 over every file's relative path and content. Symlinks make a plugin invalid,
/// so the digest always covers exactly what the sandbox can read.
pub fn digest(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "__pycache__" || name == ".git" || name.ends_with(".pyc") {
                continue;
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err(format!("symbolic link {name} is not allowed"));
            } else if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            } else {
                return Err(format!("{name} is not a regular file"));
            }
            if files.len() > MAX_PLUGIN_FILES {
                return Err("plugin has more than 2000 files".into());
            }
        }
    }
    files.sort();
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut total = 0u64;
    for file in files {
        let rel = file.strip_prefix(dir).map_err(|e| e.to_string())?;
        let bytes = fs::read(&file).map_err(|e| e.to_string())?;
        total += bytes.len() as u64;
        if total > MAX_PLUGIN_BYTES {
            return Err("plugin is larger than 32 MiB".into());
        }
        let rel = rel.to_string_lossy();
        ctx.update(&(rel.len() as u64).to_le_bytes());
        ctx.update(rel.as_bytes());
        ctx.update(&(bytes.len() as u64).to_le_bytes());
        ctx.update(&bytes);
    }
    Ok(ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Where SAIL keeps plugin state.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Ordered `(source, directory)` roots; the first plugin with an id wins.
    pub roots: Vec<(String, PathBuf)>,
    pub approvals: PathBuf,
    /// `<data>/<id>/`: each plugin's writable home inside the sandbox.
    pub data: PathBuf,
    /// `<env>/<id>.env`: owner-written `KEY=VALUE` secrets.
    pub env: PathBuf,
}

impl Paths {
    pub fn default_paths() -> Self {
        let base = sail_common::storage::data_path("plugins");
        let mut roots = vec![("sail".to_string(), base.join("hermes"))];
        if let Some(home) = crate::skills::hermes_home() {
            roots.push(("hermes".into(), home.join("plugins")));
        }
        Self {
            roots,
            approvals: base.join("hermes-approvals.json"),
            data: base.join("hermes-data"),
            env: base.join("hermes-env"),
        }
    }

    pub fn load_approvals(&self) -> Approvals {
        fs::read(&self.approvals)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_approvals(&self, approvals: &Approvals) -> Result<(), String> {
        if let Some(parent) = self.approvals.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.approvals.with_extension("json.tmp");
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(approvals).map_err(|e| e.to_string())?)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.approvals).map_err(|e| e.to_string())
    }

    /// Every plugin folder under the roots, with its current status.
    pub fn discover(&self) -> Vec<Plugin> {
        let approvals = self.load_approvals();
        let mut found: BTreeMap<String, Plugin> = BTreeMap::new();
        for (source, root) in &self.roots {
            let Ok(entries) = fs::read_dir(root) else {
                continue;
            };
            let mut dirs: Vec<_> = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect();
            dirs.sort();
            for dir in dirs {
                let candidates = if is_plugin_dir(&dir) {
                    vec![dir]
                } else {
                    // One category level, as Hermes allows.
                    fs::read_dir(&dir)
                        .map(|it| {
                            let mut v: Vec<_> = it
                                .flatten()
                                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                                .map(|e| e.path())
                                .filter(|p| is_plugin_dir(p))
                                .collect();
                            v.sort();
                            v
                        })
                        .unwrap_or_default()
                };
                for dir in candidates {
                    let plugin = inspect(&dir, source, &approvals);
                    found.entry(plugin.id.clone()).or_insert(plugin);
                }
            }
        }
        found.into_values().collect()
    }

    /// Record the owner's approval of a plugin's current files.
    pub fn approve(
        &self,
        id: &str,
        network: bool,
        env: Vec<String>,
        settings: serde_json::Map<String, Value>,
    ) -> Result<Plugin, String> {
        let plugin = self
            .discover()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("no plugin {id}"))?;
        match &plugin.status {
            Status::Unsupported(reason) | Status::Invalid(reason) => {
                return Err(format!("{id} cannot be enabled: {reason}"));
            }
            _ => {}
        }
        if let Some(bad) = env.iter().find(|name| !valid_env_name(name)) {
            return Err(format!("invalid environment variable name {bad}"));
        }
        let mut approvals = self.load_approvals();
        approvals.plugins.insert(
            id.into(),
            Approval {
                digest: plugin.digest.clone(),
                network,
                env,
                settings,
                approved_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        self.save_approvals(&approvals)?;
        self.discover()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("no plugin {id}"))
    }

    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        let mut approvals = self.load_approvals();
        let removed = approvals.plugins.remove(id).is_some();
        self.save_approvals(&approvals)?;
        Ok(removed)
    }

    /// The approved variables from `<env>/<id>.env`; other lines are ignored.
    fn env_for(&self, id: &str, approval: &Approval) -> Vec<(String, String)> {
        let Ok(text) = fs::read_to_string(self.env.join(format!("{id}.env"))) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| {
                let line = line.trim();
                let line = line.strip_prefix("export ").unwrap_or(line);
                let (key, value) = line.split_once('=')?;
                let key = key.trim();
                let value = value.trim().trim_matches('"').trim_matches('\'');
                approval
                    .env
                    .iter()
                    .any(|allowed| allowed == key)
                    .then(|| (key.to_string(), value.to_string()))
            })
            .collect()
    }
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
        // The sandbox sets these itself.
        && !["HOME", "PATH", "LANG", "HERMES_HOME", "LD_PRELOAD", "LD_LIBRARY_PATH", "PYTHONPATH"]
            .contains(&name)
}

fn is_plugin_dir(dir: &Path) -> bool {
    dir.join("plugin.yaml").is_file()
        || dir.join("plugin.yml").is_file()
        || dir.join("plugin.json").is_file()
}

fn inspect(dir: &Path, source: &str, approvals: &Approvals) -> Plugin {
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut plugin = Plugin {
        id: id.clone(),
        source: source.into(),
        dir: dir.into(),
        manifest: Manifest::default(),
        digest: String::new(),
        skills_only: false,
        status: Status::Quarantined,
        approval: approvals.plugins.get(&id).cloned(),
    };
    let invalid = |mut p: Plugin, why: String| {
        p.status = Status::Invalid(why);
        p
    };
    if !valid_id(&id) {
        return invalid(plugin, "folder name is not a valid plugin id".into());
    }
    let yaml = ["plugin.yaml", "plugin.yml"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file());
    let manifest = if let Some(path) = yaml {
        fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_norway::from_str::<Manifest>(&t).map_err(|e| e.to_string()))
    } else {
        fs::read_to_string(dir.join("plugin.json"))
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Manifest>(&t).map_err(|e| e.to_string()))
    };
    match manifest {
        Ok(m) => plugin.manifest = m,
        Err(e) => return invalid(plugin, format!("manifest: {e}")),
    }
    plugin.skills_only = !dir.join("__init__.py").is_file();
    match digest(dir) {
        Ok(d) => plugin.digest = d,
        Err(e) => return invalid(plugin, e),
    }
    if let Some(kind) = plugin.manifest.kind.as_deref()
        && UNSUPPORTED_KINDS.contains(&kind)
    {
        plugin.status = Status::Unsupported(format!(
            "{kind} plugins replace part of Hermes' own runtime"
        ));
        return plugin;
    }
    plugin.status = match &plugin.approval {
        None => Status::Quarantined,
        Some(a) if a.digest == plugin.digest => Status::Enabled,
        Some(_) => Status::Changed,
    };
    plugin
}

/// Runs `host.py` for one plugin inside bubblewrap.
#[derive(Debug, Clone)]
pub struct Sandbox {
    pub bwrap: PathBuf,
    pub python: PathBuf,
    /// Where `host.py` is written before use (outside every plugin folder).
    pub runtime: PathBuf,
    pub timeout: Duration,
}

impl Sandbox {
    pub fn default_sandbox() -> Self {
        Self {
            bwrap: std::env::var_os("SAIL_BWRAP_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/usr/bin/bwrap".into()),
            python: "/usr/bin/python3".into(),
            runtime: sail_common::storage::data_path("plugins").join("runtime"),
            timeout: Duration::from_secs(60),
        }
    }

    fn host_path(&self) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.runtime).map_err(|e| e.to_string())?;
        let path = self.runtime.join("hermes_host.py");
        if fs::read_to_string(&path).ok().as_deref() != Some(HOST_PY) {
            let tmp = self.runtime.join("hermes_host.py.tmp");
            fs::write(&tmp, HOST_PY).map_err(|e| e.to_string())?;
            fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        }
        Ok(path)
    }

    /// The `bwrap` argument list. Public for tests and for the docs.
    pub fn args(
        &self,
        plugin: &Plugin,
        approval: &Approval,
        host: &Path,
        data: &Path,
        env: &[(String, String)],
    ) -> Vec<String> {
        self.args_with(plugin, approval, host, data, env, None)
    }

    /// [`Sandbox::args`], running with the plugin's own Python environment when given
    /// (mounted read-only at [`runtime::VENV_MOUNT`]).
    pub fn args_with(
        &self,
        plugin: &Plugin,
        approval: &Approval,
        host: &Path,
        data: &Path,
        env: &[(String, String)],
        venv: Option<&Path>,
    ) -> Vec<String> {
        let s = |p: &Path| p.to_string_lossy().to_string();
        let mut args: Vec<String> = [
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--clearenv",
        ]
        .map(String::from)
        .to_vec();
        if approval.network {
            args.push("--share-net".into());
        }
        args.extend(["--ro-bind", "/usr", "/usr"].map(String::from));
        for dir in ["/bin", "/lib", "/lib64", "/sbin"] {
            match fs::read_link(dir) {
                Ok(target) => args.extend(["--symlink".into(), s(&target), dir.into()]),
                Err(_) if Path::new(dir).is_dir() => {
                    args.extend(["--ro-bind".into(), dir.into(), dir.into()])
                }
                Err(_) => {}
            }
        }
        let mut etc = vec![
            "/etc/ld.so.cache",
            "/etc/ssl",
            "/etc/ca-certificates",
            "/etc/alternatives",
            "/etc/localtime",
            "/etc/mime.types",
        ];
        if approval.network {
            etc.extend([
                "/etc/resolv.conf",
                "/etc/hosts",
                "/etc/nsswitch.conf",
                "/etc/gai.conf",
                "/run/systemd/resolve",
            ]);
        }
        for path in etc {
            args.extend(["--ro-bind-try", path, path].map(String::from));
        }
        let plugin_dir = format!("/opt/hermes/plugins/{}", plugin.id);
        args.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"].map(String::from));
        args.extend(["--ro-bind".into(), s(&plugin.dir), plugin_dir.clone()]);
        args.extend(["--ro-bind".into(), s(host), "/opt/hermes/host.py".into()]);
        args.extend(["--bind".into(), s(data), "/home/plugin".into()]);
        if let Some(venv) = venv {
            args.extend(["--ro-bind".into(), s(venv), runtime::VENV_MOUNT.into()]);
        }
        for (key, value) in [
            ("HOME", "/home/plugin"),
            ("HERMES_HOME", "/home/plugin"),
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "C.UTF-8"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .chain(env.iter().cloned())
        {
            args.extend(["--setenv".into(), key, value]);
        }
        args.extend(["--chdir".into(), "/home/plugin".into(), "--".into()]);
        let python = match venv {
            Some(_) => format!("{}/bin/python", runtime::VENV_MOUNT),
            None => s(&self.python),
        };
        args.extend([
            python,
            "-I".into(),
            "-B".into(),
            "/opt/hermes/host.py".into(),
        ]);
        args
    }

    /// Run one request (`op` and its fields) for an enabled plugin.
    pub async fn run(
        &self,
        paths: &Paths,
        plugin: &Plugin,
        mut request: Value,
    ) -> Result<Value, String> {
        if plugin.status != Status::Enabled {
            return Err(format!("plugin {} is not enabled", plugin.id));
        }
        let approval = plugin.approval.clone().ok_or("plugin is not approved")?;
        if !self.bwrap.is_file() {
            return Err(format!(
                "bubblewrap ({}) is required to run Hermes plugins",
                self.bwrap.display()
            ));
        }
        // Re-hash right before running: files changed since discovery do not run.
        if digest(&plugin.dir)? != approval.digest {
            return Err(format!(
                "plugin {} changed since it was approved",
                plugin.id
            ));
        }
        let host = self.host_path()?;
        let data = paths.data.join(&plugin.id);
        fs::create_dir_all(&data).map_err(|e| e.to_string())?;
        let env = paths.env_for(&plugin.id, &approval);
        let venv = paths.ready_venv(plugin);
        request["plugin_id"] = json!(plugin.id);
        request["plugin_dir"] = json!(format!("/opt/hermes/plugins/{}", plugin.id));
        request["settings"] = Value::Object(approval.settings.clone());
        let mut command = tokio::process::Command::new(&self.bwrap);
        command
            .args(self.args_with(plugin, &approval, &host, &data, &env, venv.as_deref()))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for (resource, value) in [
                    (libc::RLIMIT_CPU, 120u64),
                    (libc::RLIMIT_AS, 4 << 30),
                    (libc::RLIMIT_FSIZE, 256 << 20),
                    (libc::RLIMIT_NOFILE, 512),
                    (libc::RLIMIT_CORE, 0),
                ] {
                    let bound = libc::rlimit {
                        rlim_cur: value,
                        rlim_max: value,
                    };
                    if libc::setrlimit(resource, &bound) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start the plugin sandbox: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("sandbox stdin")?;
        let mut stdout = child.stdout.take().ok_or("sandbox stdout")?;
        let mut stderr = child.stderr.take().ok_or("sandbox stderr")?;
        let input = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        let work = async {
            stdin.write_all(&input).await.map_err(|e| e.to_string())?;
            drop(stdin);
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut out_reader = (&mut stdout).take(MAX_STDOUT as u64 + 1);
            let mut err_reader = (&mut stderr).take(MAX_STDERR as u64);
            let (a, b) = tokio::join!(
                out_reader.read_to_end(&mut out),
                err_reader.read_to_end(&mut err)
            );
            a.map_err(|e| e.to_string())?;
            let _ = b;
            let status = child.wait().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((out, err, status))
        };
        let (out, err, status) = tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| format!("plugin {} timed out", plugin.id))??;
        if out.len() > MAX_STDOUT {
            return Err("plugin output is larger than 1 MiB".into());
        }
        let line = String::from_utf8_lossy(&out);
        let reply: Value = line
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str(l).ok())
            .ok_or_else(|| {
                let tail = String::from_utf8_lossy(&err);
                let tail: String = tail
                    .chars()
                    .rev()
                    .take(600)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("plugin sandbox exited with {status} and no result: {tail}")
            })?;
        if reply["ok"] == json!(true) {
            Ok(reply)
        } else {
            Err(reply["error"]
                .as_str()
                .unwrap_or("plugin failed")
                .to_string())
        }
    }
}

/// Plugin id → (digest it was described at, what `register(ctx)` registered).
type DescribeCache = BTreeMap<String, (String, Result<Description, String>)>;

/// Discovery, the sandbox and a per-digest cache of what each plugin registered.
pub struct Manager {
    pub paths: Paths,
    pub sandbox: Sandbox,
    described: Mutex<DescribeCache>,
    /// Environment builds in progress or failed, per plugin id.
    building: Mutex<BTreeMap<String, runtime::BuildState>>,
}

impl Manager {
    pub fn new(paths: Paths, sandbox: Sandbox) -> Self {
        Self {
            paths,
            sandbox,
            described: Mutex::new(BTreeMap::new()),
            building: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn enabled(&self) -> Vec<Plugin> {
        self.paths
            .discover()
            .into_iter()
            .filter(|p| p.status == Status::Enabled)
            .collect()
    }

    /// What an enabled plugin registered; runs `register(ctx)` once per approved digest.
    pub async fn describe(&self, plugin: &Plugin) -> Result<Description, String> {
        if plugin.skills_only {
            return Ok(Description {
                skills: portable_skills(&plugin.dir),
                ..Default::default()
            });
        }
        if let Some((digest, cached)) = self.described.lock().unwrap().get(&plugin.id)
            && *digest == plugin.digest
        {
            return cached.clone();
        }
        let result = self
            .sandbox
            .run(&self.paths, plugin, json!({"op":"describe"}))
            .await
            .and_then(|v| serde_json::from_value::<Description>(v).map_err(|e| e.to_string()));
        self.described
            .lock()
            .unwrap()
            .insert(plugin.id.clone(), (plugin.digest.clone(), result.clone()));
        result
    }

    /// Every enabled plugin with what it registered (or why that failed).
    pub async fn catalog(&self) -> Vec<(Plugin, Result<Description, String>)> {
        let mut out = Vec::new();
        for plugin in self.enabled() {
            let description = self.describe(&plugin).await;
            out.push((plugin, description));
        }
        out
    }

    /// Add enabled plugins' skills to `library` as `<plugin>:<skill>`.
    pub async fn add_skills(&self, library: &mut crate::skills::SkillLibrary) {
        for (plugin, description) in self.catalog().await {
            let Ok(description) = description else {
                continue;
            };
            for rel in description.skills.values() {
                if let Some(dir) = Path::new(rel).parent() {
                    library.add(
                        &plugin.dir.join(dir),
                        &format!("plugin:{}", plugin.id),
                        Some(&plugin.id),
                    );
                }
            }
        }
    }

    /// Run `<plugin>.<tool>` with a JSON object, or `<plugin>./<command>` with raw text.
    pub async fn call(&self, name: &str, arguments: &str) -> Result<Value, String> {
        let (id, target) = name
            .split_once('.')
            .ok_or("name must be <plugin>.<tool> or <plugin>./<command>")?;
        let plugin = self
            .paths
            .discover()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("no plugin {id}"))?;
        let request = if let Some(command) = target.strip_prefix('/') {
            json!({"op":"command","command":command,"raw_args":arguments})
        } else {
            let args: Value = if arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(arguments)
                    .map_err(|e| format!("arguments must be a JSON object: {e}"))?
            };
            if !args.is_object() {
                return Err("arguments must be a JSON object".into());
            }
            json!({"op":"call","tool":target,"args":args})
        };
        let reply = self.sandbox.run(&self.paths, &plugin, request).await?;
        let text = reply["result"].as_str().unwrap_or_default();
        let result = serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!(text));
        Ok(json!({"plugin":id,"result":result,"truncated":reply["truncated"]}))
    }
}

fn portable_skills(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Ok(entries) = fs::read_dir(dir.join("skills")) {
        for entry in entries.flatten() {
            if entry.path().join("SKILL.md").is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                out.insert(name.clone(), format!("skills/{name}/SKILL.md"));
            }
        }
    }
    out
}

/// The daemon's shared manager.
pub fn global() -> &'static Manager {
    static MANAGER: std::sync::OnceLock<Manager> = std::sync::OnceLock::new();
    MANAGER.get_or_init(|| Manager::new(Paths::default_paths(), Sandbox::default_sandbox()))
}

#[cfg(test)]
mod tests;
