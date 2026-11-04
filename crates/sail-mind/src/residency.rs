//! How much of a file is in memory (the page cache), measured with `mincore`.
//!
//! llama-server reports nothing while it loads a model; it maps the GGUF and reads it. The
//! share of the file in the page cache rises with that reading, so it measures load
//! progress. The pages may later be evicted under memory pressure, so callers keep the
//! highest value they have seen.
use std::path::Path;

/// Bytes of `path` currently in the page cache. Reading the residency does not load any
/// page: the file is mapped but never touched.
pub fn resident_bytes(path: &Path) -> std::io::Result<u64> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size == 0 {
        return Ok(0);
    }
    let length = usize::try_from(size).map_err(std::io::Error::other)?;
    // SAFETY: a read-only shared mapping of a file we opened; it is only passed to
    // `mincore` and unmapped before returning, and no page of it is ever read.
    unsafe {
        let address = libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        );
        if address == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let page = libc::sysconf(libc::_SC_PAGESIZE).max(1) as usize;
        let mut pages = vec![0u8; length.div_ceil(page)];
        let status = libc::mincore(address, length, pages.as_mut_ptr());
        let error = std::io::Error::last_os_error();
        libc::munmap(address, length);
        if status != 0 {
            return Err(error);
        }
        let resident = pages.iter().filter(|p| *p & 1 == 1).count() as u64;
        Ok((resident * page as u64).min(size))
    }
}

/// Load progress of one model's weight files, never going backwards.
pub struct LoadProgress {
    files: Vec<std::path::PathBuf>,
    total: u64,
    best: u64,
}
impl LoadProgress {
    pub fn new(files: Vec<std::path::PathBuf>) -> Self {
        let total = files
            .iter()
            .filter_map(|f| std::fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        Self {
            files,
            total,
            best: 0,
        }
    }
    /// Measure again; unreadable files count as not loaded.
    pub fn sample(&mut self) -> sail_protocol::ModelLoad {
        let now: u64 = self
            .files
            .iter()
            .filter_map(|f| resident_bytes(f).ok())
            .sum();
        self.best = self.best.max(now.min(self.total));
        sail_protocol::ModelLoad {
            loaded_bytes: self.best,
            total_bytes: self.total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reading_a_file_raises_its_measured_residency_and_progress_never_falls() {
        // Beside the test binary: a disk-backed directory (tmpfs pages cannot be evicted).
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let path = dir.join(format!("sail-residency-{}", uuid::Uuid::new_v4()));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![7u8; 4 * 1024 * 1024]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        // Freshly written pages are cached; reading keeps them so.
        let _ = std::fs::read(&path).unwrap();
        let mut progress = LoadProgress::new(vec![path.clone()]);
        let first = progress.sample();
        assert_eq!(first.total_bytes, 4 * 1024 * 1024);
        assert_eq!(first.loaded_bytes, first.total_bytes);
        // Evicting the pages does not move progress backwards.
        let handle = std::fs::File::open(&path).unwrap();
        // SAFETY: advisory call on an open descriptor.
        unsafe {
            use std::os::fd::AsRawFd;
            libc::posix_fadvise(handle.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
        assert!(resident_bytes(&path).unwrap() < first.total_bytes);
        assert_eq!(progress.sample(), first);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_file_has_nothing_loaded() {
        let mut progress = LoadProgress::new(vec!["/nonexistent/model.gguf".into()]);
        let load = progress.sample();
        assert_eq!((load.loaded_bytes, load.total_bytes), (0, 0));
    }
}
