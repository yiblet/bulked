//! High-level search execution with production adapters
//!
//! This module provides a convenient API for executing searches with sensible
//! defaults and production implementations (`PhysicalFS`, `GrepMatcher`, `IgnoreWalker`).

use crate::filesystem::physical::PhysicalFS;
use crate::matcher::regex::GrepMatcher;
use crate::matcher::{Matcher, MatcherError};
use crate::searcher::Searcher;
use crate::types::{SearchError, SearchResult};
use crate::walker::ignore_walker::IgnoreWalker;
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during search execution
#[derive(Debug, Error)]
pub enum ExecuteError {
    /// Pattern compilation failed
    #[error("Pattern error: {source}")]
    PatternError {
        #[from]
        source: MatcherError,
    },

    /// Search execution failed
    #[error("Search error: {source}")]
    SearchError {
        #[from]
        source: SearchError,
    },
}

/// Configuration for executing a search with production adapters
#[derive(Debug, Clone)]
pub struct ExecuteConfig {
    /// Regex pattern to search for
    pub pattern: String,

    /// Root directory or file to search
    pub path: PathBuf,

    /// Number of context lines before and after each match
    pub context_lines: usize,

    /// Whether to respect .gitignore and .ignore files
    pub respect_gitignore: bool,

    /// Whether to include hidden files
    pub hidden: bool,
}

impl ExecuteConfig {
    /// Create a new configuration with the given pattern and path
    pub fn new(pattern: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            pattern: pattern.into(),
            path: path.into(),
            context_lines: 20,
            respect_gitignore: true,
            hidden: false,
        }
    }

    /// Set the number of context lines (default: 20)
    #[must_use]
    pub fn with_context_lines(mut self, lines: usize) -> Self {
        self.context_lines = lines;
        self
    }

    /// Set whether to respect gitignore files (default: true)
    #[must_use]
    pub fn with_respect_gitignore(mut self, respect: bool) -> Self {
        self.respect_gitignore = respect;
        self
    }

    /// Set whether to include hidden files (default: false)
    #[must_use]
    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }
}

/// Execute a search with production adapters (`PhysicalFS`, `GrepMatcher`, `IgnoreWalker`)
///
/// This is the main entry point for running a search with real filesystem access.
///
/// # Arguments
///
/// * `config` - Search configuration including pattern, path, context, etc.
///
/// # Returns
///
/// * `Ok(SearchResult)` - Results containing all matches found
/// * `Err(ExecuteError)` - Error describing what went wrong
///
/// # Errors
///
/// Returns an error if:
/// - The regex pattern is invalid (`ExecuteError::PatternError`)
/// - File read errors occur during search (`ExecuteError::SearchError`)
///
/// # Example
///
/// ```no_run
/// use bulked::{execute, ExecuteConfig};
///
/// let config = ExecuteConfig::new("EXAMPLE", ".")
///     .with_context_lines(5)
///     .with_respect_gitignore(true);
///
/// match execute(&config) {
///     Ok(result) => {
///         println!("Found {} matches", result.matches.len());
///     }
///     Err(e) => {
///         eprintln!("Error: {}", e);
///     }
/// }
/// ```
pub fn execute(config: &ExecuteConfig) -> Result<SearchResult, ExecuteError> {
    // Create production adapters
    let fs = PhysicalFS::new();

    let matcher = GrepMatcher::compile(&config.pattern)?.with_context(config.context_lines);

    let walker = IgnoreWalker::new(&config.path, config.respect_gitignore, config.hidden);

    // Execute search
    let searcher = Searcher::new(fs, matcher, walker);

    Ok(searcher.search_all()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use crate::walker::simple::SimpleWalker;

    #[test]
    fn test_execute_config_builder() {
        let config = ExecuteConfig::new("test", "/path")
            .with_context_lines(10)
            .with_respect_gitignore(false);

        assert_eq!(config.pattern, "test");
        assert_eq!(config.path, PathBuf::from("/path"));
        assert_eq!(config.context_lines, 10);
        assert!(!config.respect_gitignore);
    }

    #[test]
    fn test_execute_config_defaults() {
        let config = ExecuteConfig::new("pattern", "/some/path");

        assert_eq!(config.context_lines, 20);
        assert!(config.respect_gitignore);
    }

    #[test]
    fn test_execute_with_memory_fs() {
        // Setup MemoryFS with test file
        let fs = MemoryFS::new();
        let test_file = PathBuf::from("/test/file.txt");
        fs.add_file(&test_file, "line 1\nTARGET\nline 3").unwrap();

        // Setup matcher and walker
        let matcher = GrepMatcher::compile("TARGET").unwrap().with_context(1);
        let walker = SimpleWalker::new(vec![test_file.clone()]);

        // Execute search using Searcher directly
        let searcher = Searcher::new(fs, matcher, walker);
        let result = searcher.search_all().unwrap();

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].line_number, 2);
        assert_eq!(result.matches[0].line_content, "TARGET\n");
    }

    #[test]
    fn test_execute_with_invalid_pattern() {
        let config = ExecuteConfig::new("[invalid", ".");
        let result = execute(&config);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ExecuteError::PatternError { .. }));
        assert!(err.to_string().contains("Invalid regex pattern"));
    }
}
