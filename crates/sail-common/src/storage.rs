//! Where SAIL keeps persistent data.
//!
//! Every durable store (Continuity, Arena, Forge state, the review ledger, Guardian's
//! learned rules, logs) lives under one *data root*:
//!
//! 1. `SAIL_DATA`, when set, used as given;
//! 2. otherwise `/srv/sail-data`, but only when it is a mounted filesystem and writable. If
//!    the data disk is not mounted, the empty mount point sits on the root SSD, and writing
//!    there is how the host filled its root disk before (vision §13.1);
//! 3. otherwise `$XDG_DATA_HOME/sail` (or `~/.local/share/sail`), with an error logged
//!    once, so a developer machine still works.
//!
//! Hot paths are deliberately *not* here: Forge worktrees, Cargo `target/` and active
//! GGUF models stay on the runtime SSD (`/srv/sail`) for speed.
//!
//! Ownership: `scripts/setup-sail-data.sh` makes the root owned by the SAIL service user
//! and a `sail-data` group (mode `2770`, setgid so new directories keep the group). The
//! sandbox runner user must not be in that group.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Default mount point of the data disk.
pub const DEFAULT_DATA_MOUNT: &str = "/srv/sail-data";

/// The resolved data root and why it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRoot {
    pub path: PathBuf,
    pub source: DataRootSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataRootSource {
    /// `SAIL_DATA` was set.
    Env,
    /// `/srv/sail-data` is mounted and writable.
    DataDisk,
    /// The data disk was not usable; this is the user's data directory. The reason says why.
    Fallback(String),
}

/// Resolve the data root from the environment. Call [`log_data_root`] once at startup so
/// the choice (and a fallback) is visible in the logs.
pub fn resolve() -> DataRoot {
    let allow_unmounted = std::env::var("SAIL_DATA_ALLOW_UNMOUNTED")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    resolve_data_root_with(
        std::env::var_os("SAIL_DATA").map(PathBuf::from),
        Path::new(DEFAULT_DATA_MOUNT),
        user_data_dir(),
        allow_unmounted,
    )
}

/// The data root path. Resolves without side effects.
pub fn data_root() -> PathBuf {
    resolve().path
}

/// `<data root>/<component>`, without creating it. Library defaults use this so that
/// constructing a store never writes to disk.
pub fn data_path(component: &str) -> PathBuf {
    data_root().join(component)
}

/// `<data root>/<component>`, created if missing (mode 0770 before umask; under the setgid
/// root it inherits the `sail-data` group).
pub fn data_dir(component: &str) -> PathBuf {
    let dir = data_path(component);
    let _ = create_private_dir(&dir);
    dir
}

/// Log where persistent data goes. An unusable data disk is an error, not a silent switch.
pub fn log_data_root() -> DataRoot {
    let root = resolve();
    match &root.source {
        DataRootSource::Fallback(reason) => tracing::error!(
            path = %root.path.display(),
            %reason,
            "SAIL data disk unavailable; persistent data goes to the user's data directory"
        ),
        source => tracing::info!(path = %root.path.display(), ?source, "SAIL data root"),
    }
    root
}

/// Decide the data root. Pure apart from filesystem probes, for tests.
pub fn resolve_data_root(env: Option<PathBuf>, mount: &Path, fallback: PathBuf) -> DataRoot {
    resolve_data_root_with(env, mount, fallback, false)
}

