//! Plugin hooks and per-plugin Python environments at run time (round 21).
//!
//! * **Hooks.** The owner turns on individual hooks per plugin (off by default). They are
//!   kept in `hermes-hooks.json` next to the approvals. [`Manager::run_hooks`] calls every
//!   enabled plugin that registered the hook, each in its own sandbox with a short budget;
//!   a slow or failing plugin is skipped, never waited for. What a plugin may return and
//!   what SAIL does with it is decided by [`super::hooks::interpret`].
//! * **Environments.** A plugin's `requirements.txt` (or `pyproject.toml` dependencies)
//!   is part of the files the owner approved. [`Manager::build_env`] creates a virtual
//!   environment for it inside bubblewrap, with network only during that build and wheels
//!   only (no package build scripts). The plugin then runs with that environment's
//!   interpreter. The record of what was installed (names, versions, wheel hashes) is
//!   `hermes-pyenv/<id>/env.json`.
use super::{
    Manager, Paths, Plugin, Status,
    hooks::{self, HookEffect, HookPayload},
    pyenv::{self, EnvPlan, EnvRecord, EnvState},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

/// Each hook call gets this long; the turn never waits for more.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(3);
/// An environment build (venv + pip download) may take this long.
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(600);
/// Where a plugin's environment is mounted inside the sandbox (build and run alike, so
/// the paths recorded inside the venv stay valid).
pub const VENV_MOUNT: &str = "/opt/hermes/venv";

/// Owner-enabled hooks: plugin id → hook names.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HookSettings {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub plugins: BTreeMap<String, Vec<String>>,
}

/// One plugin's environment as the API shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EnvView {
    /// `not_needed`, `needed`, `building`, `ready`, `changed`, `refused` or `failed`.
    pub state: String,
    pub source: String,
    pub requirements: Vec<String>,
    pub refused: Vec<String>,
    pub locked: Vec<Value>,
    pub error: Option<String>,
}

/// An environment build in progress or the last failure, per plugin id.
#[derive(Debug, Clone, PartialEq)]
pub enum BuildState {
    Building,
    Failed(String),
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

impl Paths {
    /// `hermes-hooks.json`, next to the approvals file.
    pub fn hooks_file(&self) -> PathBuf {
        self.approvals.with_file_name("hermes-hooks.json")
    }

    /// `hermes-pyenv/`, next to the approvals file: one folder per plugin.
    pub fn pyenv_root(&self) -> PathBuf {
        self.approvals.with_file_name("hermes-pyenv")
    }

