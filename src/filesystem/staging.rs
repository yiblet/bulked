//! Staging filesystem decorator — a journaled, transactional [`FileSystem`].
//!
//! [`StagingFs`] wraps any other [`FileSystem`] and turns every write-side
//! operation into a **journal entry** instead of a mutation of the inner FS:
//!
//! - [`WriteFs::writer`] streams bytes into a temporary file created in the
//!   `temp_dir` given to [`StagingFs::new`] and records `Write { temp, target }`.
//!   The temp deliberately lives *away* from the target: `bulked search` over the
//!   tree never sees half-written output, and a run that is interrupted leaves
//!   nothing behind in the user's directories. The commit-time move is the inner
//!   FS's [`WriteFs::rename`], which the physical adapter makes atomic for the
//!   target even when the temp dir is on another device.
//! - [`WriteFs::rename`] records `Rename { from, to }` and returns `Ok` without
//!   touching the inner FS.
//! - [`WriteFs::remove_file`] records `Remove { path }` and returns `Ok`
//!   without touching the inner FS.
//!
//! Nothing reaches a target path until [`StagingFs::commit`], which replays the
//! journal **in order** (each `Write` becomes `inner.rename(temp, target)`).
//! Dropping a `StagingFs` without committing deletes every staged temp file and
//! discards the pending renames/removes, leaving the inner FS exactly as it was.
//!
//! Reads ([`ReadFs::read`], [`ReadFs::as_real_path`]) delegate straight to the
//! inner FS. That means **staged writes are not visible through `read`**: a
//! `StagingFs` always reads original contents while their replacements are
//! being staged, which is exactly what apply needs to reconstruct a file from
//! its current version.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{FileSystem, FilesystemError, ReadFs, WriteFs};

/// One journaled write-side operation recorded by [`StagingFs`].
enum StagedOp {
    /// Bytes were streamed into `temp` (in the staging temp dir); on commit,
    /// `inner.rename(temp, target)`.
    Write { temp: PathBuf, target: PathBuf },
    /// On commit, `inner.rename(from, to)`.
    Rename { from: PathBuf, to: PathBuf },
    /// On commit, `inner.remove_file(path)`.
    Remove { path: PathBuf },
}

/// A [`FileSystem`] decorator that journals writes, renames, and removes so a
/// whole set of mutations can be committed in order (or discarded on drop).
pub struct StagingFs<'a> {
    inner: &'a dyn FileSystem,
    /// Where staged temp files are created, in `inner`. A policy of this
    /// decorator, supplied by the composition root (`std::env::temp_dir()` in
    /// production), not a property of the filesystem port.
    temp_dir: PathBuf,
    journal: Mutex<Vec<StagedOp>>,
    counter: AtomicUsize,
    /// Distinguishes this instance's temps from another `StagingFs` in the same
    /// process (and from a crashed earlier run that reused the pid).
    nonce: u32,
}

