//! Bulked - Recursive grep with context
//!
//! This library provides a simple API for searching files with regex patterns
//! and extracting surrounding context lines.
//!
//! # Example
//!
//! ```no_run
//! use bulked::{execute, ExecuteConfig};
//!
//! let config = ExecuteConfig::new("TODO", ".")
//!     .with_context_lines(5)
//!     .with_respect_gitignore(true);
//!
//! match execute(&config) {
//!     Ok(result) => {
//!         for m in &result.matches {
//!             println!("{}:{} - {}", m.file_path.display(), m.line_number, m.line_content);
//!         }
//!     }
//!     Err(e) => eprintln!("Error: {}", e),
//! }
//! ```

// Internal modules
mod execute;
mod filesystem;
mod format;
mod matcher;
mod searcher;
mod types;
mod walker;
mod apply;

// Re-export main API
pub use apply::{apply_format, ApplyError};
pub use execute::{ExecuteConfig, execute};
pub use format::{Chunk, Format, FormatError};
pub use types::{ContextLine, MatchResult, SearchResult};

#[cfg(test)]
mod integration_tests {
    use crate::filesystem::memory::MemoryFS;
    use crate::matcher::Matcher; // Import the trait
    use crate::matcher::grep::GrepMatcher;
    use crate::searcher::Searcher;
    use crate::walker::simple::SimpleWalker;
    use std::path::PathBuf;

    /// Full-stack integration test with `MemoryFS`
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

        let searcher = Searcher::new(fs, matcher, walker);
        let result = searcher.search_all().unwrap();

        // Should find "fn " in both .rs files
        assert!(result.matches.len() >= 2, "Should find at least 2 matches");

        // Verify matches are from .rs files
        for m in &result.matches {
            assert!(
                m.file_path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rs")),
                "Match should be from .rs file, got: {}",
                m.file_path.display()
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
    /// Uses virtual filesystem (`MemoryFS`) for hermetic testing.
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
                file1_lines.push(format!("line {i}"));
            }
        }
        fs.add_file(
            &PathBuf::from("/project/file1.txt"),
            &file1_lines.join("\n"),
        )
        .unwrap();

        // Create another file with match near boundaries
        let file2_lines = ["line 1", "line 2", "TARGET at line 3", "line 4"];
        fs.add_file(
            &PathBuf::from("/project/file2.txt"),
            &file2_lines.join("\n"),
        )
        .unwrap();

        // Create a file that should be ignored
        fs.add_file(&PathBuf::from("/project/ignored.log"), "TARGET ignored")
            .unwrap();

        // Use real GrepMatcher with context
        let matcher = GrepMatcher::compile("TARGET").unwrap().with_context(20);

        // Use SimpleWalker (simulating gitignore filtering)
        let walker = SimpleWalker::new(vec![
            PathBuf::from("/project/file1.txt"),
            PathBuf::from("/project/file2.txt"),
            // NOT including ignored.log
        ]);

        let searcher = Searcher::new(fs, matcher, walker);
        let result = searcher.search_all().unwrap();

        // Verify correct number of matches (2 matches in non-ignored files)
        assert_eq!(
            result.matches.len(),
            2,
            "Should find exactly 2 matches in non-ignored files"
        );

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
