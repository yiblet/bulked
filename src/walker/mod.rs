//! Directory traversal abstraction
//!
//! This module defines the Walker trait which provides an abstraction over
//! directory walking. This allows testing search logic with controlled
//! file lists without depending on actual filesystem traversal.

pub mod ignore_walker;
pub mod simple;

use std::path::PathBuf;

/// Abstract directory walker interface
///
/// This trait provides directory traversal operations. Implementations can be
/// backed by actual filesystem walkers (IgnoreWalker with .gitignore support)
/// or provide controlled file lists for testing (SimpleWalker).
pub trait Walker: Send + Sync {
    /// Get an iterator over all files to search
    ///
    /// Returns paths to files that should be searched. Directories are
    /// not included, only files.
    fn files(&self) -> Box<dyn Iterator<Item = PathBuf> + '_>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walker::simple::SimpleWalker;

    #[test]
    fn test_simple_walker_empty() {
        let walker = SimpleWalker::new(vec![]);
        let files: Vec<PathBuf> = walker.files().collect();
        assert_eq!(files.len(), 0);
    }

    #[test]
    fn test_simple_walker_with_files() {
        let paths = vec![
            PathBuf::from("/test/file1.txt"),
            PathBuf::from("/test/file2.txt"),
            PathBuf::from("/test/subdir/file3.txt"),
        ];

        let walker = SimpleWalker::new(paths.clone());
        let files: Vec<PathBuf> = walker.files().collect();

        assert_eq!(files.len(), 3);
        assert_eq!(files[0], PathBuf::from("/test/file1.txt"));
        assert_eq!(files[1], PathBuf::from("/test/file2.txt"));
        assert_eq!(files[2], PathBuf::from("/test/subdir/file3.txt"));
    }
}
