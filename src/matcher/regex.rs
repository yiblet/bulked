//! Production pattern matcher using grep-regex
//!
//! This module provides `GrepMatcher`, which uses the grep-regex and grep-searcher
//! crates to perform fast regex matching. This is the production implementation
//! based on the same infrastructure used by ripgrep and Helix.

use std::path::Path;

use super::{MatchInfo, Matcher, MatcherError};
use grep::matcher::Matcher as GrepMatcherTrait;
use grep::regex::RegexMatcher as GrepRegexMatcher;
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder};

/// Production matcher using grep-regex
#[derive(Debug)]
pub struct GrepMatcher {
    matcher: GrepRegexMatcher,
    context: usize,
}

mod sink {
    use std::io;

    use grep::searcher::{Searcher, Sink, SinkError, SinkMatch};

    use crate::matcher::MatchInfo;

    #[derive(Debug)]
    pub struct UTF8<'a>(&'a mut Vec<MatchInfo>, String);

    impl<'a> UTF8<'a> {
        pub fn new(matches: &'a mut Vec<MatchInfo>) -> Self {
            Self(matches, String::new())
        }
    }

    impl Sink for UTF8<'_> {
        type Error = io::Error;

        fn matched(
            &mut self,
            _searcher: &Searcher,
            mat: &SinkMatch<'_>,
        ) -> Result<bool, io::Error> {
            let matched = match std::str::from_utf8(mat.bytes()) {
                Ok(matched) => matched,
                Err(err) => return Err(io::Error::error_message(err)),
            };
            let Some(line_number) = mat.line_number() else {
                let msg = "line numbers not enabled";
                return Err(io::Error::error_message(msg));
            };

            let byte_offset = mat.absolute_byte_offset();

            let prev = std::mem::take(&mut self.1);
            #[allow(clippy::cast_possible_truncation)] // Line numbers in practice fit in usize
            self.0.push(MatchInfo {
                line_num: line_number as usize,
                byte_offset: byte_offset as usize,
                line_content: matched.trim_end().to_string(),
                previous_lines: prev,
                next_lines: String::new(),
            });
            Ok(true)
        }

        fn context(
            &mut self,
            _searcher: &Searcher,
            mat: &grep::searcher::SinkContext<'_>,
        ) -> Result<bool, Self::Error> {
            let matched = match std::str::from_utf8(mat.bytes()) {
                Ok(matched) => matched,
                Err(err) => return Err(io::Error::error_message(err)),
            };

            match mat.kind() {
                grep::searcher::SinkContextKind::Before => {
                    self.1.push_str(matched);
                }

                grep::searcher::SinkContextKind::After => {
                    if let Some(last) = self.0.last_mut() {
                        last.next_lines.push_str(matched);
                    }
                }

                grep::searcher::SinkContextKind::Other => {}
            }

            Ok(true)
        }
    }
}

impl GrepMatcher {
    pub fn with_context(self, context: usize) -> Self {
        Self {
            matcher: self.matcher.clone(),
            context,
        }
    }

    fn build_searcher(&self) -> Searcher {
        // Create a searcher with binary detection
        // BinaryDetection::quit(b'\x00') makes grep stop searching immediately
        // when it encounters a null byte, which is a reliable indicator of binary content.
        // This matches the behavior of ripgrep and other grep tools.
        let mut searcher = SearcherBuilder::new();

        searcher
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .line_number(true);

        if self.context > 0 {
            searcher.before_context(self.context);
            searcher.after_context(self.context);
        }

        searcher.build()
    }
}

impl Matcher for GrepMatcher {
    fn compile(pattern: &str) -> Result<Self, MatcherError>
    where
        Self: Sized,
    {
        let matcher = GrepRegexMatcher::new(pattern).map_err(|source| {
            MatcherError::InvalidPattern {
                pattern: pattern.to_string(),
                source,
            }
        })?;

        Ok(Self {
            matcher,
            context: 0,
        })
    }

    fn search_in_content(&self, content: &str) -> Vec<MatchInfo> {
        let mut matches = Vec::new();

        let mut searcher = self.build_searcher();
        // Use UTF8 sink to collect matches
        let result = searcher.search_slice(
            &self.matcher,
            content.as_bytes(),
            sink::UTF8::new(&mut matches),
        );

        // Log any errors but don't fail
        if let Err(e) = result {
            tracing::warn!("Search error: {}", e);
        }

        matches
    }