    /// The enabled hooks. A missing file means none; a corrupt one is an error, so
    /// callers fail closed (no hooks run) and nothing overwrites it silently.
    pub fn load_hooks(&self) -> Result<HookSettings, String> {
        match fs::read(self.hooks_file()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("hermes-hooks.json is unreadable: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HookSettings::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Turn one hook on or off for one plugin (owner action).
    pub fn set_hook(&self, id: &str, hook: &str, enabled: bool) -> Result<HookSettings, String> {
        if !super::valid_id(id) {
            return Err("invalid plugin id".into());
        }
        if !hooks::SUPPORTED_HOOKS.contains(&hook) {
            return Err(format!("SAIL does not run the {hook} hook"));
        }
        let mut settings = self.load_hooks()?;
        settings.version = 1;
        let list = settings.plugins.entry(id.to_string()).or_default();
        list.retain(|h| h != hook);
        if enabled {
            list.push(hook.to_string());
            list.sort();
        }
        if list.is_empty() {
            settings.plugins.remove(id);
        }
        write_atomic(
            &self.hooks_file(),
            &serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?,
        )?;
        Ok(settings)
    }

    /// The record written after a successful build, if any.
    pub fn env_record(&self, id: &str) -> Option<EnvRecord> {
        let bytes = fs::read(self.pyenv_root().join(id).join("env.json")).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The venv to run `plugin` with: only when it was built for the current requirements.
    pub fn ready_venv(&self, plugin: &Plugin) -> Option<PathBuf> {
        let plan = pyenv::plan(&plugin.dir).ok()?;
        let record = self.env_record(&plugin.id);
        let venv = self.pyenv_root().join(&plugin.id).join("venv");
        (pyenv::state(&plan, record.as_ref()) == EnvState::Ready && venv.is_dir()).then_some(venv)
    }

    /// Remove a plugin's environment (owner action, or on uninstall).
    pub fn remove_env(&self, id: &str) -> Result<(), String> {
        if !super::valid_id(id) {
            return Err("invalid plugin id".into());
        }
        let dir = self.pyenv_root().join(id);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&dir).map_err(|e| e.to_string()),
            Ok(_) => Err("the environment folder is not a directory".into()),
            Err(_) => Ok(()),
        }
    }
}

fn env_state_name(state: &EnvState) -> &'static str {
    match state {
        EnvState::NotNeeded => "not_needed",
        EnvState::Needed => "needed",
        EnvState::Ready => "ready",
        EnvState::Changed => "changed",
        EnvState::Refused => "refused",
    }
}

fn requirement_line(r: &pyenv::Requirement) -> String {
    let mut line = r.name.clone();
    if !r.extras.is_empty() {
        line.push_str(&format!("[{}]", r.extras.join(",")));
    }
    line.push_str(&r.specifier);
    if !r.marker.is_empty() {
        line.push_str(&format!("; {}", r.marker));
    }
    line
}

impl Manager {
    /// The plugin's hooks as the API shows them: everything it registered, whether SAIL
    /// runs that hook, and whether the owner turned it on.
    pub fn hook_views(&self, plugin: &Plugin, registered: &[String]) -> Vec<Value> {
        let enabled = self
            .paths
            .load_hooks()
            .ok()
            .and_then(|s| s.plugins.get(&plugin.id).cloned())
            .unwrap_or_default();
        registered
            .iter()
            .map(|name| {
                let supported = hooks::SUPPORTED_HOOKS.contains(&name.as_str());
                json!({
                    "name": name,
                    "supported": supported,
                    "enabled": supported && enabled.contains(name),
                })
            })
            .collect()
    }

    /// The plugin's Python environment as the API shows it.
    pub fn env_view(&self, plugin: &Plugin) -> EnvView {
        let plan = pyenv::plan(&plugin.dir).unwrap_or_else(|e| EnvPlan {
            source: String::new(),
            requirements: Vec::new(),
            refused: vec![e],
            hash: String::new(),
        });
        let record = self.paths.env_record(&plugin.id);
        let mut state = env_state_name(&pyenv::state(&plan, record.as_ref())).to_string();
        let mut error = None;
        match self.building.lock().unwrap().get(&plugin.id) {
            Some(BuildState::Building) => state = "building".into(),
            Some(BuildState::Failed(e)) if state != "ready" => {
                state = "failed".into();
                error = Some(e.clone());
            }
            _ => {}
        }
        EnvView {
            state,
            source: plan.source.clone(),
            requirements: plan.requirements.iter().map(requirement_line).collect(),
            refused: plan.refused.clone(),
            locked: record
                .map(|r| {
                    r.locked
                        .iter()
                        .map(|l| json!({"name": l.name, "version": l.version}))
                        .collect()
                })
                .unwrap_or_default(),
            error,
        }
    }

    /// Mark a build as started. False when one is already running for this plugin.
    pub fn begin_build(&self, id: &str) -> bool {
        let mut building = self.building.lock().unwrap();
        if building.get(id) == Some(&BuildState::Building) {
            return false;
        }
        building.insert(id.to_string(), BuildState::Building);
        true
    }

