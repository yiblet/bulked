//! Filesystem abstraction - the primary test seam
//!
//! This module defines the `FileSystem` trait which provides an abstraction over
//! filesystem operations. This allows the core search logic to be tested without
//! touching the real filesystem.

pub mod memory;
pub mod physical;

use std::{borrow::Cow, path::Path};

/// Abstract filesystem interface
///
/// This trait provides the operations needed for searching files.
/// Implementations can be backed by real filesystem (`PhysicalFS`) or
/// in-memory storage (`MemoryFS` for testing).
pub trait FileSystem: Send + Sync {
    /// Read the entire contents of a file as a string
    ///
    /// Returns an error if the file doesn't exist, isn't readable, or contains invalid UTF-8.
    fn read_to_string(&self, path: &Path) -> Result<String, String>;

    fn as_real_path<'a>(&self, path: &'a Path) -> Option<Cow<'a, Path>>;

    /// Check if a path exists
    fn exists(&self, path: &Path) -> bool;

    /// Check if a path points to a file (not a directory)
    fn is_file(&self, path: &Path) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use std::path::PathBuf;

    /// Contract test that verifies any `FileSystem` implementation
    /// satisfies the basic requirements
    #[allow(clippy::needless_pass_by_value)] // Test helper, generics require ownership
    fn test_filesystem_contract<F: FileSystem>(fs: F, test_file: &Path, test_content: &str) {
        // Test exists
        assert!(fs.exists(test_file), "File should exist");

        // Test is_file
        assert!(fs.is_file(test_file), "Should be identified as file");

        // Test read_to_string
        let content = fs
            .read_to_string(test_file)
            .expect("Should read existing file");
        assert_eq!(content, test_content);

        // Test nonexistent file
        let nonexistent = Path::new("/nonexistent.txt");
        assert!(!fs.exists(nonexistent), "Nonexistent file should not exist");
        assert!(
            fs.read_to_string(nonexistent).is_err(),
            "Reading nonexistent file should error"
        );
    }

    #[test]
    fn test_memory_fs_satisfies_contract() {
        let fs = MemoryFS::new();
        let test_path = PathBuf::from("/test/file.txt");
        let test_content = "line 1\nline 2\nline 3\n";

        fs.add_file(&test_path, test_content).unwrap();

        test_filesystem_contract(fs, &test_path, test_content);
    }
}
