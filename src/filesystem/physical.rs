//! Physical filesystem implementation
//!
//! This module provides `PhysicalFS`, which uses the real OS filesystem.
//! This is the production adapter used by the CLI.

use super::FileSystem;
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
    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
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

// Note: We don't add #[cfg(test)] tests for PhysicalFS here because
// testing it would require touching the real filesystem, which violates
// our hermetic testing principle. PhysicalFS is simple enough that we
// trust std::fs, and we test the FileSystem trait contract with MemoryFS.
