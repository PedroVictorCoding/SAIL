//! Quarantine, install, lock and uninstall (round 20, **Kilo's slice**). File-system only:
//! no network, no clock (callers pass `now`). **This module doc is the spec.**
//!
//! # Layout ([`Store`])
//!
//! ```text
//! <market>/lock.json             Lock (version 1), written atomically (tmp + fsync + rename)
//! <market>/quarantine/<stage>/   one staged item: files/ + staged.json (StagedReport)
//! <skills>/<name>/               installed skills  (= $SAIL_DATA/skills)
//! <plugins>/<id>/                installed plugins (= $SAIL_DATA/plugins/hermes)
//! ```
//!
//! # Staging ([`Store::stage`])
//!
//! Writes `bundle` to `<market>/quarantine/<stage_id>/files/` (create dirs; files are
//! written `0o644`, dirs `0o755`; refuse any path failing
//! [`super::safe_relative_path`]; refuse a bundle over [`super::MAX_BUNDLE_FILES`] or
//! [`super::MAX_BUNDLE_BYTES`]), then fills and returns the [`StagedReport`] and saves
//! it as `staged.json`:
//!
//! * `stage_id`: the caller's (32 lowercase hex; refuse anything else).
//! * `install_name`:
//!   * skill: the `name` field of the bundle's root `SKILL.md` frontmatter (parse with
//!     [`crate::skills::split_frontmatter`] + `serde_norway`). It must pass
//!     [`crate::skills::valid_name`]; otherwise `install_name` is the raw name (or empty)
//!     and `decision` becomes `Block` with reason "SKILL.md name \"…\" is not a valid
//!     Agent Skills name". A bundle without `SKILL.md` → `Block`, "no SKILL.md".
//!   * plugin: `listing.id`. It must pass [`crate::hermes::valid_id`], else `Block`.
//! * `content_hash`: skills → [`content_hash`]; plugins → [`crate::hermes::digest`] of the
//!   `files/` folder (so it equals the digest the owner approves later).
//! * `files`: every path with its size, sorted by path.
//! * `report`, `decision`, `reason`: from [`super::guard::scan`] and
//!   [`super::guard::decide`] with `listing.trust` — unless a name rule above already
//!   blocked it. **Conflict rule:** if `<skills>/<install_name>` (or `<plugins>/<id>`)
//!   exists and the lock has no entry for it, the folder belongs to the owner: `Block`,
//!   reason "a skill named … already exists and was not installed from the market".
//!   If it exists and the lock has it, the stage is an **update** (allowed).
//! * `preview`: skills → `SKILL.md`; plugins → `README.md` (any case) or else
//!   `plugin.yaml`; as UTF-8 (lossy), cut at 64 KiB on a char boundary.
//! * `revision` from the bundle, `staged_at = now`.
//!
//! # Install ([`Store::install`])
//!
//! 1. Load `staged.json`; missing → `Err("no staged item …")`.
//! 2. `decision == Block` → `Err(reason)`. `NeedsAck` and `!acknowledge` → `Err` "…
//!    needs your confirmation".
//! 3. Re-hash `files/` the same way as staging; a different hash → `Err` "the staged files
//!    changed; stage it again" (and leave everything in place).
//! 4. Move into place **atomically**: rename `files/` to `<dest>.market-new` (same
//!    filesystem), then if `<dest>` exists rename it to `<dest>.market-old`, rename
//!    `.market-new` → `<dest>`, then delete `.market-old`. On any failure restore the
//!    old folder. (Stale `.market-new`/`.market-old` from a crash are removed before
//!    starting.) The quarantine folder is removed after success.
//! 5. Write the [`LockEntry`] (`installed_at` keeps the old value on update,
//!    `updated_at = now`, `files` = staged paths) under key `skill:<name>` /
//!    `plugin:<id>` and save the lock. Return the entry.
//!
//! For a plugin, an update leaves the previous approval in
//! `hermes-approvals.json` untouched; [`crate::hermes`] then reports the plugin as
//! `Changed` until the owner approves the new files. That is intended.
//!
//! # Discard and cleanup
//!
//! [`Store::discard`] removes one quarantine folder (Ok if absent).
//! [`Store::sweep`] removes quarantine folders whose `staged_at` is older than
//! `now - max_age` (and folders without a readable `staged.json`).
//!
//! # Installed list ([`Store::installed`])
//!
//! Every lock entry with `state`: `"missing"` if `install_path` does not exist,
//! `"modified"` if its hash (same method as at install) differs from `content_hash`,
//! else `"ok"`. `removed` is left `None` (the service fills it from the catalog). Sorted
//! by key.
//!
//! # Uninstall ([`Store::uninstall`])
//!
//! Only lock keys can be uninstalled. A `"modified"` item needs `force`, else `Err`
//! "… was edited after install; confirm to delete your changes". Removes the folder
//! (`remove_dir_all`, after checking it is a real directory, not a symlink, and that it
//! is directly inside `<skills>` or `<plugins>`), then the lock entry. `"missing"` just
//! drops the entry. For plugins it also calls [`crate::hermes::Paths::revoke`] when given
//! `hermes` paths (ignore "no approval" results).
//!
//! # Hash ([`content_hash`])
//!
//! SHA-256 (via `ring`) over, for each file in path order: the path bytes, a NUL, the
//! length as 8 bytes big-endian, the content. Lowercase hex. For an installed skill
//! folder, [`dir_content_hash`] walks the folder (no symlinks: a symlink → `Err`),
//! builds the same map, and hashes it identically.
use super::{
    Bundle, Decision, Installed, Kind, Lock, LockEntry, MAX_BUNDLE_BYTES, MAX_BUNDLE_FILES,
    StagedFile, StagedReport, guard, safe_relative_path,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Store {
    /// `$SAIL_DATA/market`
    pub market: PathBuf,
    /// `$SAIL_DATA/skills`
    pub skills: PathBuf,
    /// `$SAIL_DATA/plugins/hermes`
    pub plugins: PathBuf,
}

fn valid_stage_id(stage_id: &str) -> bool {
    stage_id.len() == 32
        && stage_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

#[cfg(unix)]
fn set_tree_modes(dir: &Path) -> Result<(), String> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        set_mode(&current, 0o755);
        for entry in fs::read_dir(&current).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                set_mode(&entry.path(), 0o644);
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_tree_modes(_dir: &Path) -> Result<(), String> {
    Ok(())
}

fn write_staged_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        set_mode(parent, 0o755);
    }
    let mut file = fs::File::create(path).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    set_mode(path, 0o644);
    Ok(())
}