impl<'a> StagingFs<'a> {
    /// Wrap `inner` in a fresh staging filesystem with an empty journal, staging
    /// its temp files under `temp_dir` (which must exist in `inner`).
    pub fn new(inner: &'a dyn FileSystem, temp_dir: impl Into<PathBuf>) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        Self {
            inner,
            temp_dir: temp_dir.into(),
            journal: Mutex::new(Vec::new()),
            counter: AtomicUsize::new(0),
            nonce,
        }
    }

    /// Pick a unique temp path in the staging temp dir, named after `target` so
    /// a leftover from a crash is recognizable.
    fn temp_path_for(&self, target: &Path) -> PathBuf {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let mut name = std::ffi::OsString::from(format!("bulked-{pid}-{}-{n}-", self.nonce));
        name.push(target.file_name().unwrap_or_default());
        self.temp_dir.join(name)
    }

    fn push(&self, op: StagedOp) {
        self.journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(op);
    }

    /// Replay the journal against the inner filesystem, in the order the
    /// operations were issued.
    ///
    /// Stops at the **first** failure and returns it as a `(path, error)` pair.
    /// Operations before the failure are already applied (there is no cross-op
    /// rollback). Every `Write` temp not yet renamed into place — the failed one
    /// and all later ones — is removed best-effort so nothing is left behind.
    pub fn commit(self) -> Result<(), Vec<(PathBuf, FilesystemError)>> {
        // Draining the journal here means the later `Drop` has nothing to clean up.
        let ops = std::mem::take(&mut *self.journal.lock().unwrap_or_else(|e| e.into_inner()));
        let mut ops = ops.into_iter();
        let mut failure = None;

        for op in ops.by_ref() {
            let result = match &op {
                StagedOp::Write { temp, target } => self.inner.rename(temp, target).map_err(|e| {
                    let _ = self.inner.remove_file(temp);
                    (target.clone(), e)
                }),
                StagedOp::Rename { from, to } => {
                    self.inner.rename(from, to).map_err(|e| (to.clone(), e))
                }
                StagedOp::Remove { path } => {
                    self.inner.remove_file(path).map_err(|e| (path.clone(), e))
                }
            };
            if let Err(f) = result {
                failure = Some(f);
                break;
            }
        }

        match failure {
            None => Ok(()),
            Some(f) => {
                // Best-effort cleanup of the temps for every op we never reached.
                for op in ops {
                    if let StagedOp::Write { temp, .. } = op {
                        let _ = self.inner.remove_file(&temp);
                    }
                }
                Err(vec![f])
            }
        }
    }
}

impl Drop for StagingFs<'_> {
    fn drop(&mut self) {
        // Anything still journaled was never committed: remove the staged temp
        // files (best-effort) and simply forget the pending renames/removes.
        let ops = std::mem::take(&mut *self.journal.lock().unwrap_or_else(|e| e.into_inner()));
        for op in ops {
            if let StagedOp::Write { temp, .. } = op {
                let _ = self.inner.remove_file(&temp);
            }
        }
    }
}

impl ReadFs for StagingFs<'_> {
    fn read(&self, path: &Path) -> Result<Box<dyn std::io::Read>, FilesystemError> {
        self.inner.read(path)
    }

    fn as_real_path<'b>(&self, path: &'b Path) -> Option<Cow<'b, Path>> {
        self.inner.as_real_path(path)
    }
}