    /// Build (or rebuild) `plugin`'s environment. The owner asked for it; the plugin must
    /// be approved and enabled, since its requirements are part of what they approved.
    /// Call [`Manager::begin_build`] first; this records the outcome either way.
    pub async fn build_env(&self, plugin: &Plugin) -> Result<EnvRecord, String> {
        let result = self.build_env_inner(plugin).await;
        let mut building = self.building.lock().unwrap();
        match &result {
            Ok(_) => {
                building.remove(&plugin.id);
            }
            Err(e) => {
                building.insert(plugin.id.clone(), BuildState::Failed(e.clone()));
            }
        }
        result
    }

    async fn build_env_inner(&self, plugin: &Plugin) -> Result<EnvRecord, String> {
        if plugin.status != Status::Enabled {
            return Err(format!(
                "approve plugin {} before building its environment",
                plugin.id
            ));
        }
        let plan = pyenv::plan(&plugin.dir)?;
        match pyenv::state(&plan, self.paths.env_record(&plugin.id).as_ref()) {
            EnvState::NotNeeded => return Err("this plugin lists no Python packages".into()),
            EnvState::Refused => {
                return Err(format!(
                    "these requirement lines can't be installed: {}",
                    plan.refused.join("; ")
                ));
            }
            _ => {}
        }
        if !self.sandbox.bwrap.is_file() {
            return Err("bubblewrap is required to build plugin environments".into());
        }
        let root = self.paths.pyenv_root().join(&plugin.id);
        let new = root.join("venv.new");
        let build = root.join("build");
        for dir in [&new, &build] {
            if dir.exists() {
                fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
            }
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let venv = Path::new(VENV_MOUNT);
        let report = Path::new("/opt/hermes/build/report.json");
        let python = self.sandbox.python.to_string_lossy().to_string();
        let create = vec![
            python.clone(),
            "-I".into(),
            "-m".into(),
            "venv".into(),
            VENV_MOUNT.into(),
        ];
        let install = pyenv::pip_args(venv, report, &plan);
        for argv in [create, install] {
            self.run_build_step(&new, &build, &argv).await?;
        }
        let locked = pyenv::parse_report(
            &fs::read(build.join("report.json")).map_err(|e| format!("pip wrote no report: {e}"))?,
        )?;
        let record = EnvRecord {
            plan_hash: plan.hash.clone(),
            built_at: chrono::Utc::now().timestamp(),
            python,
            locked,
        };
        // Swap in the new environment, then write the record that makes it "ready".
        let current = root.join("venv");
        let old = root.join("venv.old");
        if old.exists() {
            fs::remove_dir_all(&old).map_err(|e| e.to_string())?;
        }
        if current.exists() {
            fs::rename(&current, &old).map_err(|e| e.to_string())?;
        }
        if let Err(e) = fs::rename(&new, &current) {
            if old.exists() {
                let _ = fs::rename(&old, &current);
            }
            return Err(e.to_string());
        }
        write_atomic(
            &root.join("env.json"),
            &serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?,
        )?;
        let _ = fs::remove_dir_all(&old);
        let _ = fs::remove_dir_all(&build);
        Ok(record)
    }

    /// One build command in bubblewrap: network on, the new venv and the build folder
    /// writable, nothing else from the host but read-only system files.
    async fn run_build_step(&self, venv: &Path, build: &Path, argv: &[String]) -> Result<(), String> {
        let args = build_args(venv, build, argv);
        let mut command = tokio::process::Command::new(&self.sandbox.bwrap);
        command
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|e| format!("could not start the build sandbox: {e}"))?;
        let output = tokio::time::timeout(BUILD_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| "the environment build took longer than 10 minutes".to_string())?
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            return Ok(());
        }
        let tail: String = String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        Err(format!("{} failed: {tail}", argv.get(3).map_or("build", |s| s.as_str())))
    }

