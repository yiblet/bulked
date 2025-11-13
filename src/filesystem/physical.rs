//! Physical filesystem implementation
//!
//! This module provides `PhysicalFS`, which uses the real OS filesystem.
//! This is the production adapter used by the CLI.

use super::{FileSystem, FilesystemError};
use std::borrow::Cow;
use std::fs;
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

impl FileSystem for PhysicalFS {
    fn read_to_string(&self, path: &Path) -> Result<String, FilesystemError> {
        fs::read_to_string(path).map_err(|source| FilesystemError::ReadError {
            path: path.to_path_buf(),
            source,
        })
    }

    fn write_string(&self, path: &Path, content: &str) -> Result<(), FilesystemError> {
        fs::write(path, content).map_err(|source| FilesystemError::WriteError {
            path: path.to_path_buf(),
            source,
        })
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn as_real_path<'a>(&self, path: &'a Path) -> Option<Cow<'a, Path>> {
        Some(Cow::Borrowed(path))
    }
}
