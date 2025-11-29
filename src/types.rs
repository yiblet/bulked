//! Core domain types - no I/O dependencies
//!
//! These types represent the pure data structures used throughout bulked.
//! They have no dependencies on filesystem, network, or other I/O.

use std::path::PathBuf;
use thiserror::Error;

use crate::filesystem::FilesystemError;
use crate::matcher::{MatchInfo, MatcherError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestInput {
    pub file_path: PathBuf,
    pub line_number: usize,
    // TODO: add support for context messages
    // pub message: String,
}

/// A single match result from searching a file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    /// Path to the file containing the match
    pub file_path: PathBuf,
    /// Line number (1-indexed) where the match occurred
    pub line_number: usize,
    /// Content of the line containing the match
    pub line_content: String,

    /// Line match range (if any)
    /// the range is relative to the start of the line.
    pub line_match: Option<std::ops::Range<usize>>,

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
            line_match: match_info.line_match,
            line_content: match_info.line_content,
            byte_offset: match_info.byte_offset,
            context_before: {
                let lines: Vec<&str> = match_info.previous_lines.split_inclusive('\n').collect();
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
                    .split_inclusive('\n')
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
    #[cfg(test)]
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
            line_match: None,
            byte_offset: 100,
            context_before: vec![],
            context_after: vec![],
        };
        result.add_match(match_result.clone());
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0], match_result);
    }
}
