//! In-memory filesystem implementation for testing
//!
//! This module provides `MemoryFS`, a fake filesystem that stores all data in memory.
//! It's used for hermetic testing without touching the real filesystem.

use super::FileSystem;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// In-memory filesystem for testing
///
/// This is a "fake" implementation that provides a working filesystem
/// entirely in memory. It's fast, deterministic, and allows complete
/// control over the filesystem state in tests.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct MemoryFS {
    files: Arc<RwLock<HashMap<PathBuf, Vec<u8>>>>,
}

#[allow(dead_code)]
impl MemoryFS {
    /// Create a new empty in-memory filesystem
    pub fn new() -> Self {
        Self {
            files: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add a file to the filesystem with string content
    pub fn add_file(&self, path: &Path, content: &str) -> Result<(), String> {
        self.add_file_bytes(path, content.as_bytes())
    }

    /// Add a file to the filesystem with binary content
    pub fn add_file_bytes(&self, path: &Path, content: &[u8]) -> Result<(), String> {
        let mut files = self
            .files
            .write()
            .map_err(|e| format!("Lock error: {e}"))?;
        files.insert(path.to_path_buf(), content.to_vec());
        Ok(())
    }

    /// Remove a file from the filesystem
    pub fn remove_file(&self, path: &Path) -> Result<(), String> {
        let mut files = self
            .files
            .write()
            .map_err(|e| format!("Lock error: {e}"))?;
        files
            .remove(path)
            .ok_or_else(|| format!("File not found: {}", path.display()))?;
        Ok(())
    }

    /// Clear all files from the filesystem
    pub fn clear(&self) {
        if let Ok(mut files) = self.files.write() {
            files.clear();
        }
    }
}

impl Default for MemoryFS {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystem for MemoryFS {
    fn as_real_path<'a>(&self, _: &'a Path) -> Option<Cow<'a, Path>> {
        None
    }

    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        let files = self
            .files
            .read()
            .map_err(|e| format!("Lock error: {e}"))?;

        let bytes = files
            .get(path)
            .ok_or_else(|| format!("File not found: {}", path.display()))?;

        String::from_utf8(bytes.clone())
            .map_err(|e| format!("Invalid UTF-8 in file {}: {}", path.display(), e))
    }

    fn exists(&self, path: &Path) -> bool {
        self.files
            .read()
            .map(|files| files.contains_key(path))
            .unwrap_or(false)
    }

    fn is_file(&self, path: &Path) -> bool {
        // In MemoryFS, everything stored is a file
        self.exists(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_fs_create_and_read() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        let content = "hello world";

        fs.add_file(&path, content).unwrap();

        assert!(fs.exists(&path));
        assert!(fs.is_file(&path));
        assert_eq!(fs.read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn test_memory_fs_nonexistent_file() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/nonexistent.txt");

        assert!(!fs.exists(&path));
        assert!(fs.read_to_string(&path).is_err());
    }

    #[test]
    fn test_memory_fs_remove_file() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test.txt");

        fs.add_file(&path, "content").unwrap();
        assert!(fs.exists(&path));

        fs.remove_file(&path).unwrap();
        assert!(!fs.exists(&path));
    }

    #[test]
    fn test_memory_fs_clear() {
        let fs = MemoryFS::new();
        fs.add_file(&PathBuf::from("/file1.txt"), "content1")
            .unwrap();
        fs.add_file(&PathBuf::from("/file2.txt"), "content2")
            .unwrap();

        assert!(fs.exists(&PathBuf::from("/file1.txt")));
        assert!(fs.exists(&PathBuf::from("/file2.txt")));

        fs.clear();

        assert!(!fs.exists(&PathBuf::from("/file1.txt")));
        assert!(!fs.exists(&PathBuf::from("/file2.txt")));
    }
}