    /// Enabled plugins that registered `hook` and for which the owner turned it on.
    /// Cheap: reads the settings file and the describe cache, starts no sandbox.
    pub async fn hooked(&self, hook: &str) -> Vec<Plugin> {
        let Ok(settings) = self.paths.load_hooks() else {
            return Vec::new();
        };
        if settings.plugins.values().all(|h| !h.iter().any(|n| n == hook)) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for plugin in self.enabled() {
            let on = settings
                .plugins
                .get(&plugin.id)
                .is_some_and(|h| h.iter().any(|n| n == hook));
            if on
                && let Ok(description) = self.describe(&plugin).await
                && description.hooks.iter().any(|n| n == hook)
            {
                out.push(plugin);
            }
        }
        out
    }

    /// Run one hook on every plugin that has it turned on. Each plugin gets
    /// [`HOOK_TIMEOUT`]; failures come back as [`HookEffect::Rejected`].
    pub async fn run_hooks(&self, payload: &HookPayload) -> Vec<(String, HookEffect)> {
        let hook = hook_name(payload);
        let mut out = Vec::new();
        for plugin in self.hooked(hook).await {
            let settings = plugin
                .approval
                .as_ref()
                .map(|a| Value::Object(a.settings.clone()))
                .unwrap_or_else(|| json!({}));
            let mut request = hooks::hook_request(
                &plugin.id,
                &format!("/opt/hermes/plugins/{}", plugin.id),
                &settings,
                payload,
            );
            request["op"] = json!("hook");
            let effect = match tokio::time::timeout(
                HOOK_TIMEOUT,
                self.sandbox.run(&self.paths, &plugin, request),
            )
            .await
            {
                Ok(Ok(reply)) => hooks::interpret(hook, &reply),
                Ok(Err(e)) => HookEffect::Rejected(e),
                Err(_) => HookEffect::Rejected(format!("{hook} took longer than 3 s")),
            };
            if let HookEffect::Rejected(reason) = &effect {
                tracing::warn!(plugin = %plugin.id, hook, %reason, "Hermes hook output not used");
            }
            out.push((plugin.id.clone(), effect));
        }
        out
    }
}

/// The hook name of a payload (its serde tag).
pub fn hook_name(payload: &HookPayload) -> &'static str {
    match payload {
        HookPayload::PreLlmCall { .. } => "pre_llm_call",
        HookPayload::PostLlmCall { .. } => "post_llm_call",
        HookPayload::PreToolCall { .. } => "pre_tool_call",
        HookPayload::PostToolCall { .. } => "post_tool_call",
    }
}

/// The `bwrap` arguments for one environment build step. Public for tests and docs.
pub fn build_args(venv: &Path, build: &Path, argv: &[String]) -> Vec<String> {
    let s = |p: &Path| p.to_string_lossy().to_string();
    let mut args: Vec<String> = [
        "--unshare-all",
        "--share-net",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--ro-bind",
        "/usr",
        "/usr",
    ]
    .map(String::from)
    .to_vec();
    for dir in ["/bin", "/lib", "/lib64", "/sbin"] {
        match fs::read_link(dir) {
            Ok(target) => args.extend(["--symlink".into(), s(&target), dir.into()]),
            Err(_) if Path::new(dir).is_dir() => {
                args.extend(["--ro-bind".into(), dir.into(), dir.into()])
            }
            Err(_) => {}
        }
    }
    for path in [
        "/etc/ld.so.cache",
        "/etc/ssl",
        "/etc/ca-certificates",
        "/etc/alternatives",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/gai.conf",
        "/run/systemd/resolve",
    ] {
        args.extend(["--ro-bind-try", path, path].map(String::from));
    }
    args.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"].map(String::from));
    args.extend(["--bind".into(), s(venv), VENV_MOUNT.into()]);
    args.extend(["--bind".into(), s(build), "/opt/hermes/build".into()]);
    for (key, value) in [
        ("HOME", "/tmp"),
        ("PATH", "/usr/bin:/bin"),
        ("LANG", "C.UTF-8"),
        ("PIP_NO_INPUT", "1"),
    ] {
        args.extend(["--setenv".into(), key.into(), value.into()]);
    }
    args.extend(["--chdir".into(), "/tmp".into(), "--".into()]);
    args.extend(argv.iter().cloned());
    args
}
