//! Staging filesystem decorator — the "pending set" used for atomic apply.
//!
//! [`StagingFs`] wraps any other [`FileSystem`] and turns writes into *staged*
//! writes: each `writer`/`write_string` call streams into a temporary file in the
//! underlying filesystem and records the `(temp, target)` pair instead of touching
//! the target. Nothing reaches a target path until [`StagingFs::commit`], which
//! moves each staged temp into place (an atomic rename when the underlying FS
//! supports it, otherwise a streaming copy — see [`super::move_file`]).
//!
//! This gives callers an all-or-nothing write set: stage every file, and only if
//! every stage succeeds do you commit. If anything fails first, dropping the
//! `StagingFs` deletes every staged temp file (RAII), leaving all targets
//! untouched.
//!
//! Reads, existence checks, and `as_real_path` delegate to the underlying FS, so a
//! `StagingFs` reads original file contents while staging their replacements.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{FileSystem, FilesystemError};

/// A staged `(temp, target)` write recorded by [`StagingFs`].
struct Staged {
    temp: PathBuf,
    target: PathBuf,
}

/// A [`FileSystem`] decorator that stages writes as tracked temp files so a set of
/// writes can be committed atomically (or discarded on drop).
pub struct StagingFs<'a> {
    inner: &'a dyn FileSystem,
    pending: Mutex<Vec<Staged>>,
    counter: AtomicUsize,
}

impl<'a> StagingFs<'a> {
    /// Wrap `inner` in a fresh, empty staging filesystem.
    pub fn new(inner: &'a dyn FileSystem) -> Self {
        Self {
            inner,
            pending: Mutex::new(Vec::new()),
            counter: AtomicUsize::new(0),
        }
    }

    /// Pick a unique temp path that lives in the same directory as `target` (so a
    /// commit can rename within one directory — i.e. one device — and stay atomic).
    fn temp_path_for(&self, target: &Path) -> PathBuf {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let mut name = target
            .file_name()
            .map(std::ffi::OsString::from)
            .unwrap_or_default();
        name.push(format!(".bulked-staged-{pid}-{n}"));
        target.with_file_name(name)
    }

    /// Commit every staged write: rename each temp file into its target.
    ///
    /// The temp and target always live in the same underlying filesystem (the temp
    /// is created next to its target), so a plain `inner.rename` is all that's
    /// needed — atomic on a real filesystem. Returns the `(target, error)` pairs for
    /// any files that failed to commit; a failure leaves earlier files already
    /// committed (there is no cross-file rollback). Temp files for failed moves are
    /// cleaned up best-effort, and any temp not reached is removed when `self` drops.
    pub fn commit(self) -> Result<(), Vec<(PathBuf, FilesystemError)>> {
        // Draining `pending` here means a later drop has nothing left to clean up.
        let staged = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        let mut errors = Vec::new();
        for s in &staged {
            if let Err(e) = self.inner.rename(&s.temp, &s.target) {
                let _ = self.inner.remove_file(&s.temp);
                errors.push((s.target.clone(), e));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl Drop for StagingFs<'_> {
    fn drop(&mut self) {
        if let Ok(pending) = self.pending.lock() {
            for s in pending.iter() {
                // Best-effort cleanup of any staged-but-uncommitted temp file.
                let _ = self.inner.remove_file(&s.temp);
            }
        }
    }
}

impl FileSystem for StagingFs<'_> {
    fn read_to_string(&self, path: &Path) -> Result<String, FilesystemError> {
        self.inner.read_to_string(path)
    }

    fn read(&self, path: &Path) -> Result<Box<dyn std::io::Read>, FilesystemError> {
        self.inner.read(path)
    }

    fn write_string(&self, path: &Path, content: &str) -> Result<(), FilesystemError> {
        let mut writer = self.writer(path)?;
        writer
            .write_all(content.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(|source| FilesystemError::WriteError {
                path: path.to_path_buf(),
                source,
            })
    }

    fn writer(&self, path: &Path) -> Result<Box<dyn std::io::Write>, FilesystemError> {
        let temp = self.temp_path_for(path);
        let writer = self.inner.writer(&temp)?;
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Staged {
                temp,
                target: path.to_path_buf(),
            });
        Ok(writer)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), FilesystemError> {
        self.inner.rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> Result<(), FilesystemError> {
        self.inner.remove_file(path)
    }

    fn as_real_path<'b>(&self, path: &'b Path) -> Option<Cow<'b, Path>> {
        self.inner.as_real_path(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.inner.is_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;

    #[test]
    fn staged_write_is_invisible_until_commit() {
        let inner = MemoryFS::new();
        let target = PathBuf::from("/t.txt");
        inner.add_file(&target, "orig").unwrap();

        let staging = StagingFs::new(&inner);
        staging.write_string(&target, "new").unwrap();

        // The target still holds its original content; the write is staged.
        assert_eq!(inner.read_to_string(&target).unwrap(), "orig");
        // Original target + one staged temp.
        assert_eq!(inner.file_count(), 2);

        staging.commit().unwrap();

        assert_eq!(inner.read_to_string(&target).unwrap(), "new");
        // Temp file was moved into place, not left behind.
        assert_eq!(inner.file_count(), 1);
    }

    #[test]
    fn dropping_without_commit_cleans_up_temps_and_leaves_target_untouched() {
        let inner = MemoryFS::new();
        let target = PathBuf::from("/t.txt");
        inner.add_file(&target, "orig").unwrap();

        {
            let staging = StagingFs::new(&inner);
            staging.write_string(&target, "new").unwrap();
            assert_eq!(inner.file_count(), 2);
            // staging dropped here without commit
        }

        assert_eq!(inner.read_to_string(&target).unwrap(), "orig");
        // Staged temp file was removed on drop.
        assert_eq!(inner.file_count(), 1);
    }
}
