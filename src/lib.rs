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

    /// SUCCESS CRITERIA TEST - Phase 3
    ///
    /// This is the integration test specified in the plan that verifies:
    /// - Matches are found in correct files
    /// - Each match includes exactly 20 lines before and 20 lines after (or up to file boundaries)
    /// - Line numbers are accurate
    /// - Gitignored files are excluded by default
    ///
    /// Uses virtual filesystem (MemoryFS) for hermetic testing.
    #[test]
    fn test_bulked_search_with_context() {
        // Create test directory with realistic structure
        let fs = MemoryFS::new();

        // Create .gitignore
        fs.add_file(&PathBuf::from("/project/.gitignore"), "*.log\ntemp/\n")
            .unwrap();

        // Create a file with enough lines for context testing (50 lines)
        let mut file1_lines = vec![];
        for i in 1..=50 {
            if i == 25 {
                file1_lines.push("TARGET match on line 25".to_string());
            } else {
                file1_lines.push(format!("line {}", i));
            }
        }
        fs.add_file(
            &PathBuf::from("/project/file1.txt"),
            &file1_lines.join("\n"),
        )
        .unwrap();

        // Create another file with match near boundaries
        let file2_lines = vec!["line 1", "line 2", "TARGET at line 3", "line 4"];
        fs.add_file(
            &PathBuf::from("/project/file2.txt"),
            &file2_lines.join("\n"),
        )
        .unwrap();

        // Create a file that should be ignored
        fs.add_file(&PathBuf::from("/project/ignored.log"), "TARGET ignored")
            .unwrap();

        // Use real GrepMatcher
        let matcher = GrepMatcher::compile("TARGET").unwrap();

        // Use SimpleWalker (simulating gitignore filtering)
        let walker = SimpleWalker::new(vec![
            PathBuf::from("/project/file1.txt"),
            PathBuf::from("/project/file2.txt"),
            // NOT including ignored.log
        ]);

        let config = SearchConfig {
            pattern: "TARGET".to_string(),
            root_path: PathBuf::from("/project"),
            context_lines: 20,
            respect_gitignore: true,
        };

        let searcher = Searcher::new(fs, matcher, walker, config);
        let result = searcher.search_all();

        // Verify correct number of matches (2 matches in non-ignored files)
        assert_eq!(
            result.matches.len(),
            2,
            "Should find exactly 2 matches in non-ignored files"
        );
        assert_eq!(result.errors.len(), 0, "Should have no errors");

        // Verify first match (file1.txt, line 25 with full context)
        let match1 = result
            .matches
            .iter()
            .find(|m| m.file_path.to_str().unwrap().contains("file1.txt"))
            .expect("Should find match in file1.txt");

        assert_eq!(match1.line_number, 25);
        assert!(match1.line_content.contains("TARGET"));

        // Verify context before (should be exactly 20 lines: lines 5-24)
        assert_eq!(
            match1.context_before.len(),
            20,
            "Should have exactly 20 lines of context before"
        );
        assert_eq!(match1.context_before[0].line_number, 5);
        assert_eq!(match1.context_before[19].line_number, 24);

        // Verify context after (should be exactly 20 lines: lines 26-45)
        assert_eq!(
            match1.context_after.len(),
            20,
            "Should have exactly 20 lines of context after"
        );
        assert_eq!(match1.context_after[0].line_number, 26);
        assert_eq!(match1.context_after[19].line_number, 45);

        // Verify second match (file2.txt, line 3 near start - limited context)
        let match2 = result
            .matches
            .iter()
            .find(|m| m.file_path.to_str().unwrap().contains("file2.txt"))
            .expect("Should find match in file2.txt");

        assert_eq!(match2.line_number, 3);
        assert!(match2.line_content.contains("TARGET"));

        // Verify context before (only 2 lines available: lines 1-2)
        assert_eq!(
            match2.context_before.len(),
            2,
            "Should have only 2 lines before (file boundary)"
        );
        assert_eq!(match2.context_before[0].line_number, 1);
        assert_eq!(match2.context_before[1].line_number, 2);

        // Verify context after (only 1 line available: line 4)
        assert_eq!(
            match2.context_after.len(),
            1,
            "Should have only 1 line after (file boundary)"
        );
        assert_eq!(match2.context_after[0].line_number, 4);

        // Verify gitignored file was NOT searched
        assert!(
            !result
                .matches
                .iter()
                .any(|m| m.file_path.to_str().unwrap().contains("ignored.log")),
            "Should not find matches in gitignored files"
        );
    }
}
