//! In-memory filesystem implementation for testing
//!
//! This module provides `MemoryFS`, a fake filesystem that stores all data in memory.
//! It's used for hermetic testing without touching the real filesystem.

use super::{FilesystemError, ReadFs, WriteFs};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// In-memory filesystem for testing
///
/// This is a "fake" implementation that provides a working filesystem
/// entirely in memory. It's fast, deterministic, and allows complete
/// control over the filesystem state in tests.
///
/// Besides implementing [`ReadFs`] + [`WriteFs`], it offers inherent
/// whole-string helpers (`add_file`, `read_to_string`, `write_string`,
/// `exists`, `file_count`) that tests use to set up and inspect state. Those
/// are deliberately *not* part of the trait surface.
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
    pub fn add_file(&self, path: &Path, content: &str) -> Result<(), FilesystemError> {
        self.add_file_bytes(path, content.as_bytes())
    }

    /// Add a file to the filesystem with binary content
    pub fn add_file_bytes(&self, path: &Path, content: &[u8]) -> Result<(), FilesystemError> {
        let mut files = self.files.write().map_err(|_| FilesystemError::LockError)?;
        files.insert(path.to_path_buf(), content.to_vec());
        Ok(())
    }

    /// Read the entire contents of a file as a string (test helper).
    ///
    /// Invalid UTF-8 is reported as a `ReadError` whose source is an
    /// `io::Error` of kind `InvalidData`, mirroring what a caller of
    /// [`ReadFs::read`] + `Read::read_to_string` would observe.
    pub fn read_to_string(&self, path: &Path) -> Result<String, FilesystemError> {
        let bytes = self.bytes(path)?;
        String::from_utf8(bytes).map_err(|e| FilesystemError::ReadError {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })
    }

    /// Replace the whole contents of a file with `content` (test helper).
    pub fn write_string(&self, path: &Path, content: &str) -> Result<(), FilesystemError> {
        self.add_file(path, content)
    }

    /// Whether `path` is stored (test helper).
    pub fn exists(&self, path: &Path) -> bool {
        self.files
            .read()
            .map(|files| files.contains_key(path))
            .unwrap_or(false)
    }

    /// Every path currently stored, unordered (test helper).
    pub fn paths(&self) -> Vec<PathBuf> {
        self.files
            .read()
            .map(|files| files.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Number of files currently stored (test helper for asserting temp cleanup).
    pub fn file_count(&self) -> usize {
        self.files.read().map(|files| files.len()).unwrap_or(0)
    }

    /// Clear all files from the filesystem
    pub fn clear(&self) {
        if let Ok(mut files) = self.files.write() {
            files.clear();
        }
    }

    fn bytes(&self, path: &Path) -> Result<Vec<u8>, FilesystemError> {
        let files = self.files.read().map_err(|_| FilesystemError::LockError)?;
        files
            .get(path)
            .cloned()
            .ok_or_else(|| FilesystemError::FileNotFound {
                path: path.to_path_buf(),
            })
    }
}

impl Default for MemoryFS {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadFs for MemoryFS {
    fn as_real_path<'a>(&self, _: &'a Path) -> Option<Cow<'a, Path>> {
        None
    }

    fn read(&self, path: &Path) -> Result<Box<dyn std::io::Read>, FilesystemError> {
        let bytes = self.bytes(path)?;
        Ok(Box::new(std::io::Cursor::new(bytes)))
    }
}

impl WriteFs for MemoryFS {
    fn writer(&self, path: &Path) -> Result<Box<dyn std::io::Write>, FilesystemError> {
        Ok(Box::new(MemoryWriter {
            files: Arc::clone(&self.files),
            path: path.to_path_buf(),
            buf: Vec::new(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), FilesystemError> {
        let mut files = self.files.write().map_err(|_| FilesystemError::LockError)?;
        let data = files
            .remove(from)
            .ok_or_else(|| FilesystemError::FileNotFound {
                path: from.to_path_buf(),
            })?;
        files.insert(to.to_path_buf(), data);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<(), FilesystemError> {
        let mut files = self.files.write().map_err(|_| FilesystemError::LockError)?;
        files
            .remove(path)
            .ok_or_else(|| FilesystemError::FileNotFound {
                path: path.to_path_buf(),
            })?;
        Ok(())
    }
}

/// Streaming writer for [`MemoryFS`].
///
/// Bytes are buffered and published into the in-memory map on `flush` — and on
/// drop, so a writer that is written-then-dropped lands its content like a real
/// file handle would.
struct MemoryWriter {
    files: Arc<RwLock<HashMap<PathBuf, Vec<u8>>>>,
    path: PathBuf,
    buf: Vec<u8>,
}

impl Write for MemoryWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut files = self
            .files
            .write()
            .map_err(|_| std::io::Error::other("memory fs lock poisoned"))?;
        files.insert(self.path.clone(), self.buf.clone());
        Ok(())
    }
}

impl Drop for MemoryWriter {
    fn drop(&mut self) {
        let _ = self.flush();
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
        assert_eq!(fs.read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn test_memory_fs_nonexistent_file() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/nonexistent.txt");

        assert!(!fs.exists(&path));
        assert!(matches!(
            fs.read_to_string(&path),
            Err(FilesystemError::FileNotFound { .. })
        ));
        assert!(matches!(
            fs.read(&path),
            Err(FilesystemError::FileNotFound { .. })
        ));
    }

    #[test]
    fn test_memory_fs_read_to_string_rejects_invalid_utf8() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/bin");
        fs.add_file_bytes(&path, &[0xff, 0xfe]).unwrap();

        match fs.read_to_string(&path) {
            Err(FilesystemError::ReadError { source, .. }) => {
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
            }
            other => panic!("expected ReadError(InvalidData), got {other:?}"),
        }
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
