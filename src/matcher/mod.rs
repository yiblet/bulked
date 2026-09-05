//! Pattern matching abstraction
//!
//! This module defines the Matcher trait which provides an abstraction over
//! regex pattern matching. This allows testing search logic with predictable
//! match results without depending on actual regex engine behavior.

use std::path::Path;
use thiserror::Error;

// Import the regex error type from grep crate
use grep::regex::Error as GrepRegexError;

pub mod regex;
#[cfg(test)]
pub mod stub;

/// Errors that can occur during pattern matching operations
#[derive(Debug, Error)]
pub enum MatcherError {
    /// Invalid regex pattern
    #[error("Invalid regex pattern '{pattern}': {source}")]
    InvalidPattern {
        pattern: String,
        #[source]
        source: GrepRegexError,
    },

    /// Search operation failed
    #[error("Search error: {source}")]
    SearchError {
        #[source]
        source: std::io::Error,
    },
}

/// Information about a single match within file content
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchInfo {
    /// Line number where the match occurred (1-indexed)
    pub line_num: usize,
    /// Byte offset within the file
    pub byte_offset: usize,
    /// Content of the line containing the match
    pub line_content: String,

    pub line_match: Option<std::ops::Range<usize>>,

    pub previous_lines: String,

    pub next_lines: String,
}

/// What a [`Matcher`] should search.
///
/// The searcher hands a [`Source::Path`] when the filesystem can expose a real
/// on-disk path (so the matcher may memory-map or stream it) and a
/// [`Source::Content`] when the file had to be read into memory first (e.g. an
/// in-memory test filesystem). Implementations must produce the same matches
/// for the same bytes regardless of which variant they receive.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    /// A real file on disk, searched in place.
    Path(&'a Path),
    /// File contents already in memory.
    Content(&'a str),
}

/// Abstract pattern matching interface
///
/// This trait provides regex matching operations. Implementations can be
/// backed by actual regex engines (`GrepMatcher`) or provide canned responses
/// for testing (`StubMatcher`).
pub trait Matcher: Send + Sync {
    /// Search `src` for matches.
    ///
    /// Returns every match with its line number, byte offset, the matched
    /// range within the line, and any configured context lines. I/O and
    /// decoding failures are returned, never swallowed.
    fn search(&self, src: Source<'_>) -> Result<Vec<MatchInfo>, MatcherError>;
}

#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;
    use crate::matcher::regex::GrepMatcher;
    use crate::matcher::stub::StubMatcher;

    #[test]
    fn test_grep_matcher_compiles_valid_pattern() {
        let matcher = GrepMatcher::compile("foo.*bar");
        assert!(matcher.is_ok(), "Should compile valid pattern");
    }

    #[test]
    fn test_grep_matcher_rejects_invalid_pattern() {
        let matcher = GrepMatcher::compile("[unclosed");
        assert!(matcher.is_err(), "Should reject invalid pattern");
    }

    #[test]
    fn test_grep_matcher_finds_matches() {
        let matcher = GrepMatcher::compile("hello").unwrap();
        let content = "line 1\nhello world\nline 3\nsay hello\n";

        let matches = matcher.search(Source::Content(content)).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_num, 2);
        assert!(matches[0].line_content.contains("hello world"));
        assert_eq!(matches[1].line_num, 4);
        assert!(matches[1].line_content.contains("say hello"));
    }

    /// The matcher owns real file I/O, so this adapter test writes a real temp
    /// file and checks that searching it by path yields exactly what searching
    /// the same bytes as in-memory content yields (line numbers and highlight
    /// ranges alike).
    #[test]
    fn test_grep_matcher_content_and_path_sources_agree() {
        let content = "a\nhello\nb\nhello world\n";
        let path = std::env::temp_dir().join(format!(
            "bulked-matcher-sources-agree-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, content).unwrap();

        let matcher = GrepMatcher::compile("hello").unwrap();
        let by_path = matcher.search(Source::Path(&path));
        let _ = std::fs::remove_file(&path);
        let by_path = by_path.unwrap();
        let by_content = matcher.search(Source::Content(content)).unwrap();

        assert_eq!(by_path.len(), 2);
        assert_eq!(by_path.len(), by_content.len());
        for (p, c) in by_path.iter().zip(by_content.iter()) {
            assert_eq!(p.line_num, c.line_num);
            assert_eq!(p.line_match, c.line_match);
            assert!(p.line_match.is_some(), "line_match must be populated");
        }
        assert_eq!(by_content[0].line_num, 2);
        assert_eq!(by_content[0].line_match, Some(0..5));
        assert_eq!(by_content[1].line_num, 4);
        assert_eq!(by_content[1].line_match, Some(0..5));
    }

    /// Searching in-memory content must populate `line_match` (previously only
    /// the on-disk path branch did).
    #[test]
    fn test_grep_matcher_content_source_sets_line_match() {
        let matcher = GrepMatcher::compile("hello").unwrap();

        let matches = matcher.search(Source::Content("say hello\n")).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_num, 1);
        assert_eq!(matches[0].line_match, Some(4..9));
    }

    #[test]
    fn test_stub_matcher_returns_predefined_matches() {
        let mut matcher = StubMatcher::new();
        matcher.add_match(MatchInfo {
            line_num: 10,
            byte_offset: 100,
            line_content: "test line\n".to_string(),
            line_match: None,
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        let matches = matcher.search(Source::Content("any content")).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_num, 10);
        assert_eq!(matches[0].line_content, "test line\n");
    }
}