#[derive(Deserialize)]
struct FrontmatterName {
    name: Option<String>,
}

fn frontmatter_name(text: &str) -> Option<String> {
    let (front, _) = crate::skills::split_frontmatter(text)?;
    let front: FrontmatterName = serde_norway::from_str(front).ok()?;
    front.name
}

fn preview_bytes(kind: Kind, files: &BTreeMap<String, Vec<u8>>) -> String {
    let bytes = match kind {
        Kind::Skill => files.get("SKILL.md").map(Vec::as_slice),
        Kind::Plugin => files
            .iter()
            .find(|(path, _)| !path.contains('/') && path.eq_ignore_ascii_case("readme.md"))
            .map(|(_, bytes)| bytes.as_slice())
            .or_else(|| files.get("plugin.yaml").map(Vec::as_slice)),
    };
    let text = String::from_utf8_lossy(bytes.unwrap_or_default());
    if text.len() <= 64 * 1024 {
        return text.into_owned();
    }
    let mut end = 64 * 1024;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn hash_files(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    for (path, content) in files {
        context.update(path.as_bytes());
        context.update(&[0]);
        context.update(&(content.len() as u64).to_be_bytes());
        context.update(content);
    }
    context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_tree(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current)
            .map_err(|e| format!("cannot read {}: {e}", current.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err(format!(
                    "{} is a symbolic link; SAIL does not follow those",
                    entry.path().display()
                ));
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(dir)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = fs::read(entry.path()).map_err(|e| e.to_string())?;
                files.insert(relative, bytes);
            } else {
                return Err(format!("{} is not a regular file", entry.path().display()));
            }
        }
    }
    Ok(files)
}