    fn is_match(&self, text: &str) -> bool {
        self.matcher.is_match(text.as_bytes()).unwrap_or(false)
    }

    fn search_path(&self) -> Option<impl FnMut(&Path) -> Result<Vec<MatchInfo>, MatcherError>> {
        Some(move |path: &Path| {
            let mut matches = Vec::new();
            let mut searcher = self.build_searcher();
            // Use UTF8 sink to collect matches
            searcher
                .search_path(&self.matcher, path, sink::UTF8::new(&mut matches))
                .map_err(|source| MatcherError::SearchError { source })?;

            Ok(matches)
        })
    }
}

#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;

    #[test]
    fn test_grep_matcher_simple_pattern() {
        let matcher = GrepMatcher::compile("test").unwrap();

        assert!(matcher.is_match("this is a test"));
        assert!(matcher.is_match("test"));
        assert!(!matcher.is_match("no match here"));
    }

    #[test]
    fn test_grep_matcher_regex_pattern() {
        let matcher = GrepMatcher::compile("fo+bar").unwrap();

        // "fo+bar" means "f" followed by one or more "o" followed by "bar"
        assert!(matcher.is_match("foobar")); // Two o's
        assert!(matcher.is_match("fooooobar")); // Many o's
        assert!(matcher.is_match("fobar")); // One o (minimum required by +)
        assert!(!matcher.is_match("fbar")); // No o, should not match
        assert!(!matcher.is_match("f bar")); // Space instead of o
    }

    #[test]
    fn test_grep_matcher_search_multiline() {
        let matcher = GrepMatcher::compile("match").unwrap();
        let content = "line 1\nthis is a match\nline 3\nanother match here\nline 5";

        let matches = matcher.search_in_content(content);

        assert_eq!(matches.len(), 2);

        assert_eq!(matches[0].line_num, 2);
        assert_eq!(matches[0].line_content, "this is a match");

        assert_eq!(matches[1].line_num, 4);
        assert_eq!(matches[1].line_content, "another match here");
    }

    #[test]
    fn test_grep_matcher_no_matches() {
        let matcher = GrepMatcher::compile("notfound").unwrap();
        let content = "line 1\nline 2\nline 3";

        let matches = matcher.search_in_content(content);

        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_grep_matcher_case_sensitive() {
        let matcher = GrepMatcher::compile("Test").unwrap();

        assert!(matcher.is_match("Test"));
        assert!(!matcher.is_match("test")); // Case sensitive by default
    }

    #[test]
    fn test_grep_matcher_invalid_regex() {
        let result = GrepMatcher::compile("[unclosed");

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, MatcherError::InvalidPattern { .. }));
        assert!(err.to_string().contains("Invalid regex pattern"));
    }

    #[test]
    fn test_grep_matcher_with_context() {
        let matcher = GrepMatcher::compile("MATCH").unwrap().with_context(3);

        // Create content with a match on line 5
        let content =
            "line 1\nline 2\nline 3\nline 4\nMATCH line 5\nline 6\nline 7\nline 8\nline 9";

        let matches = matcher.search_in_content(content);

        assert_eq!(matches.len(), 1, "Should find exactly one match");

        let m = &matches[0];
        assert_eq!(m.line_num, 5, "Match should be on line 5");
        assert_eq!(m.line_content, "MATCH line 5");

        // Check context before (lines 2, 3, 4)
        let before_lines: Vec<&str> = m.previous_lines.lines().collect();
        println!("Context before: {before_lines:?}");
        println!("previous_lines raw: {:?}", m.previous_lines);
        assert_eq!(
            before_lines.len(),
            3,
            "Should have 3 lines of context before"
        );
        assert_eq!(before_lines[0], "line 2");
        assert_eq!(before_lines[1], "line 3");
        assert_eq!(before_lines[2], "line 4");

        // Check context after (lines 6, 7, 8)
        let after_lines: Vec<&str> = m.next_lines.lines().collect();
        println!("Context after: {after_lines:?}");
        println!("next_lines raw: {:?}", m.next_lines);
        assert_eq!(after_lines.len(), 3, "Should have 3 lines of context after");
        assert_eq!(after_lines[0], "line 6");
        assert_eq!(after_lines[1], "line 7");
        assert_eq!(after_lines[2], "line 8");
    }
}
