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
use super::{Bundle, Installed, Lock, LockEntry, StagedReport};
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
        unimplemented!("Kilo, round 20")
    }

    pub fn save_lock(&self, lock: &Lock) -> Result<(), String> {
        let _ = lock;
        unimplemented!("Kilo, round 20")
    }

    pub fn stage(
        &self,
        stage_id: &str,
        listing: super::Listing,
        bundle: &Bundle,
        now: i64,
    ) -> Result<StagedReport, String> {
        let _ = (stage_id, listing, bundle, now);
        unimplemented!("Kilo, round 20")
    }

    pub fn staged(&self, stage_id: &str) -> Result<StagedReport, String> {
        let _ = stage_id;
        unimplemented!("Kilo, round 20")
    }

    pub fn install(
        &self,
        stage_id: &str,
        acknowledge: bool,
        now: i64,
    ) -> Result<LockEntry, String> {
        let _ = (stage_id, acknowledge, now);
        unimplemented!("Kilo, round 20")
    }

    pub fn discard(&self, stage_id: &str) -> Result<(), String> {
        let _ = stage_id;
        unimplemented!("Kilo, round 20")
    }

    pub fn sweep(&self, now: i64, max_age_secs: i64) -> Result<usize, String> {
        let _ = (now, max_age_secs);
        unimplemented!("Kilo, round 20")
    }

    pub fn installed(&self) -> Result<Vec<Installed>, String> {
        unimplemented!("Kilo, round 20")
    }

    pub fn uninstall(
        &self,
        key: &str,
        force: bool,
        hermes: Option<&crate::hermes::Paths>,
    ) -> Result<(), String> {
        let _ = (key, force, hermes);
        unimplemented!("Kilo, round 20")
    }
}

pub fn content_hash(bundle: &Bundle) -> String {
    let _ = bundle;
    unimplemented!("Kilo, round 20")
}

pub fn dir_content_hash(dir: &Path) -> Result<String, String> {
    let _ = dir;
    unimplemented!("Kilo, round 20")
}