fn item_hash(kind: Kind, dir: &Path) -> Result<String, String> {
    match kind {
        Kind::Skill => dir_content_hash(dir),
        Kind::Plugin => crate::hermes::digest(dir),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn restore(dest: &Path, old: Option<&Path>) -> Result<(), String> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|e| e.to_string())?;
    }
    if let Some(old) = old {
        fs::rename(old, dest).map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Store {
    pub fn lock_path(&self) -> PathBuf {
        self.market.join("lock.json")
    }

    pub fn quarantine(&self) -> PathBuf {
        self.market.join("quarantine")
    }

    /// The lock, or an empty one when missing. A corrupt lock is an `Err` (never
    /// silently replaced).
    pub fn load_lock(&self) -> Result<Lock, String> {
        let path = self.lock_path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} is corrupt: {e}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
        }
    }

    pub fn save_lock(&self, lock: &Lock) -> Result<(), String> {
        if !self.market.exists() {
            fs::create_dir_all(&self.market).map_err(|e| e.to_string())?;
            set_mode(&self.market, 0o755);
        }
        let path = self.lock_path();
        let tmp = with_suffix(&path, ".tmp");
        let bytes = serde_json::to_vec_pretty(lock).map_err(|e| e.to_string())?;
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn stage(
        &self,
        stage_id: &str,
        listing: super::Listing,
        bundle: &Bundle,
        now: i64,
    ) -> Result<StagedReport, String> {
        if !valid_stage_id(stage_id) {
            return Err(format!(
                "stage id {stage_id:?} is not 32 lowercase hex characters"
            ));
        }
        if bundle.files.len() > MAX_BUNDLE_FILES || bundle.total_bytes() > MAX_BUNDLE_BYTES {
            return Err(format!(
                "{} files, {} bytes: over the {MAX_BUNDLE_FILES}-file / 32 MiB limit",
                bundle.files.len(),
                bundle.total_bytes()
            ));
        }
        if let Some(path) = bundle.files.keys().find(|path| !safe_relative_path(path)) {
            return Err(format!("unsafe file path {path:?}"));
        }
        let lock = self.load_lock()?;
        let kind = listing.kind;

        let mut name_problem = None;
        let install_name = match kind {
            Kind::Skill => match bundle.files.get("SKILL.md") {
                None => {
                    name_problem = Some("no SKILL.md".to_string());
                    String::new()
                }
                Some(bytes) => {
                    let text = String::from_utf8_lossy(bytes);
                    let name = frontmatter_name(&text).unwrap_or_default();
                    if crate::skills::valid_name(&name) {
                        name
                    } else {
                        name_problem = Some(format!(
                            "SKILL.md name {name:?} is not a valid Agent Skills name"
                        ));
                        name
                    }
                }
            },
            Kind::Plugin => {
                let id = listing.id.clone();
                if crate::hermes::valid_id(&id) {
                    id
                } else {
                    name_problem = Some(format!("plugin id {id:?} is not a valid plugin id"));
                    id
                }
            }
        };

        let dest = match kind {
            Kind::Skill => self.skills.join(&install_name),
            Kind::Plugin => self.plugins.join(&install_name),
        };
        let key = match kind {
            Kind::Skill => format!("skill:{install_name}"),
            Kind::Plugin => format!("plugin:{install_name}"),
        };
        if name_problem.is_none()
            && !install_name.is_empty()
            && dest.exists()
            && !lock.installed.contains_key(&key)
        {
            let what = match kind {
                Kind::Skill => "skill",
                Kind::Plugin => "plugin",
            };
            name_problem = Some(format!(
                "a {what} named {install_name} already exists and was not installed from the market"
            ));
        }

        let report = guard::scan(bundle, kind);
        let (mut decision, mut reason) = guard::decide(listing.trust, &report);
        if let Some(problem) = name_problem {
            decision = Decision::Block;
            reason = problem;
        }

        let stage_dir = self.quarantine().join(stage_id);
        if stage_dir.exists() {
            fs::remove_dir_all(&stage_dir).map_err(|e| e.to_string())?;
        }
        let files_dir = stage_dir.join("files");
        fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
        for (path, bytes) in &bundle.files {
            write_staged_file(&files_dir.join(path), bytes)?;
        }
        set_tree_modes(&stage_dir)?;
        set_mode(&self.quarantine(), 0o755);

        let content_hash = match kind {
            Kind::Skill => content_hash(bundle),
            Kind::Plugin => crate::hermes::digest(&files_dir)?,
        };
        let staged = StagedReport {
            stage_id: stage_id.to_string(),
            files: bundle
                .files
                .iter()
                .map(|(path, bytes)| StagedFile {
                    path: path.clone(),
                    bytes: bytes.len() as u64,
                })
                .collect(),
            install_name,
            content_hash,
            revision: bundle.revision.clone(),
            report,
            decision,
            reason,
            preview: preview_bytes(kind, &bundle.files),
            staged_at: now,
            listing,
        };
        fs::write(
            stage_dir.join("staged.json"),
            serde_json::to_vec_pretty(&staged).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(staged)
    }

    pub fn staged(&self, stage_id: &str) -> Result<StagedReport, String> {
        if !valid_stage_id(stage_id) {
            return Err(format!(
                "stage id {stage_id:?} is not 32 lowercase hex characters"
            ));
        }
        let path = self.quarantine().join(stage_id).join("staged.json");
        let bytes = fs::read(&path).map_err(|_| format!("no staged item {stage_id}"))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| format!("staged item {stage_id} is corrupt: {e}"))
    }

    pub fn install(
        &self,
        stage_id: &str,
        acknowledge: bool,
        now: i64,
    ) -> Result<LockEntry, String> {
        let staged = self.staged(stage_id)?;
        match staged.decision {
            Decision::Block => return Err(staged.reason.clone()),
            Decision::NeedsAck if !acknowledge => {
                return Err(format!("{} needs your confirmation", staged.listing.name));
            }
            _ => {}
        }
        let kind = staged.listing.kind;
        let name_ok = match kind {
            Kind::Skill => crate::skills::valid_name(&staged.install_name),
            Kind::Plugin => crate::hermes::valid_id(&staged.install_name),
        };
        if !name_ok {
            return Err(format!(
                "{} {:?} is not a valid folder name",
                match kind {
                    Kind::Skill => "skill",
                    Kind::Plugin => "plugin",
                },
                staged.install_name
            ));
        }
        let files_dir = self.quarantine().join(stage_id).join("files");
        let current_hash = match kind {
            Kind::Skill => {
                let files = read_tree(&files_dir)?;
                hash_files(&files)
            }
            Kind::Plugin => crate::hermes::digest(&files_dir)?,
        };
        if current_hash != staged.content_hash {
            return Err("the staged files changed; stage it again".into());
        }

        let dest = match kind {
            Kind::Skill => self.skills.join(&staged.install_name),
            Kind::Plugin => self.plugins.join(&staged.install_name),
        };
        let key = match kind {
            Kind::Skill => format!("skill:{}", staged.install_name),
            Kind::Plugin => format!("plugin:{}", staged.install_name),
        };
        let mut lock = self.load_lock()?;
        if dest.exists() && !lock.installed.contains_key(&key) {
            let what = match kind {
                Kind::Skill => "skill",
                Kind::Plugin => "plugin",
            };
            return Err(format!(
                "a {what} named {} already exists and was not installed from the market",
                staged.install_name
            ));
        }
        let parent = match kind {
            Kind::Skill => &self.skills,
            Kind::Plugin => &self.plugins,
        };
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            set_mode(parent, 0o755);
        }

        let new = with_suffix(&dest, ".market-new");
        let old = with_suffix(&dest, ".market-old");
        if new.exists() {
            fs::remove_dir_all(&new).map_err(|e| e.to_string())?;
        }
        if old.exists() {
            fs::remove_dir_all(&old).map_err(|e| e.to_string())?;
        }
        fs::rename(&files_dir, &new).map_err(|e| e.to_string())?;
        let had_old = dest.exists();
        if had_old && let Err(error) = fs::rename(&dest, &old) {
            let _ = fs::remove_dir_all(&new);
            return Err(error.to_string());
        }
        if let Err(error) = fs::rename(&new, &dest) {
            if had_old {
                let _ = fs::rename(&old, &dest);
            }
            let _ = fs::remove_dir_all(&new);
            return Err(error.to_string());
        }

        let installed_at = lock
            .installed
            .get(&key)
            .map(|entry| entry.installed_at)
            .unwrap_or(now);
        let entry = LockEntry {
            kind,
            name: staged.install_name.clone(),
            id: staged.listing.id.clone(),
            source: staged.listing.source.clone(),
            trust: staged.listing.trust,
            route: staged.listing.route.clone(),
            revision: staged.revision.clone(),
            content_hash: staged.content_hash.clone(),
            verdict: staged.report.verdict,
            scanner: staged.report.scanner.clone(),
            findings: staged.report.findings.len(),
            acknowledged_caution: staged.decision == Decision::NeedsAck && acknowledge,
            install_path: dest.clone(),
            files: staged.files.iter().map(|file| file.path.clone()).collect(),
            installed_at,
            updated_at: now,
        };
        lock.installed.insert(key, entry.clone());
        if let Err(error) = self.save_lock(&lock) {
            restore(&dest, had_old.then_some(old.as_path()))?;
            return Err(error);
        }
        if had_old {
            let _ = fs::remove_dir_all(&old);
        }
        let _ = fs::remove_dir_all(self.quarantine().join(stage_id));
        Ok(entry)
    }

    pub fn discard(&self, stage_id: &str) -> Result<(), String> {
        if !valid_stage_id(stage_id) {
            return Err(format!(
                "stage id {stage_id:?} is not 32 lowercase hex characters"
            ));
        }
        match fs::remove_dir_all(self.quarantine().join(stage_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn sweep(&self, now: i64, max_age_secs: i64) -> Result<usize, String> {
        let quarantine = self.quarantine();
        let entries = match fs::read_dir(&quarantine) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.to_string()),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let dir = entry.path();
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let stale = match fs::read(dir.join("staged.json")) {
                Ok(bytes) => serde_json::from_slice::<StagedReport>(&bytes)
                    .map(|staged| staged.staged_at < now - max_age_secs)
                    .unwrap_or(true),
                Err(_) => true,
            };
            if stale {
                fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn installed(&self) -> Result<Vec<Installed>, String> {
        let lock = self.load_lock()?;
        let mut installed = Vec::with_capacity(lock.installed.len());
        for (key, entry) in lock.installed {
            let state = if !entry.install_path.exists() {
                "missing"
            } else if item_hash(entry.kind, &entry.install_path)
                .is_ok_and(|hash| hash == entry.content_hash)
            {
                "ok"
            } else {
                "modified"
            };
            installed.push(Installed {
                key,
                entry,
                state: state.to_string(),
                removed: None,
            });
        }
        Ok(installed)
    }

    pub fn uninstall(
        &self,
        key: &str,
        force: bool,
        hermes: Option<&crate::hermes::Paths>,
    ) -> Result<(), String> {
        let mut lock = self.load_lock()?;
        let Some(entry) = lock.installed.get(key).cloned() else {
            return Err(format!("{key} was not installed from the market"));
        };
        if !entry.install_path.exists() {
            lock.installed.remove(key);
            return self.save_lock(&lock);
        }
        let modified = item_hash(entry.kind, &entry.install_path)
            .map(|hash| hash != entry.content_hash)
            .unwrap_or(true);
        if modified && !force {
            return Err(format!(
                "{} was edited after install; confirm to delete your changes",
                entry.name
            ));
        }
        let root = match entry.kind {
            Kind::Skill => &self.skills,
            Kind::Plugin => &self.plugins,
        };
        let metadata = fs::symlink_metadata(&entry.install_path).map_err(|e| e.to_string())?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || entry.install_path.parent() != Some(root.as_path())
        {
            return Err(format!(
                "{} is not directly inside {}; refusing to delete it",
                entry.install_path.display(),
                root.display()
            ));
        }
        fs::remove_dir_all(&entry.install_path).map_err(|e| e.to_string())?;
        if entry.kind == Kind::Plugin
            && let Some(paths) = hermes
        {
            let _ = paths.revoke(&entry.id);
        }
        lock.installed.remove(key);
        self.save_lock(&lock)
    }
}

pub fn content_hash(bundle: &Bundle) -> String {
    hash_files(&bundle.files)
}

pub fn dir_content_hash(dir: &Path) -> Result<String, String> {
    read_tree(dir).map(|files| hash_files(&files))
}
