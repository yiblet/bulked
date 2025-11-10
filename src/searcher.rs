//! Core search logic - functional core with no I/O dependencies
//!
//! This module provides the Searcher, which orchestrates the search operation
//! using abstract dependencies (FileSystem, Matcher, Walker traits). This
//! implements the functional core of the hexagonal architecture.

use crate::context::add_context_to_match;
use crate::filesystem::FileSystem;
use crate::matcher::{MatchInfo, Matcher};
use crate::types::{MatchResult, SearchConfig, SearchError, SearchResult};
use crate::walker::Walker;
use std::path::PathBuf;

/// Core search orchestrator
///
/// This struct is generic over the FileSystem, Matcher, and Walker traits.
/// This allows it to work with any combination of implementations (real or test).
pub struct Searcher<FS, M, W>
where
    FS: FileSystem,
    M: Matcher,
    W: Walker,
{
    fs: FS,
    matcher: M,
    walker: W,
    config: SearchConfig,
}

impl<FS, M, W> Searcher<FS, M, W>
where
    FS: FileSystem,
    M: Matcher,
    W: Walker,
{
    /// Create a new searcher with the given dependencies and configuration
    pub fn new(fs: FS, matcher: M, walker: W, config: SearchConfig) -> Self {
        Self {
            fs,
            matcher,
            walker,
            config,
        }
    }

    /// Search a single file for matches
    ///
    /// Returns Ok with matches if successful, or Err with a SearchError if the file
    /// couldn't be searched.
    fn search_file(&self, path: &PathBuf) -> Result<Vec<MatchResult>, SearchError> {
        // Check if file exists
        if !self.fs.exists(path) {
            return Err(SearchError::FileReadError {
                path: path.clone(),
                error: "File does not exist".to_string(),
            });
        }

        // Check if it's actually a file
        if !self.fs.is_file(path) {
            return Err(SearchError::FileReadError {
                path: path.clone(),
                error: "Not a file".to_string(),
            });
        }

        // Skip binary files
        if self.fs.is_binary(path) {
            tracing::debug!("Skipping binary file: {}", path.display());
            return Err(SearchError::BinaryFileSkipped(path.clone()));
        }

        let match_infos = match self
            .fs
            .as_real_path(path)
            .and_then(|path| Some(self.matcher.search_path()?(&path)))
        {
            None => {
                // Read file contents
                let content = self.fs.read_to_string(path).map_err(|e| {
                    tracing::warn!("Failed to read {}: {}", path.display(), e);
                    SearchError::FileReadError {
                        path: path.clone(),
                        error: e,
                    }
                })?;

                // Search for matches
                self.matcher.search_in_content(&content)
            }

            Some(matches) => matches.map_err(|e| {
                tracing::warn!("Search error: {}", e);
                SearchError::FileReadError {
                    path: path.clone(),
                    error: e,
                }
            })?,
        };

        // Convert to MatchResult
        let matches: Vec<MatchResult> = match_infos
            .into_iter()
            .map(|info: MatchInfo| MatchResult::from_match_info(info, path.clone()))
            .collect();

        Ok(matches)
    }

    /// Search all files and return results
    ///
    /// This is the main entry point for searching. It walks all files,
    /// searches each one, and collects results and errors.
    pub fn search_all(&self) -> SearchResult {
        let mut result = SearchResult::new();

        for path in self.walker.files() {
            match self.search_file(&path) {
                Ok(matches) => {
                    for m in matches {
                        result.add_match(m);
                    }
                }
                Err(err) => {
                    // Log errors but continue searching
                    match &err {
                        SearchError::BinaryFileSkipped(_) => {
                            // Debug level for binary files (expected)
                        }
                        SearchError::FileReadError { path, error } => {
                            tracing::warn!("Error reading {}: {}", path.display(), error);
                        }
                        SearchError::PatternError(e) => {
                            tracing::error!("Pattern error: {}", e);
                        }
                    }
                    result.add_error(err);
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use crate::matcher::grep::GrepMatcher;
    use crate::matcher::stub::StubMatcher;
    use crate::walker::simple::SimpleWalker;
    use std::path::PathBuf;

    /// Test Searcher with all test doubles (solitary unit test)
    #[test]
    fn test_searcher_with_all_test_doubles() {
        // Setup MemoryFS
        let fs = MemoryFS::new();
        let test_path = PathBuf::from("/test/foo.txt");
        fs.add_file(&test_path, "line 1\nTARGET line\nline 3")
            .unwrap();

        // Setup StubMatcher
        let mut stub_matcher = StubMatcher::new();
        stub_matcher.add_match(crate::matcher::MatchInfo {
            line_num: 2,
            byte_offset: 7,
            line_content: "TARGET line".to_string(),
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        // Setup SimpleWalker
        let walker = SimpleWalker::new(vec![test_path.clone()]);

        // Setup config
        let config = SearchConfig {
            pattern: "TARGET".to_string(),
            root_path: PathBuf::from("/test"),
            context_lines: 0,
            respect_gitignore: false,
        };

        // Create searcher with all test doubles
        let searcher = Searcher::new(fs, stub_matcher, walker, config);

        // Execute search
        let result = searcher.search_all();

        // Assertions
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.errors.len(), 0);

        let match_result = &result.matches[0];
        assert_eq!(match_result.file_path, test_path);
        assert_eq!(match_result.line_number, 2);
        assert_eq!(match_result.line_content, "TARGET line");
    }

    /// Test Searcher with real GrepMatcher and MemoryFS (sociable integration test)
    #[test]
    fn test_searcher_with_real_grep_matcher() {
        // Setup MemoryFS with multiple files
        let fs = MemoryFS::new();
        let file1 = PathBuf::from("/src/main.rs");
        let file2 = PathBuf::from("/src/lib.rs");

        fs.add_file(&file1, "fn main() {\n    println!(\"hello\");\n}\n")
            .unwrap();
        fs.add_file(&file2, "pub fn hello() {\n    println!(\"hello\");\n}\n")
            .unwrap();

        // Use real GrepMatcher
        let matcher = GrepMatcher::compile("hello").unwrap();

        // Setup walker
        let walker = SimpleWalker::new(vec![file1.clone(), file2.clone()]);

        // Setup config
        let config = SearchConfig {
            pattern: "hello".to_string(),
            root_path: PathBuf::from("/src"),
            context_lines: 0,
            respect_gitignore: false,
        };

        // Create searcher
        let searcher = Searcher::new(fs, matcher, walker, config);

        // Execute search
        let result = searcher.search_all();

        // Should find "hello" in both files
        assert_eq!(result.matches.len(), 3); // main.rs line 2, lib.rs lines 1 and 2
        assert_eq!(result.errors.len(), 0);

        // Check that matches are from both files
        let files_with_matches: std::collections::HashSet<_> =
            result.matches.iter().map(|m| &m.file_path).collect();
        assert!(files_with_matches.contains(&file1));
        assert!(files_with_matches.contains(&file2));
    }

    /// Test Searcher handles binary files correctly
    #[test]
    fn test_searcher_skips_binary_files() {
        let fs = MemoryFS::new();
        let binary_file = PathBuf::from("/test/binary.bin");
        let text_file = PathBuf::from("/test/text.txt");

        // Add binary file (contains null byte)
        fs.add_file(&binary_file, "binary\0data").unwrap();
        fs.add_file(&text_file, "text data with match").unwrap();

        let matcher = GrepMatcher::compile("match").unwrap();
        let walker = SimpleWalker::new(vec![binary_file.clone(), text_file.clone()]);

        let config = SearchConfig {
            pattern: "match".to_string(),
            root_path: PathBuf::from("/test"),
            context_lines: 0,
            respect_gitignore: false,
        };

        let searcher = Searcher::new(fs, matcher, walker, config);
        let result = searcher.search_all();

        // Should find match in text file
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].file_path, text_file);

        // Should have error for binary file
        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
            result.errors[0],
            SearchError::BinaryFileSkipped(_)
        ));
    }

    /// Test Searcher handles nonexistent files
    #[test]
    fn test_searcher_handles_nonexistent_files() {
        let fs = MemoryFS::new();
        let nonexistent = PathBuf::from("/nonexistent.txt");

        let matcher = GrepMatcher::compile("test").unwrap();
        let walker = SimpleWalker::new(vec![nonexistent.clone()]);

        let config = SearchConfig {
            pattern: "test".to_string(),
            root_path: PathBuf::from("/"),
            context_lines: 0,
            respect_gitignore: false,
        };

        let searcher = Searcher::new(fs, matcher, walker, config);
        let result = searcher.search_all();

        // Should have no matches
        assert_eq!(result.matches.len(), 0);

        // Should have error for nonexistent file
        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
            result.errors[0],
            SearchError::FileReadError { .. }
        ));
    }

    /// Test Searcher with no matches
    #[test]
    fn test_searcher_no_matches() {
        let fs = MemoryFS::new();
        let file = PathBuf::from("/test/file.txt");
        fs.add_file(&file, "no matches here").unwrap();

        let matcher = GrepMatcher::compile("nonexistent").unwrap();
        let walker = SimpleWalker::new(vec![file]);

        let config = SearchConfig {
            pattern: "nonexistent".to_string(),
            root_path: PathBuf::from("/test"),
            context_lines: 0,
            respect_gitignore: false,
        };

        let searcher = Searcher::new(fs, matcher, walker, config);
        let result = searcher.search_all();

        assert_eq!(result.matches.len(), 0);
        assert_eq!(result.errors.len(), 0);
    }

    /// Test Searcher with context extraction (Phase 2)
    #[test]
    fn test_searcher_with_context() {
        let fs = MemoryFS::new();
        let file = PathBuf::from("/test/file.txt");
        let content = "line 1\nline 2\nMATCH here\nline 4\nline 5\nline 6";
        fs.add_file(&file, content).unwrap();

        let matcher = GrepMatcher::compile("MATCH").unwrap();
        let walker = SimpleWalker::new(vec![file.clone()]);

        let config = SearchConfig {
            pattern: "MATCH".to_string(),
            root_path: PathBuf::from("/test"),
            context_lines: 2, // Request 2 lines of context
            respect_gitignore: false,
        };

        let searcher = Searcher::new(fs, matcher, walker, config);
        let result = searcher.search_all();

        assert_eq!(result.matches.len(), 1);
        let m = &result.matches[0];

        // Verify match details
        assert_eq!(m.file_path, file);
        assert_eq!(m.line_number, 3);
        assert!(m.line_content.contains("MATCH"));

        // Verify context before (lines 1-2)
        assert_eq!(m.context_before.len(), 2);
        assert_eq!(m.context_before[0].line_number, 1);
        assert_eq!(m.context_before[0].content, "line 1");
        assert_eq!(m.context_before[1].line_number, 2);
        assert_eq!(m.context_before[1].content, "line 2");

        // Verify context after (lines 4-5)
        assert_eq!(m.context_after.len(), 2);
        assert_eq!(m.context_after[0].line_number, 4);
        assert_eq!(m.context_after[0].content, "line 4");
        assert_eq!(m.context_after[1].line_number, 5);
        assert_eq!(m.context_after[1].content, "line 5");
    }
}
