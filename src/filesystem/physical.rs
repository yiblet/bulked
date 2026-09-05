//! Physical filesystem implementation
//!
//! This module provides `PhysicalFS`, which uses the real OS filesystem.
//! This is the production adapter used by the CLI.

use super::{FilesystemError, ReadFs, WriteFs};
use std::borrow::Cow;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

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
        fs::rename(from, to).map_err(|source| FilesystemError::WriteError {
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
