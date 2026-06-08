//! Production walker using ignore crate
//!
//! This module provides `IgnoreWalker`, which uses the ignore crate to traverse
//! directories while respecting .gitignore files. This is the production
//! implementation based on the same infrastructure used by ripgrep and Helix.

use super::Walker;
use ignore::WalkBuilder;
use std::{collections::HashSet, path::PathBuf, sync::Mutex};

/// Production walker using ignore crate
///
/// This walker respects .gitignore files and other ignore patterns.
/// It's used in production to efficiently traverse large directory trees.
pub struct IgnoreWalker {
    roots: Vec<PathBuf>,
    respect_gitignore: bool,
    include_hidden: bool,
    include_bk: bool,
}

impl IgnoreWalker {
    /// Create a new ignore walker
    ///
    /// # Arguments
    ///
    /// * `root` - Root directory to start walking from
    /// * `respect_gitignore` - Whether to respect .gitignore files
    /// * `hidden` - Whether to include hidden files
    /// * `include_bk` - Whether to include bulked's own `.bk` output files
    pub fn new(
        roots: Vec<PathBuf>,
        respect_gitignore: bool,
        hidden: bool,
        include_bk: bool,
    ) -> Self {
        Self {
            roots,
            respect_gitignore,
            include_hidden: hidden,
            include_bk,
        }
    }
}

impl Walker for IgnoreWalker {
    fn files(&self) -> Box<dyn Iterator<Item = PathBuf> + '_> {
        let Some((root, rem)) = self.roots.split_first() else {
            return Box::new(std::iter::empty());
        };

        let mut walker = WalkBuilder::new(root);
        for path in rem {
            walker.add(path);
        }

        let walker = walker
            .git_ignore(self.respect_gitignore)
            .git_global(self.respect_gitignore)
            .git_exclude(self.respect_gitignore)
            .hidden(!self.include_hidden);

        let visited = Mutex::new(HashSet::new());

        let walker = if self.respect_gitignore {
            walker.filter_entry(move |entry| {
                // Always skip .git directories
                if entry.file_name() == ".git" {
                    return false;
                }

                let Ok(mut visited) = visited.lock() else {
                    return true;
                };
                // Skip visited files
                if visited.contains(entry.path()) {
                    return false;
                }

                visited.insert(entry.path().to_path_buf());
                true
            })
        } else {
            walker.filter_entry(move |entry| {
                let Ok(mut visited) = visited.lock() else {
                    return true;
                };
                // Skip visited files
                if visited.contains(entry.path()) {
                    return false;
                }

                visited.insert(entry.path().to_path_buf());
                true
            })
        };

        let walker = walker.build();

        // Skip bulked's own output format so search never matches the files it
        // (or a previous run) produced, unless the caller opts in with --include-bk.
        let include_bk = self.include_bk;

        Box::new(
            walker
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
                .map(|entry| entry.path().to_path_buf())
                .filter(move |path| include_bk || path.extension().is_none_or(|ext| ext != "bk")),
        )
    }
}

// Note: We don't add #[cfg(test)] tests for IgnoreWalker here because
// testing it would require creating real directories and .gitignore files,
// which violates our hermetic testing principle. IgnoreWalker is a thin
// wrapper over the ignore crate, which is well-tested. We test the Walker
// trait contract with SimpleWalker.
