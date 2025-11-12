//! Simple walker implementation for testing
//!
//! This module provides `SimpleWalker`, a test double that returns a predefined
//! list of files. This allows testing search logic without filesystem traversal.

use super::Walker;
use std::path::PathBuf;

/// Simple walker for testing
///
/// This walker returns a predefined list of file paths.
/// It's used in tests to control exactly which files are "walked".
#[allow(dead_code)]
pub(crate) struct SimpleWalker {
    files: Vec<PathBuf>,
}

#[allow(dead_code)]
impl SimpleWalker {
    /// Create a new simple walker with the given file list
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self { files }
    }

    /// Create a simple walker from string paths (convenience for tests)
    pub fn from_paths<S: AsRef<str>>(paths: &[S]) -> Self {
        let files = paths.iter().map(|s| PathBuf::from(s.as_ref())).collect();
        Self::new(files)
    }
}

impl Walker for SimpleWalker {
    fn files(&self) -> Box<dyn Iterator<Item = PathBuf> + '_> {
        Box::new(self.files.clone().into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_walker_empty() {
        let walker = SimpleWalker::new(vec![]);
        let files: Vec<PathBuf> = walker.files().collect();
        assert!(files.is_empty());
    }

    #[test]
    fn test_simple_walker_single_file() {
        let walker = SimpleWalker::new(vec![PathBuf::from("/test/file.txt")]);
        let files: Vec<PathBuf> = walker.files().collect();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0], PathBuf::from("/test/file.txt"));
    }

    #[test]
    fn test_simple_walker_multiple_files() {
        let walker = SimpleWalker::new(vec![
            PathBuf::from("/test/file1.txt"),
            PathBuf::from("/test/file2.txt"),
            PathBuf::from("/test/file3.txt"),
        ]);

        let files: Vec<PathBuf> = walker.files().collect();

        assert_eq!(files.len(), 3);
    }

    #[test]
    fn test_simple_walker_from_paths() {
        let walker = SimpleWalker::from_paths(&["/test/file1.txt", "/test/file2.txt"]);

        let files: Vec<PathBuf> = walker.files().collect();

        assert_eq!(files.len(), 2);
        assert_eq!(files[0], PathBuf::from("/test/file1.txt"));
        assert_eq!(files[1], PathBuf::from("/test/file2.txt"));
    }
}
