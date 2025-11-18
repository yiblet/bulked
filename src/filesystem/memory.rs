//! In-memory filesystem implementation for testing
//!
//! This module provides `MemoryFS`, a fake filesystem that stores all data in memory.
//! It's used for hermetic testing without touching the real filesystem.

use super::{FileSystem, FilesystemError};
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
    pub fn add_file(&self, path: &Path, content: &str) -> Result<(), FilesystemError> {
        self.add_file_bytes(path, content.as_bytes())
    }

    /// Add a file to the filesystem with binary content
    pub fn add_file_bytes(&self, path: &Path, content: &[u8]) -> Result<(), FilesystemError> {
        let mut files = self.files.write().map_err(|_| FilesystemError::LockError)?;
        files.insert(path.to_path_buf(), content.to_vec());
        Ok(())
    }

    /// Remove a file from the filesystem
    pub fn remove_file(&self, path: &Path) -> Result<(), FilesystemError> {
        let mut files = self.files.write().map_err(|_| FilesystemError::LockError)?;
        files
            .remove(path)
            .ok_or_else(|| FilesystemError::FileNotFound {
                path: path.to_path_buf(),
            })?;
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

    fn read(&self, path: &Path) -> Result<Box<dyn std::io::Read>, FilesystemError> {
        let st = self.read_to_string(path)?;
        let vec = Vec::from(st);
        Ok(Box::new(std::io::Cursor::new(vec)))
    }

    fn read_to_string(&self, path: &Path) -> Result<String, FilesystemError> {
        let files = self.files.read().map_err(|_| FilesystemError::LockError)?;

        let bytes = files
            .get(path)
            .ok_or_else(|| FilesystemError::FileNotFound {
                path: path.to_path_buf(),
            })?;

        String::from_utf8(bytes.clone()).map_err(|source| FilesystemError::InvalidUtf8 {
            path: path.to_path_buf(),
            source,
        })
    }

    fn write_string(&self, path: &Path, content: &str) -> Result<(), FilesystemError> {
        let mut files = self.files.write().map_err(|_| FilesystemError::LockError)?;
        files.insert(path.to_path_buf(), content.as_bytes().to_vec());
        Ok(())
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

    #[test]
    fn test_memory_fs_write_string() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test.txt");
        let content = "Hello, world!";

        // Write the file
        fs.write_string(&path, content).unwrap();

        // Verify it exists and can be read back
        assert!(fs.exists(&path));
        assert_eq!(fs.read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn test_memory_fs_write_overwrites_existing() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test.txt");

        // Write initial content
        fs.write_string(&path, "initial").unwrap();
        assert_eq!(fs.read_to_string(&path).unwrap(), "initial");

        // Overwrite with new content
        fs.write_string(&path, "updated").unwrap();
        assert_eq!(fs.read_to_string(&path).unwrap(), "updated");
    }
}
