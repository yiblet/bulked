//! Core domain types - no I/O dependencies
//!
//! These types represent the pure data structures used throughout bulked.
//! They have no dependencies on filesystem, network, or other I/O.

use std::path::PathBuf;
use thiserror::Error;

use crate::filesystem::FilesystemError;
use crate::matcher::{MatchInfo, MatcherError};

/// A single match result from searching a file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    /// Path to the file containing the match
    pub file_path: PathBuf,
    /// Line number (1-indexed) where the match occurred
    pub line_number: usize,
    /// Content of the line containing the match
    pub line_content: String,
    /// Byte offset of the match within the file
    pub byte_offset: usize,
    /// Context lines before the match (added in Phase 2)
    pub context_before: Vec<ContextLine>,
    /// Context lines after the match (added in Phase 2)
    pub context_after: Vec<ContextLine>,
}

impl MatchResult {
    #[must_use]
    pub fn from_match_info(match_info: MatchInfo, path: PathBuf) -> Self {
        Self {
            file_path: path,
            line_number: match_info.line_num,
            line_content: match_info.line_content,
            byte_offset: match_info.byte_offset,
            context_before: {
                let lines: Vec<&str> = match_info.previous_lines.lines().collect();
                let count = lines.len();
                lines
                    .into_iter()
                    .enumerate()
                    .map(|(idx, line)| ContextLine {
                        line_number: match_info.line_num - count + idx,
                        content: line.to_string(),
                    })
                    .collect()
            },
            context_after: {
                match_info
                    .next_lines
                    .lines()
                    .enumerate()
                    .map(|(idx, line)| ContextLine {
                        line_number: match_info.line_num + idx + 1,
                        content: line.to_string(),
                    })
                    .collect()
            },
        }
    }
}

/// A line of context around a match
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLine {
    /// Line number (1-indexed)
    pub line_number: usize,
    /// Content of the line
    pub content: String,
}

/// Errors that can occur during searching
#[derive(Debug, Error)]
pub enum SearchError {
    /// Failed to read a file
    #[error("Failed to read file: {source}")]
    FileReadError {
        #[from]
        source: FilesystemError,
    },

    /// Pattern matching failed
    #[error("Pattern matching error: {source}")]
    MatcherError {
        #[from]
        source: MatcherError,
    },

    /// Multiple errors occurred during search
    #[error("{} errors occurred during search", .0.len())]
    Multiple(Vec<SearchError>),
}

impl SearchError {
    /// Create a multiple error from a vector of errors
    pub fn from_errors(errors: Vec<SearchError>) -> Self {
        match errors.len() {
            0 => panic!("Cannot create SearchError::Multiple from empty vector"),
            1 => errors.into_iter().next().unwrap(),
            _ => SearchError::Multiple(errors),
        }
    }
}

/// Result of a search operation
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// All matches found
    pub matches: Vec<MatchResult>,
}

impl SearchResult {
    /// Create a new empty search result
    #[must_use]
    pub fn new() -> Self {
        Self {
            matches: Vec::new(),
        }
    }

    /// Add a match to the result
    pub fn add_match(&mut self, match_result: MatchResult) {
        self.matches.push(match_result);
    }
}

impl Default for SearchResult {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;

    #[test]
    fn test_search_result_new() {
        let result = SearchResult::new();
        assert!(result.matches.is_empty());
    }

    #[test]
    fn test_search_result_add_match() {
        let mut result = SearchResult::new();
        let match_result = MatchResult {
            file_path: PathBuf::from("/test/file.txt"),
            line_number: 42,
            line_content: "test line".to_string(),
            byte_offset: 100,
            context_before: vec![],
            context_after: vec![],
        };
        result.add_match(match_result.clone());
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0], match_result);
    }

    #[test]
    fn test_search_error_from_errors() {
        use crate::filesystem::FilesystemError;

        let error1 = SearchError::FileReadError {
            source: FilesystemError::FileNotFound {
                path: PathBuf::from("/test1"),
            },
        };
        // Single error should unwrap
        let single = SearchError::from_errors(vec![error1]);
        assert!(matches!(single, SearchError::FileReadError { .. }));

        // Multiple errors should wrap
        let error2 = SearchError::FileReadError {
            source: FilesystemError::FileNotFound {
                path: PathBuf::from("/test2"),
            },
        };
        let error3 = SearchError::FileReadError {
            source: FilesystemError::FileNotFound {
                path: PathBuf::from("/test3"),
            },
        };
        let multiple = SearchError::from_errors(vec![error2, error3]);
        match multiple {
            SearchError::Multiple(errors) => {
                assert_eq!(errors.len(), 2);
            }
            _ => panic!("Expected Multiple variant"),
        }
    }
}
