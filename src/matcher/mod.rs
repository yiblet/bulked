//! Pattern matching abstraction
//!
//! This module defines the Matcher trait which provides an abstraction over
//! regex pattern matching. This allows testing search logic with predictable
//! match results without depending on actual regex engine behavior.

use std::path::Path;

pub mod grep;
pub mod stub;

/// Information about a single match within file content
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchInfo {
    /// Line number where the match occurred (1-indexed)
    pub line_num: usize,
    /// Byte offset within the file
    pub byte_offset: usize,
    /// Content of the line containing the match
    pub line_content: String,

    pub previous_lines: String,

    pub next_lines: String,
}

/// Abstract pattern matching interface
///
/// This trait provides regex matching operations. Implementations can be
/// backed by actual regex engines (GrepMatcher) or provide canned responses
/// for testing (StubMatcher).
pub trait Matcher: Send + Sync {
    /// Compile a pattern into a matcher
    ///
    /// Returns an error if the pattern is invalid.
    fn compile(pattern: &str) -> Result<Self, String>
    where
        Self: Sized;

    /// Search for matches in file content
    ///
    /// Returns all matches found in the content, with line numbers and positions.
    fn search_in_content(&self, content: &str) -> Vec<MatchInfo>;

    /// Check if a single line matches the pattern
    ///
    /// This is a helper method for simpler matching scenarios.
    fn is_match(&self, text: &str) -> bool;

    /// Search for matches in file content
    ///
    /// Returns all matches found in the content, with line numbers and positions.
    fn search_path(&self) -> Option<impl FnMut(&Path) -> Result<Vec<MatchInfo>, String>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::grep::GrepMatcher;
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

        let matches = matcher.search_in_content(content);

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_num, 2);
        assert!(matches[0].line_content.contains("hello world"));
        assert_eq!(matches[1].line_num, 4);
        assert!(matches[1].line_content.contains("say hello"));
    }

    #[test]
    fn test_stub_matcher_returns_predefined_matches() {
        let mut matcher = StubMatcher::new();
        matcher.add_match(MatchInfo {
            line_num: 10,
            byte_offset: 100,
            line_content: "test line".to_string(),
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        let matches = matcher.search_in_content("any content");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_num, 10);
        assert_eq!(matches[0].line_content, "test line");
    }
}
