//! Bulked - Recursive grep with context
//!
//! This library provides a grep-like search tool that shows context lines
//! around each match. It's built using hexagonal architecture with dependency
//! injection for testability.
//!
//! # Architecture
//!
//! The codebase follows hexagonal architecture (Ports and Adapters):
//!
//! - **Ports** (abstract interfaces): `FileSystem`, `Matcher`, `Walker` traits
//! - **Adapters** (concrete implementations):
//!   - Production: `PhysicalFS`, `GrepMatcher`, `IgnoreWalker`
//!   - Testing: `MemoryFS`, `StubMatcher`, `SimpleWalker`
//! - **Functional Core**: `Searcher` depends only on trait abstractions
//!
//! # Example
//!
//! ```rust,no_run
//! use bulked::filesystem::physical::PhysicalFS;
//! use bulked::matcher::grep::GrepMatcher;
//! use bulked::matcher::Matcher; // Import the trait
//! use bulked::walker::ignore_walker::IgnoreWalker;
//! use bulked::searcher::Searcher;
//! use bulked::types::SearchConfig;
//! use std::path::PathBuf;
//!
//! // Create production adapters
//! let fs = PhysicalFS::new();
//! let matcher = GrepMatcher::compile("pattern").unwrap();
//! let walker = IgnoreWalker::new(".", true);
//!
//! // Configure search
//! let config = SearchConfig {
//!     pattern: "pattern".to_string(),
//!     root_path: PathBuf::from("."),
//!     context_lines: 20,
//!     respect_gitignore: true,
//! };
//!
//! // Run search
//! let searcher = Searcher::new(fs, matcher, walker, config);
//! let result = searcher.search_all();
//!
//! println!("Found {} matches", result.matches.len());
//! ```

pub mod context;
pub mod filesystem;
pub mod matcher;
pub mod searcher;
pub mod types;
pub mod walker;

// Re-export commonly used types
pub use types::{MatchResult, SearchConfig, SearchError, SearchResult};

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use crate::matcher::grep::GrepMatcher;
    use crate::matcher::Matcher; // Import the trait
    use crate::searcher::Searcher;
    use crate::walker::simple::SimpleWalker;
    use std::path::PathBuf;

    /// Full-stack integration test with MemoryFS
    ///
    /// This tests the entire search pipeline with real implementations
    /// (except filesystem) to verify they work together correctly.
    #[test]
    fn test_full_stack_integration() {
        // Create a realistic directory structure in memory
        let fs = MemoryFS::new();

        fs.add_file(&PathBuf::from("/project/.gitignore"), "*.tmp\ntarget/\n")
            .unwrap();
        fs.add_file(
            &PathBuf::from("/project/src/main.rs"),
            "fn main() {\n    println!(\"Hello\");\n}\n",
        )
        .unwrap();
        fs.add_file(
            &PathBuf::from("/project/src/lib.rs"),
            "pub fn greet() {\n    println!(\"Hello\");\n}\n",
        )
        .unwrap();
        fs.add_file(&PathBuf::from("/project/test.tmp"), "temporary\n")
            .unwrap();

        // Use real GrepMatcher
        let matcher = GrepMatcher::compile("fn ").unwrap();

        // Use SimpleWalker with files (in production, IgnoreWalker would handle filtering)
        let walker = SimpleWalker::new(vec![
            PathBuf::from("/project/src/main.rs"),
            PathBuf::from("/project/src/lib.rs"),
            // Intentionally not including test.tmp (simulating .gitignore)
        ]);

        let config = SearchConfig {
            pattern: "fn ".to_string(),
            root_path: PathBuf::from("/project"),
            context_lines: 0,
            respect_gitignore: true,
        };

        let searcher = Searcher::new(fs, matcher, walker, config);
        let result = searcher.search_all();

        // Should find "fn " in both .rs files
        assert!(result.matches.len() >= 2, "Should find at least 2 matches");
        assert_eq!(result.errors.len(), 0, "Should have no errors");

        // Verify matches are from .rs files
        for m in &result.matches {
            let path_str = m.file_path.to_str().unwrap();
            assert!(
                path_str.ends_with(".rs"),
                "Match should be from .rs file, got: {}",
                path_str
            );
        }
    }
}