/// As [`resolve_data_root`]; `allow_unmounted` (`SAIL_DATA_ALLOW_UNMOUNTED=1`) accepts a
/// plain directory at the mount point, for machines without a separate data disk.
pub fn resolve_data_root_with(
    env: Option<PathBuf>,
    mount: &Path,
    fallback: PathBuf,
    allow_unmounted: bool,
) -> DataRoot {
    if let Some(path) = env.filter(|p| !p.as_os_str().is_empty()) {
        return DataRoot {
            path,
            source: DataRootSource::Env,
        };
    }
    let reason = if !mount.is_dir() {
        format!("{} does not exist", mount.display())
    } else if !allow_unmounted && !is_mount_point(mount) {
        format!(
            "{} is not a mounted filesystem (is the data disk mounted?)",
            mount.display()
        )
    } else if !writable_dir(mount) {
        format!(
            "{} is not writable by this user (run scripts/setup-sail-data.sh)",
            mount.display()
        )
    } else {
        return DataRoot {
            path: mount.to_path_buf(),
            source: DataRootSource::DataDisk,
        };
    };
    DataRoot {
        path: fallback,
        source: DataRootSource::Fallback(reason),
    }
}

fn user_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("sail")
}

/// True when `path` is the root of a mounted filesystem: its device differs from its
/// parent's, or it is `/`.
pub fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let parent = path.parent().unwrap_or(Path::new("/"));
    match fs::metadata(parent) {
        Ok(p) => p.dev() != meta.dev() || p.ino() == meta.ino(),
        Err(_) => false,
    }
}

fn writable_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".sail-write-probe-{}", std::process::id()));
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o770)
        .create(dir)
}

