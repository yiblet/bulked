//! Core domain types - no I/O dependencies
//!
//! These types represent the pure data structures used throughout bulked.
//! They have no dependencies on filesystem, network, or other I/O.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use thiserror::Error;

use crate::filesystem::FilesystemError;
use crate::matcher::{MatchInfo, MatcherError};

/// A `(path, line)` location to ingest, as produced by another tool.
///
/// Deserializes directly from the `{"path": ..., "line": ...}` records that the
/// `ingest` subcommand accepts (with the common aliases other tools use), so no
/// intermediate wire type is needed. Line numbers are 1-based, so zero is
/// unrepresentable and rejected at decode time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct IngestInput {
    #[serde(
        rename = "path",
        alias = "file",
        alias = "file_path",
        alias = "filepath",
        alias = "filename"
    )]
    pub file_path: PathBuf,
    #[serde(
        rename = "line",
        alias = "line_number",
        alias = "lineno",
        alias = "linenum",
        alias = "ln"
    )]
    pub line_number: NonZeroUsize,
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
    /// Content of the line containing the match, including its trailing `'\n'`
    /// if the file had one
    pub line_content: String,

    /// Line match range (if any)
    /// the range is relative to the start of the line.
    pub line_match: Option<std::ops::Range<usize>>,

    /// Byte offset of the match within the file
    pub byte_offset: usize,
    /// Zero or more `'\n'`-terminated lines immediately preceding the match
    /// line, in file order. Empty when there is no context before.
    pub context_before: String,
    /// Zero or more lines immediately following the match line, in file order.
    /// Every line but the last is `'\n'`-terminated; the last is too unless it
    /// is the file's final line and the file has no trailing newline.
    pub context_after: String,
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
            context_before: match_info.previous_lines,
            context_after: match_info.next_lines,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_match_info_copies_context_strings_through() {
        let info = MatchInfo {
            line_num: 10,
            byte_offset: 42,
            line_match: Some(0..5),
            line_content: "MATCH\n".to_string(),
            previous_lines: "l8\nl9\n".to_string(),
            next_lines: "l11\n".to_string(),
        };

        let result = MatchResult::from_match_info(info, PathBuf::from("/test/file.txt"));

        assert_eq!(result.file_path, PathBuf::from("/test/file.txt"));
        assert_eq!(result.line_number, 10);
        assert_eq!(result.byte_offset, 42);
        assert_eq!(result.line_match, Some(0..5));
        assert_eq!(result.line_content, "MATCH\n");
        assert_eq!(result.context_before, "l8\nl9\n");
        assert_eq!(result.context_after, "l11\n");
    }

    #[test]
    fn test_ingest_input_deserializes_from_path_and_line() {
        let input: IngestInput = serde_json::from_str(r#"{"path":"src/a.rs","line":12}"#).unwrap();
        assert_eq!(
            input,
            IngestInput {
                file_path: PathBuf::from("src/a.rs"),
                line_number: NonZeroUsize::new(12).unwrap(),
            }
        );
        // Zero is not a line number.
        assert!(serde_json::from_str::<IngestInput>(r#"{"path":"src/a.rs","line":0}"#).is_err());
    }
}
