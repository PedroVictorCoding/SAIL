//! Linux descriptor-based workspace access. Never authorize a pathname and reopen it later.
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::{ffi::OsStrExt, fs::OpenOptionsExt},
};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone)]
pub struct ConfinedRoot {
    path: PathBuf,
    directory: Arc<File>,
}

#[derive(Debug)]
pub struct ConfinedFile {
    path: PathBuf,
    descriptor: File,
}

impl ConfinedRoot {
    pub fn new(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        Ok(Self {
            path,
            directory: Arc::new(directory),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn relative<'a>(&self, requested: &'a Path) -> io::Result<&'a Path> {
        let relative = if requested.is_absolute() {
            requested
                .strip_prefix(&self.path)
                .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "outside workspace"))?
        } else {
            requested
        };
        if relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "traversal denied",
            ));
        }
        // These private stores are never workspace data, even if accidentally nested in it.
        if relative.components().any(|part| matches!(part, Component::Normal(name) if
            [".git", ".ssh", ".aws", ".local", ".env", "vault.key", "vault.enc"].iter().any(|blocked| name == *blocked))) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "private workspace resource"));
        }
        let relative = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            relative
        };
        Ok(relative)
    }
    /// Replace one regular workspace file relative to a pinned parent. No
    /// caller pathname is reopened after confinement, and links are not followed.
    pub fn atomic_write(&self, requested: &Path, content: &[u8]) -> io::Result<usize> {
        use std::io::Write;
        if content.len() > 65536 {
            return Err(io::Error::other("write exceeds 64 KiB"));
        }
        let relative = self.relative(requested)?;
        let basename = relative
            .file_name()
            .ok_or_else(|| io::Error::other("file name required"))?;
        let parent = self.open(
            relative
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        if !parent.metadata()?.is_dir() {
            return Err(io::Error::other("parent is not a directory"));
        }
        match self.open(relative) {
            Ok(file) if file.metadata()?.is_file() => {}
            Ok(_) => return Err(io::Error::other("only regular files may be replaced")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let temporary = CString::new(format!(".sail-write-{}", uuid::Uuid::new_v4())).unwrap();
        let target = CString::new(basename.as_bytes()).map_err(io::Error::other)?;
        let directory = parent.descriptor.as_raw_fd();
        // SAFETY: names remain valid and the pinned directory descriptor is owned.
        let fd = unsafe {
            libc::openat(
                directory,
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            // SAFETY: successful openat returned a newly owned descriptor.
            let mut file = unsafe { File::from_raw_fd(fd) };
            file.write_all(content)?;
            file.sync_all()?;
            // renameat replaces the directory entry, never a symlink's target.
            if unsafe { libc::renameat(directory, temporary.as_ptr(), directory, target.as_ptr()) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(content.len())
        })();
        if result.is_err() {
            unsafe { libc::unlinkat(directory, temporary.as_ptr(), 0) };
        }
        result
    }
    pub fn open(&self, requested: &Path) -> io::Result<ConfinedFile> {
        let relative = self.relative(requested)?;
        let name = CString::new(relative.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
        #[cfg(target_os = "linux")]
        {
            #[repr(C)]
            struct OpenHow {
                flags: u64,
                mode: u64,
                resolve: u64,
            }
            // NO_XDEV | NO_MAGICLINKS | NO_SYMLINKS | BENEATH. No unsafe fallback
            // on older kernels, unsupported filesystems or unavailable namespaces.
            let how = OpenHow {
                flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
                mode: 0,
                resolve: 0x01 | 0x02 | 0x04 | 0x08,
            };
            // SAFETY: both pointers remain valid for this synchronous syscall.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    &how,
                    std::mem::size_of::<OpenHow>(),
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a successful openat2 returns a uniquely owned descriptor.
            let descriptor = unsafe { File::from_raw_fd(fd as i32) };
            let metadata = descriptor.metadata()?;
            if (!metadata.is_file() && !metadata.is_dir())
                || (metadata.is_file() && std::os::unix::fs::MetadataExt::nlink(&metadata) != 1)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "special file denied",
                ));
            }
            Ok(ConfinedFile {
                path: self.path.join(relative),
                descriptor,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "confined filesystem access requires Linux openat2",
            ))
        }
    }
}

impl ConfinedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.descriptor.metadata()
    }
    fn descriptor_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.descriptor.as_raw_fd()))
    }
    pub fn read_limited(&self, limit: usize) -> io::Result<Vec<u8>> {
        if !self.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let mut data = Vec::new();
        // Reopen our own pinned inode, never the caller's original path.
        File::open(self.descriptor_path())?
            .take(limit.saturating_add(1) as u64)
            .read_to_end(&mut data)?;
        if data.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "read limit exceeded",
            ));
        }
        Ok(data)
    }
    pub fn entries(&self, limit: usize) -> io::Result<Vec<(String, bool)>> {
        if !self.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory",
            ));
        }
        std::fs::read_dir(self.descriptor_path())?
            .take(limit)
            .map(|entry| {
                let entry = entry?;
                Ok((
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.file_type()?.is_dir(),
                ))
            })
            .collect()
    }
}
