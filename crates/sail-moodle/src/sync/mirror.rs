//! Which course files to download. A file is fetched when SAIL hasn't seen its key, or
//! when Moodle reports a different size or modified time than the newest local version.
use crate::contracts::*;
use crate::store::FileVersion;
use std::collections::{HashMap, HashSet};

/// What the mirror should do for one course.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirrorPlan {
    /// New or changed files, smallest first so a download budget covers the most files.
    pub fetch: Vec<FileRef>,
    /// Files over [`MAX_FILE_BYTES`]: recorded, never downloaded.
    pub too_large: Vec<FileRef>,
    /// Files whose newest local version is current.
    pub unchanged: usize,
}

/// Plan downloads for `files` against the newest known version per key.
pub fn plan(files: &[FileRef], known: &HashMap<FileKey, FileVersion>) -> MirrorPlan {
    let mut out = MirrorPlan::default();
    for file in files {
        if file.size > MAX_FILE_BYTES {
            out.too_large.push(file.clone());
            continue;
        }
        match known.get(&file.key()) {
            Some(v) if v.size == file.size && v.modified == file.modified => out.unchanged += 1,
            _ => out.fetch.push(file.clone()),
        }
    }
    out.fetch
        .sort_by(|a, b| a.size.cmp(&b.size).then_with(|| a.key().cmp(&b.key())));
    out
}

/// Every file a snapshot references: module files, assignment attachments and
/// announcement attachments, one per key.
pub fn snapshot_files(snapshot: &CourseSnapshot) -> Vec<FileRef> {
    let mut files: Vec<FileRef> = snapshot
        .sections
        .iter()
        .flat_map(|s| s.modules.iter())
        .flat_map(|m| m.files.iter().cloned())
        .collect();
    files.extend(
        snapshot
            .assignments
            .iter()
            .flat_map(|a| a.attachments.iter().cloned()),
    );
    files.extend(
        snapshot
            .announcements
            .iter()
            .flat_map(|a| a.attachments.iter().cloned()),
    );
    let mut seen = HashSet::new();
    files.retain(|f| seen.insert(f.key()));
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::diff::tests::{file, t};

    fn version(f: &FileRef) -> FileVersion {
        FileVersion {
            key: f.key(),
            course_id: f.course_id,
            size: f.size,
            modified: f.modified,
            sha256: "0".repeat(64),
            downloaded_at: t(0),
        }
    }

    #[test]
    fn new_changed_unchanged_and_too_large() {
        let same = file(1, "a.pdf", 10);
        let changed = file(1, "b.pdf", 20);
        let new = file(1, "c.pdf", 5);
        let huge = file(1, "video.mp4", MAX_FILE_BYTES + 1);
        let mut known = HashMap::new();
        known.insert(same.key(), version(&same));
        let mut old_b = version(&changed);
        old_b.size = 19;
        known.insert(changed.key(), old_b);
        let p = plan(&[same, changed.clone(), new.clone(), huge.clone()], &known);
        assert_eq!(p.unchanged, 1);
        assert_eq!(p.fetch, vec![new, changed]);
        assert_eq!(p.too_large, vec![huge]);
    }
}