impl WriteFs for StagingFs<'_> {
    fn writer(&self, path: &Path) -> Result<Box<dyn std::io::Write>, FilesystemError> {
        let temp = self.temp_path_for(path);
        let writer = self.inner.writer(&temp)?;
        self.push(StagedOp::Write {
            temp,
            target: path.to_path_buf(),
        });
        Ok(writer)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), FilesystemError> {
        self.push(StagedOp::Rename {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<(), FilesystemError> {
        self.push(StagedOp::Remove {
            path: path.to_path_buf(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use std::io::Write;

    /// Stage `content` for `target` through the trait's streaming `writer`.
    fn stage_write(staging: &StagingFs<'_>, target: &Path, content: &str) {
        let mut w = staging.writer(target).unwrap();
        w.write_all(content.as_bytes()).unwrap();
        w.flush().unwrap();
    }

    #[test]
    fn staged_write_is_invisible_until_commit() {
        let inner = MemoryFS::new();
        let target = PathBuf::from("/t.txt");
        inner.add_file(&target, "orig").unwrap();

        let staging = StagingFs::new(&inner, "/tmp");
        stage_write(&staging, &target, "new");

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
            let staging = StagingFs::new(&inner, "/tmp");
            stage_write(&staging, &target, "new");
            assert_eq!(inner.file_count(), 2);
            // staging dropped here without commit
        }

        assert_eq!(inner.read_to_string(&target).unwrap(), "orig");
        // Staged temp file was removed on drop.
        assert_eq!(inner.file_count(), 1);
    }

    #[test]
    fn staged_temp_lives_in_the_temp_dir_not_beside_the_target() {
        let inner = MemoryFS::new();
        let target = PathBuf::from("/work/src/edits.bk");
        inner.add_file(&target, "orig").unwrap();

        let staging = StagingFs::new(&inner, "/tmp");
        stage_write(&staging, &target, "new");

        let temps: Vec<PathBuf> = inner.paths().into_iter().filter(|p| *p != target).collect();
        assert_eq!(temps.len(), 1);
        assert_eq!(
            temps[0].parent(),
            Some(Path::new("/tmp")),
            "temp must not be created in the target's directory: {temps:?}"
        );
        assert!(
            temps[0].to_string_lossy().ends_with("-edits.bk"),
            "temp is named after its target: {temps:?}"
        );

        staging.commit().unwrap();
        assert_eq!(inner.read_to_string(&target).unwrap(), "new");
        assert_eq!(inner.file_count(), 1);
    }

    #[test]
    fn test_staging_rename_is_deferred_until_commit() {
        let inner = MemoryFS::new();
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        inner.add_file(&a, "content").unwrap();

        let staging = StagingFs::new(&inner, "/tmp");
        staging.rename(&a, &b).unwrap();

        // Journaled only: the inner FS is untouched.
        assert!(inner.exists(&a));
        assert!(!inner.exists(&b));

        staging.commit().unwrap();

        assert!(inner.exists(&b));
        assert!(!inner.exists(&a));
        assert_eq!(inner.read_to_string(&b).unwrap(), "content");
    }

    #[test]
    fn test_staging_remove_is_deferred_until_commit() {
        let inner = MemoryFS::new();
        let a = PathBuf::from("/a");
        inner.add_file(&a, "content").unwrap();

        let staging = StagingFs::new(&inner, "/tmp");
        staging.remove_file(&a).unwrap();

        // Journaled only: still present.
        assert!(inner.exists(&a));

        staging.commit().unwrap();

        assert!(!inner.exists(&a));
        assert_eq!(inner.file_count(), 0);
    }

    #[test]
    fn test_staging_drop_discards_rename_and_remove() {
        let inner = MemoryFS::new();
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        let c = PathBuf::from("/c");
        inner.add_file(&a, "A").unwrap();
        inner.add_file(&c, "C").unwrap();

        {
            let staging = StagingFs::new(&inner, "/tmp");
            staging.rename(&a, &b).unwrap();
            staging.remove_file(&c).unwrap();
            // dropped without commit
        }

        assert!(inner.exists(&a));
        assert!(!inner.exists(&b));
        assert!(inner.exists(&c));
        assert_eq!(inner.read_to_string(&a).unwrap(), "A");
        assert_eq!(inner.read_to_string(&c).unwrap(), "C");
        assert_eq!(inner.file_count(), 2);
    }

    #[test]
    fn test_staging_journal_replays_in_order() {
        let inner = MemoryFS::new();
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        inner.add_file(&a, "orig").unwrap();

        let staging = StagingFs::new(&inner, "/tmp");
        stage_write(&staging, &a, "new");
        staging.rename(&a, &b).unwrap();

        // Nothing has happened yet beyond the temp file appearing.
        assert_eq!(inner.read_to_string(&a).unwrap(), "orig");
        assert!(!inner.exists(&b));

        staging.commit().unwrap();

        // Write landed on /a first, then the rename moved it to /b.
        assert_eq!(inner.read_to_string(&b).unwrap(), "new");
        assert!(!inner.exists(&a));
        assert_eq!(inner.file_count(), 1);
    }

    #[test]
    fn test_staging_commit_stops_at_first_failure_and_cleans_later_temps() {
        let inner = MemoryFS::new();
        let a = PathBuf::from("/a");
        let missing = PathBuf::from("/missing");
        let z = PathBuf::from("/z");
        inner.add_file(&a, "orig").unwrap();

        let staging = StagingFs::new(&inner, "/tmp");
        stage_write(&staging, &a, "new"); // ok
        staging.remove_file(&missing).unwrap(); // will fail on commit
        stage_write(&staging, &z, "never"); // must be cleaned up, never committed

        let failures = staging.commit().unwrap_err();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, missing);
        assert!(matches!(
            failures[0].1,
            FilesystemError::FileNotFound { .. }
        ));

        // The op before the failure was applied; the one after was not and left no temp.
        assert_eq!(inner.read_to_string(&a).unwrap(), "new");
        assert!(!inner.exists(&z));
        assert_eq!(inner.file_count(), 1);
    }
}
