//! Stub matcher implementation for testing
//!
//! This module provides `StubMatcher`, a test double that returns predefined
//! match results. This allows testing search logic without depending on
//! regex engine behavior.

use super::{MatchInfo, Matcher, MatcherError, Source};

/// Stub matcher for testing
///
/// This is a test double that returns predefined matches regardless of what
/// [`Source`] it is asked to search. It allows complete control over match
/// behavior in tests.
pub(crate) struct StubMatcher {
    matches: Vec<MatchInfo>,
}

impl StubMatcher {
    /// Create a new stub matcher with no predefined matches
    pub fn new() -> Self {
        Self {
            matches: Vec::new(),
        }
    }

    /// Add a predefined match that will be returned by `search`
    pub fn add_match(&mut self, match_info: MatchInfo) {
        self.matches.push(match_info);
    }
}

impl Default for StubMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Matcher for StubMatcher {
    fn search(&self, _src: Source<'_>) -> Result<Vec<MatchInfo>, MatcherError> {
        // Return predefined matches, ignoring the source entirely
        Ok(self.matches.clone())
    }
}

#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_stub_matcher_new_has_no_matches() {
        let matcher = StubMatcher::new();
        let matches = matcher.search(Source::Content("any content")).unwrap();
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_stub_matcher_add_match() {
        let mut matcher = StubMatcher::new();
        matcher.add_match(MatchInfo {
            line_num: 5,
            byte_offset: 42,
            line_content: "test line\n".to_string(),
            line_match: None,
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        let matches = matcher.search(Source::Content("ignored")).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_num, 5);
        assert_eq!(matches[0].byte_offset, 42);
        assert_eq!(matches[0].line_content, "test line\n");
    }

    #[test]
    fn test_stub_matcher_multiple_matches() {
        let mut matcher = StubMatcher::new();
        matcher.add_match(MatchInfo {
            line_num: 1,
            byte_offset: 0,
            line_content: "first\n".to_string(),
            line_match: None,
            previous_lines: String::new(),
            next_lines: String::new(),
        });
        matcher.add_match(MatchInfo {
            line_num: 2,
            byte_offset: 10,
            line_content: "second\n".to_string(),
            line_match: None,
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        let matches = matcher.search(Source::Content("ignored")).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_content, "first\n");
        assert_eq!(matches[1].line_content, "second\n");
    }

    /// The stub ignores the source kind: a path that does not exist yields the
    /// same canned matches as in-memory content.
    #[test]
    fn test_stub_matcher_ignores_source_kind() {
        let mut matcher = StubMatcher::new();
        matcher.add_match(MatchInfo {
            line_num: 3,
            byte_offset: 0,
            line_content: "canned\n".to_string(),
            line_match: None,
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        let by_path = matcher
            .search(Source::Path(Path::new("/definitely/not/here")))
            .unwrap();
        let by_content = matcher.search(Source::Content("")).unwrap();

        assert_eq!(by_path, by_content);
        assert_eq!(by_path.len(), 1);
    }
}
