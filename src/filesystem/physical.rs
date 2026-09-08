//! Physical filesystem implementation
//!
//! This module provides `PhysicalFS`, which uses the real OS filesystem.
//! This is the production adapter used by the CLI.

use super::{FilesystemError, ReadFs, WriteFs};
use std::borrow::Cow;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Physical filesystem adapter
///
/// This adapter uses `std::fs` to interact with the real filesystem.
/// It's used in production but never in tests (tests use `MemoryFS`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysicalFS;

impl PhysicalFS {
    /// Create a new `PhysicalFS` instance
    pub fn new() -> Self {
        Self
    }
}

impl ReadFs for PhysicalFS {
    fn as_real_path<'a>(&self, path: &'a Path) -> Option<Cow<'a, Path>> {
        Some(Cow::Borrowed(path))
    }

    fn read(&self, path: &Path) -> Result<Box<dyn std::io::Read>, FilesystemError> {
        // Map the OS error onto the typed variants so callers (notably the
        // searcher) can distinguish "missing" from "unreadable" without a
        // racy `exists()` pre-check.
        let file = fs::File::open(path).map_err(|source| match source.kind() {
            ErrorKind::NotFound => FilesystemError::FileNotFound {
                path: path.to_path_buf(),
            },
            ErrorKind::IsADirectory => FilesystemError::NotAFile {
                path: path.to_path_buf(),
            },
            _ => FilesystemError::ReadError {
                path: path.to_path_buf(),
                source,
            },
        })?;
        Ok(Box::new(file))
    }
}

impl WriteFs for PhysicalFS {
    fn writer(&self, path: &Path) -> Result<Box<dyn std::io::Write>, FilesystemError> {
        let file = fs::File::create(path).map_err(|source| FilesystemError::WriteError {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Box::new(std::io::BufWriter::new(file)))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), FilesystemError> {
        rename_across_devices(from, to).map_err(|source| FilesystemError::WriteError {
            path: to.to_path_buf(),
            source,
        })
    }

    fn remove_file(&self, path: &Path) -> Result<(), FilesystemError> {
        fs::remove_file(path).map_err(|source| FilesystemError::WriteError {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// `rename(2)`, with a fallback for when `from` and `to` are on different devices.
///
/// The cheapest way to know whether a rename will work is to try it: one
/// syscall, and the kernel answers `EXDEV` (`ErrorKind::CrossesDevices`) when it
/// cannot. In that case the bytes are copied into a temp file *beside* `to` and
/// that sibling is renamed into place, so `to` is still replaced in a single
/// step and a reader never sees a half-written file. The sibling exists only
/// for the duration of the copy; on any failure it is removed.
fn rename_across_devices(from: &Path, to: &Path) -> std::io::Result<()> {
    match fs::rename(from, to) {
        Err(e) if e.kind() == ErrorKind::CrossesDevices => {}
        other => return other,
    }

    let sibling = sibling_temp_path(to);
    let copied = fs::copy(from, &sibling).and_then(|_| fs::rename(&sibling, to));
    if copied.is_err() {
        let _ = fs::remove_file(&sibling);
        return copied.map(|_| ());
    }
    fs::remove_file(from)
}

/// A unique scratch name in the same directory as `target`.
fn sibling_temp_path(target: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(format!(".bulked-{}-{nanos}.tmp", std::process::id()));
    target.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "bulked-test-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_rename_replaces_target_and_removes_source() {
        let scratch = Scratch::new("rename");
        let from = scratch.0.join("from.txt");
        let to = scratch.0.join("to.txt");
        fs::write(&from, "new").unwrap();
        fs::write(&to, "old").unwrap();

        PhysicalFS.rename(&from, &to).unwrap();

        assert_eq!(fs::read_to_string(&to).unwrap(), "new");
        assert!(!from.exists());
        assert_eq!(
            fs::read_dir(&scratch.0).unwrap().count(),
            1,
            "no sibling temp left behind"
        );
    }

    #[test]
    fn test_rename_reports_missing_source() {
        let scratch = Scratch::new("missing");
        let err = PhysicalFS
            .rename(&scratch.0.join("nope"), &scratch.0.join("to.txt"))
            .unwrap_err();
        assert!(matches!(err, FilesystemError::WriteError { .. }));
    }

    #[test]
    fn test_sibling_temp_path_stays_in_the_target_directory() {
        let p = sibling_temp_path(Path::new("/a/b/edits.bk"));
        assert_eq!(p.parent(), Some(Path::new("/a/b")));
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(".edits.bk.bulked-") && name.ends_with(".tmp"),
            "{name}"
        );
    }
}