/// Move a file or directory from an old default location into the data root, once.
///
/// Does nothing when `new` already exists or `old` does not. Otherwise copies `old` to a
/// temporary name beside `new`, renames it into place (so a crash never leaves a partial
/// `new`), then renames `old` to `<old>.migrated-to-sail-data` as a backup. The old data is
/// never deleted. Symlinks are refused rather than followed.
///
/// Returns `Ok(true)` when something was migrated.
pub fn migrate_legacy(old: &Path, new: &Path) -> io::Result<bool> {
    if fs::symlink_metadata(old).is_err() || old == new {
        return Ok(false);
    }
    // An existing destination directory: merge the old directory's entries into it,
    // each one moved only if the destination does not have it yet.
    if new.is_dir() && fs::symlink_metadata(old)?.is_dir() {
        let mut moved = false;
        for entry in fs::read_dir(old)? {
            let entry = entry?;
            moved |= migrate_legacy(&entry.path(), &new.join(entry.file_name()))?;
        }
        if moved {
            let backup = PathBuf::from(format!("{}.migrated-to-sail-data", old.display()));
            let _ = fs::rename(old, &backup);
        }
        return Ok(moved);
    }
    if new.exists() {
        return Ok(false);
    }
    if fs::symlink_metadata(old)?.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "refusing to migrate symlink {}",
            old.display()
        )));
    }
    if let Some(parent) = new.parent() {
        create_private_dir(parent)?;
    }
    let tmp = new.with_file_name(format!(
        ".{}.migrating-{}",
        new.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_file(&tmp);
    copy_tree(old, &tmp)?;
    fs::rename(&tmp, new)?;
    let backup = PathBuf::from(format!("{}.migrated-to-sail-data", old.display()));
    if fs::rename(old, &backup).is_err() {
        tracing::warn!(
            old = %old.display(),
            new = %new.display(),
            "migrated SAIL data but could not rename the old copy; it is now unused"
        );
    }
    tracing::info!(old = %old.display(), new = %new.display(), "migrated SAIL data");
    Ok(true)
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "refusing to copy symlink {}",
            src.display()
        )));
    }
    if meta.is_dir() {
        create_private_dir(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst)?;
        fs::File::open(dst)?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sail-storage-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn env_wins() {
        let r = resolve_data_root(Some("/x/y".into()), Path::new("/nonexistent"), "/fb".into());
        assert_eq!(r.path, PathBuf::from("/x/y"));
        assert_eq!(r.source, DataRootSource::Env);
    }

    #[test]
    fn empty_env_is_ignored() {
        let r = resolve_data_root(
            Some("".into()),
            Path::new("/nonexistent-sail"),
            "/fb".into(),
        );
        assert_eq!(r.path, PathBuf::from("/fb"));
    }

    #[test]
    fn missing_or_unmounted_data_disk_falls_back_with_reason() {
        let r = resolve_data_root(None, Path::new("/nonexistent-sail-data"), "/fb".into());
        assert!(
            matches!(&r.source, DataRootSource::Fallback(why) if why.contains("does not exist"))
        );

        // A plain directory on the same filesystem as its parent is not a mount.
        let dir = tmp("unmounted");
        let r = resolve_data_root(None, &dir, "/fb".into());
        assert_eq!(r.path, PathBuf::from("/fb"));
        assert!(
            matches!(&r.source, DataRootSource::Fallback(why) if why.contains("not a mounted filesystem")),
            "{r:?}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn root_and_proc_are_mount_points() {
        assert!(is_mount_point(Path::new("/")));
        assert!(is_mount_point(Path::new("/proc")));
        let dir = tmp("plain");
        assert!(!is_mount_point(&dir));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migrates_files_and_directories_once_and_keeps_a_backup() {
        let base = tmp("migrate");
        let old_file = base.join("old/ledger.json");
        fs::create_dir_all(old_file.parent().unwrap()).unwrap();
        fs::write(&old_file, "{\"a\":1}").unwrap();
        let new_file = base.join("data/forge/review_ledger.json");
        assert!(migrate_legacy(&old_file, &new_file).unwrap());
        assert_eq!(fs::read_to_string(&new_file).unwrap(), "{\"a\":1}");
        assert!(!old_file.exists());
        assert!(base.join("old/ledger.json.migrated-to-sail-data").exists());
        // Second call: nothing to do.
        assert!(!migrate_legacy(&old_file, &new_file).unwrap());

        let old_dir = base.join("state/forge");
        fs::create_dir_all(old_dir.join("digests")).unwrap();
        fs::write(old_dir.join("digests/2026-11-01.md"), "d").unwrap();
        fs::write(old_dir.join("nightly_state.json"), "{}").unwrap();
        let new_dir = base.join("data/forge-state");
        assert!(migrate_legacy(&old_dir, &new_dir).unwrap());
        assert_eq!(
            fs::read_to_string(new_dir.join("digests/2026-11-01.md")).unwrap(),
            "d"
        );
        assert!(new_dir.join("nightly_state.json").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn old_directory_merges_into_an_existing_one() {
        let base = tmp("merge");
        fs::create_dir_all(base.join("old/digests")).unwrap();
        fs::write(base.join("old/state.json"), "old").unwrap();
        fs::write(base.join("old/digests/a.md"), "a").unwrap();
        fs::create_dir_all(base.join("new")).unwrap();
        fs::write(base.join("new/state.json"), "new").unwrap();
        assert!(migrate_legacy(&base.join("old"), &base.join("new")).unwrap());
        assert_eq!(
            fs::read_to_string(base.join("new/state.json")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(base.join("new/digests/a.md")).unwrap(),
            "a"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn allow_unmounted_accepts_a_plain_directory() {
        let dir = tmp("allow");
        let r = resolve_data_root_with(None, &dir, "/fb".into(), true);
        assert_eq!(r.path, dir);
        assert_eq!(r.source, DataRootSource::DataDisk);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_destination_is_never_overwritten() {
        let base = tmp("keep");
        fs::write(base.join("old"), "old").unwrap();
        fs::write(base.join("new"), "new").unwrap();
        assert!(!migrate_legacy(&base.join("old"), &base.join("new")).unwrap());
        assert_eq!(fs::read_to_string(base.join("new")).unwrap(), "new");
        assert!(base.join("old").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn symlinks_are_refused() {
        let base = tmp("link");
        fs::write(base.join("target"), "x").unwrap();
        std::os::unix::fs::symlink(base.join("target"), base.join("old")).unwrap();
        assert!(migrate_legacy(&base.join("old"), &base.join("new")).is_err());
        assert!(!base.join("new").exists());
        fs::remove_dir_all(&base).unwrap();
    }
}
