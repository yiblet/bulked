//! In-memory filesystem implementation for testing
//!
//! This module provides MemoryFS, a fake filesystem that stores all data in memory.
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
            .map_err(|e| format!("Lock error: {}", e))?;
        files.insert(path.to_path_buf(), content.to_vec());
        Ok(())
    }

    /// Remove a file from the filesystem
    pub fn remove_file(&self, path: &Path) -> Result<(), String> {
        let mut files = self
            .files
            .write()
            .map_err(|e| format!("Lock error: {}", e))?;
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
            .map_err(|e| format!("Lock error: {}", e))?;

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

    fn read_line_at(&self, path: &Path, line_number: usize) -> Result<String, String> {
        if line_number == 0 {
            return Err("Line numbers are 1-indexed".to_string());
        }

        let content = self.read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();

        if line_number > lines.len() {
            return Err(format!(
                "Line {} out of range (file has {} lines)",
                line_number,
                lines.len()
            ));
        }

        Ok(lines[line_number - 1].to_string())
    }

    fn read_line_range(
        &self,
        path: &Path,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<String>, String> {
        if start_line == 0 || end_line == 0 {
            return Err("Line numbers are 1-indexed".to_string());
        }

        if start_line > end_line {
            return Err(format!(
                "Start line {} is greater than end line {}",
                start_line, end_line
            ));
        }

        let content = self.read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();

        if start_line > lines.len() {
            return Err(format!(
                "Start line {} out of range (file has {} lines)",
                start_line,
                lines.len()
            ));
        }

        let actual_end = end_line.min(lines.len());
        Ok(lines[start_line - 1..actual_end]
            .iter()
            .map(|s| s.to_string())
            .collect())
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
    fn test_memory_fs_read_line_at() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test.txt");
        fs.add_file(&path, "line 1\nline 2\nline 3\n").unwrap();

        assert_eq!(fs.read_line_at(&path, 1).unwrap(), "line 1");
        assert_eq!(fs.read_line_at(&path, 2).unwrap(), "line 2");
        assert_eq!(fs.read_line_at(&path, 3).unwrap(), "line 3");

        assert!(fs.read_line_at(&path, 0).is_err()); // Invalid line number
        assert!(fs.read_line_at(&path, 4).is_err()); // Out of range
    }

    #[test]
    fn test_memory_fs_read_line_range() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test.txt");
        fs.add_file(&path, "line 1\nline 2\nline 3\nline 4\nline 5\n")
            .unwrap();

        let lines = fs.read_line_range(&path, 2, 4).unwrap();
        assert_eq!(lines, vec!["line 2", "line 3", "line 4"]);

        // Test range at boundaries
        let lines = fs.read_line_range(&path, 1, 2).unwrap();
        assert_eq!(lines, vec!["line 1", "line 2"]);

        // Test range beyond file end (should clamp)
        let lines = fs.read_line_range(&path, 3, 100).unwrap();
        assert_eq!(lines, vec!["line 3", "line 4", "line 5"]);

        // Test invalid ranges
        assert!(fs.read_line_range(&path, 0, 2).is_err());
        assert!(fs.read_line_range(&path, 3, 2).is_err());
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
