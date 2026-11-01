//! File I/O shared by the state files: crash-safe replacement and reading
//! a file that may not exist yet.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

/// Replaces `path` with `bytes` atomically: the parent directory is created,
/// the bytes go to a temporary file in that same directory (so the rename
/// never crosses a filesystem), are synced, and the file is renamed over
/// `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// The file's contents, or `None` when it does not exist; any other I/O
/// error is returned.
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// An exclusive lock on the sibling `<path>.lock` — never replaced, unlike
/// `path` itself — creating its parent directory if needed. The lock is
/// held until the returned file is dropped. An error carries the path that
/// failed (the parent directory or the lock file).
pub(crate) fn lock_sibling(path: &Path) -> Result<File, (PathBuf, io::Error)> {
    let mut name = OsString::from(path.as_os_str());
    name.push(".lock");
    let lock_path = PathBuf::from(name);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|e| (parent.to_path_buf(), e))?;
    }
    let lock = File::create(&lock_path).map_err(|e| (lock_path.clone(), e))?;
    lock.lock().map_err(|e| (lock_path, e))?;
    Ok(lock)
}
