//! Filesystem abstraction - the primary test seam
//!
//! This module defines the filesystem ports. [`ReadFs`] is what searching,
//! ingesting, and verifying need; [`WriteFs`] is what applying needs on top of
//! that. [`FileSystem`] is the union of the two, implemented automatically for
//! every type that implements both. This allows the core logic to be tested
//! without touching the real filesystem.

#[cfg(test)]
pub mod memory;
pub mod physical;
pub mod staging;

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Errors that can occur during filesystem operations
#[derive(Debug, Error)]
pub enum FilesystemError {
    /// File not found
    #[error("File not found: {path}")]
    FileNotFound { path: PathBuf },

    /// Path exists but is not a file (e.g., directory)
    #[error("Not a file: {path}")]
    NotAFile { path: PathBuf },

    /// Failed to read file (including invalid UTF-8, which surfaces as an
    /// `io::Error` of kind `InvalidData` when a caller reads into a `String`)
    #[error("Failed to read file {path}: {source}")]
    ReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to write file
    #[error("Failed to write file {path}: {source}")]
    WriteError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Lock error (only produced by the test-only `MemoryFS`)
    #[cfg(test)]
    #[error("Lock error")]
    LockError,
}

/// Read side of the filesystem port.
///
/// This is all that `search`, `ingest`, and apply's verification phase need.
pub trait ReadFs: Send + Sync {
    /// Open a streaming reader over `path`.
    ///
    /// # Errors
    /// Returns [`FilesystemError::FileNotFound`] when the path does not exist and
    /// [`FilesystemError::ReadError`] (or `NotAFile`) for any other failure, so
    /// callers do not need a separate existence check.
    fn read(&self, path: &Path) -> Result<Box<dyn std::io::Read>, FilesystemError>;

    /// Performance escape hatch: when this returns `Some`, the path can be handed
    /// straight to the OS (e.g. memory-mapped search) instead of being read
    /// through [`ReadFs::read`].
    fn as_real_path<'a>(&self, path: &'a Path) -> Option<Cow<'a, Path>>;
}

/// Write side of the filesystem port.
///
/// Only apply needs this, and only through [`staging::StagingFs`].
pub trait WriteFs: Send + Sync {
    /// Open a streaming writer to `path`, creating it or truncating an existing file.
    ///
    /// This is the streaming dual of [`ReadFs::read`]; it lets callers write a
    /// file incrementally with bounded memory instead of materializing the whole
    /// contents up front.
    fn writer(&self, path: &Path) -> Result<Box<dyn std::io::Write>, FilesystemError>;

    /// Rename `from` to `to` within this filesystem (atomic on the real FS when both
    /// live on the same device).
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FilesystemError>;

    /// Remove a file.
    fn remove_file(&self, path: &Path) -> Result<(), FilesystemError>;
}

/// A full filesystem: anything that can both read and write.
///
/// This trait has no methods of its own; it exists so callers that need both
/// sides can name one trait object (`&dyn FileSystem`). It is implemented
/// automatically for every `ReadFs + WriteFs` type.
pub trait FileSystem: ReadFs + WriteFs {}

impl<T: ReadFs + WriteFs + ?Sized> FileSystem for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use std::io::{Read, Write};
    use std::path::PathBuf;

    /// Contract test that verifies any `FileSystem` implementation
    /// satisfies the basic requirements through the trait surface alone.
    #[allow(clippy::needless_pass_by_value)] // Test helper, generics require ownership
    fn test_filesystem_contract<F: FileSystem>(fs: F, test_file: &Path, test_content: &str) {
        // Test read
        let mut content = String::new();
        fs.read(test_file)
            .expect("Should open existing file")
            .read_to_string(&mut content)
            .expect("Should read existing file");
        assert_eq!(content, test_content);

        // Test nonexistent file: `read` carries the typed error, no `exists` needed
        let nonexistent = Path::new("/nonexistent.txt");
        assert!(
            matches!(
                fs.read(nonexistent),
                Err(FilesystemError::FileNotFound { ref path }) if path == nonexistent
            ),
            "Reading nonexistent file should yield FileNotFound"
        );

        // Test writer -> read round trip
        let written = PathBuf::from("/written.txt");
        {
            let mut w = fs.writer(&written).expect("writer should open");
            w.write_all(b"round trip").unwrap();
            w.flush().unwrap();
        }
        let mut back = String::new();
        fs.read(&written)
            .unwrap()
            .read_to_string(&mut back)
            .unwrap();
        assert_eq!(back, "round trip");

        // Test rename then remove
        let moved = PathBuf::from("/moved.txt");
        fs.rename(&written, &moved).expect("rename should succeed");
        assert!(matches!(
            fs.read(&written),
            Err(FilesystemError::FileNotFound { .. })
        ));
        fs.remove_file(&moved).expect("remove should succeed");
        assert!(matches!(
            fs.read(&moved),
            Err(FilesystemError::FileNotFound { .. })
        ));
    }

    #[test]
    fn test_memory_fs_satisfies_contract() {
        let fs = MemoryFS::new();
        let test_path = PathBuf::from("/test/file.txt");
        let test_content = "line 1\nline 2\nline 3\n";

        fs.add_file(&test_path, test_content).unwrap();

        test_filesystem_contract(fs, &test_path, test_content);
    }

    /// `MemoryWriter` must publish buffered bytes into the in-memory map both on
    /// explicit `flush` and implicitly on `drop`, mirroring a real file handle.
    #[test]
    fn test_memory_fs_writer_publishes_on_flush_and_drop() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/out.txt");

        let mut w = fs.writer(&path).expect("writer should open");
        w.write_all(b"hello ").unwrap();
        assert!(
            !fs.exists(&path),
            "nothing is published before the first flush"
        );

        w.flush().unwrap();
        assert_eq!(
            fs.read_to_string(&path).unwrap(),
            "hello ",
            "flush publishes the bytes written so far"
        );

        w.write_all(b"world\n").unwrap();
        drop(w);
        assert_eq!(
            fs.read_to_string(&path).unwrap(),
            "hello world\n",
            "drop publishes everything written, including bytes after the last flush"
        );
    }
}
