//! Production walker using ignore crate
//!
//! This module provides `IgnoreWalker`, which uses the ignore crate to traverse
//! directories while respecting .gitignore files. This is the production
//! implementation based on the same infrastructure used by ripgrep and Helix.

use super::Walker;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Production walker using ignore crate
///
/// This walker respects .gitignore files and other ignore patterns.
/// It's used in production to efficiently traverse large directory trees.
pub struct IgnoreWalker {
    root: PathBuf,
    respect_gitignore: bool,
    include_hidden: bool,
}

impl IgnoreWalker {
    /// Create a new ignore walker
    ///
    /// # Arguments
    ///
    /// * `root` - Root directory to start walking from
    /// * `respect_gitignore` - Whether to respect .gitignore files
    /// * `hidden` - Whether to include hidden files
    pub fn new(root: impl AsRef<Path>, respect_gitignore: bool, hidden: bool) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            respect_gitignore,
            include_hidden: hidden,
        }
    }
}

impl Walker for IgnoreWalker {
    fn files(&self) -> Box<dyn Iterator<Item = PathBuf> + '_> {
        let mut walker = WalkBuilder::new(&self.root);

        let walker = walker
            .git_ignore(self.respect_gitignore)
            .git_global(self.respect_gitignore)
            .git_exclude(self.respect_gitignore)
            .hidden(!self.include_hidden);

        let walker = if self.respect_gitignore {
            walker.filter_entry(|entry| {
                // Always skip .git directories
                entry.file_name() != ".git"
            })
        } else {
            walker
        };

        let walker = walker.build();

        Box::new(
            walker
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
                .map(|entry| entry.path().to_path_buf()),
        )
    }
}

// Note: We don't add #[cfg(test)] tests for IgnoreWalker here because
// testing it would require creating real directories and .gitignore files,
// which violates our hermetic testing principle. IgnoreWalker is a thin
// wrapper over the ignore crate, which is well-tested. We test the Walker
// trait contract with SimpleWalker.
