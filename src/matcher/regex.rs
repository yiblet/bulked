//! Production pattern matcher using grep-regex
//!
//! This module provides `GrepMatcher`, which uses the grep-regex and grep-searcher
//! crates to perform fast regex matching. This is the production implementation
//! based on the same infrastructure used by ripgrep and Helix.

use super::{MatchInfo, Matcher, MatcherError, Source};
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
                line_content: matched.to_string(),
                line_match: None,
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
    /// Compile `pattern` into a matcher with no context lines.
    ///
    /// Returns [`MatcherError::InvalidPattern`] if the regex does not compile.
    pub fn compile(pattern: &str) -> Result<Self, MatcherError> {
        let matcher =
            GrepRegexMatcher::new(pattern).map_err(|source| MatcherError::InvalidPattern {
                pattern: pattern.to_string(),
                source,
            })?;

        Ok(Self {
            matcher,
            context: 0,
        })
    }

    /// Set the number of context lines captured before and after each match.
    #[must_use]
    pub fn with_context(self, context: usize) -> Self {
        Self {
            matcher: self.matcher,
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

    /// Fill in `line_match` for every collected match by re-running the regex
    /// on the matched line. The grep sink only reports whole lines, so this
    /// post-pass is what gives callers the highlight range within the line.
    fn annotate_line_matches(&self, matches: &mut [MatchInfo]) {
        for cur_match in matches {
            let Ok(Some(m)) = self.matcher.find_at(cur_match.line_content.as_bytes(), 0) else {
                continue;
            };

            cur_match.line_match = Some(m.start()..m.end());
        }
    }
}

impl Matcher for GrepMatcher {
    fn search(&self, src: Source<'_>) -> Result<Vec<MatchInfo>, MatcherError> {
        let mut matches = Vec::new();
        let mut searcher = self.build_searcher();

        // Both branches collect through the same UTF8 sink, so a path and its
        // in-memory contents yield identical matches.
        match src {
            Source::Path(path) => {
                searcher.search_path(&self.matcher, path, sink::UTF8::new(&mut matches))
            }
            Source::Content(content) => searcher.search_slice(
                &self.matcher,
                content.as_bytes(),
                sink::UTF8::new(&mut matches),
            ),
        }
        .map_err(|source| MatcherError::SearchError { source })?;

        self.annotate_line_matches(&mut matches);

        Ok(matches)
    }
}

#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;

    /// `true` when the pattern matches somewhere in `text` (searched as content).
    fn matches_somewhere(matcher: &GrepMatcher, text: &str) -> bool {
        !matcher.search(Source::Content(text)).unwrap().is_empty()
    }

    #[test]
    fn test_grep_matcher_simple_pattern() {
        let matcher = GrepMatcher::compile("test").unwrap();

        assert!(matches_somewhere(&matcher, "this is a test"));
        assert!(matches_somewhere(&matcher, "test"));
        assert!(!matches_somewhere(&matcher, "no match here"));
    }

    #[test]
    fn test_grep_matcher_regex_pattern() {
        let matcher = GrepMatcher::compile("fo+bar").unwrap();

        // "fo+bar" means "f" followed by one or more "o" followed by "bar"
        assert!(matches_somewhere(&matcher, "foobar")); // Two o's
        assert!(matches_somewhere(&matcher, "fooooobar")); // Many o's
        assert!(matches_somewhere(&matcher, "fobar")); // One o (minimum required by +)
        assert!(!matches_somewhere(&matcher, "fbar")); // No o, should not match
        assert!(!matches_somewhere(&matcher, "f bar")); // Space instead of o
    }

    #[test]
    fn test_grep_matcher_search_multiline() {
        let matcher = GrepMatcher::compile("match").unwrap();
        let content = "line 1\nthis is a match\nline 3\nanother match here\nline 5";

        let matches = matcher.search(Source::Content(content)).unwrap();

        assert_eq!(matches.len(), 2);

        assert_eq!(matches[0].line_num, 2);
        assert_eq!(matches[0].line_content, "this is a match\n");

        assert_eq!(matches[1].line_num, 4);
        assert_eq!(matches[1].line_content, "another match here\n");
    }

    #[test]
    fn test_grep_matcher_no_matches() {
        let matcher = GrepMatcher::compile("notfound").unwrap();
        let content = "line 1\nline 2\nline 3";

        let matches = matcher.search(Source::Content(content)).unwrap();

        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_grep_matcher_case_sensitive() {
        let matcher = GrepMatcher::compile("Test").unwrap();

        assert!(matches_somewhere(&matcher, "Test"));
        assert!(!matches_somewhere(&matcher, "test")); // Case sensitive by default
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
    fn test_grep_matcher_missing_path_is_an_error() {
        let matcher = GrepMatcher::compile("x").unwrap();
        let missing = std::env::temp_dir().join(format!(
            "bulked-matcher-missing-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        let err = matcher.search(Source::Path(&missing)).unwrap_err();

        assert!(matches!(err, MatcherError::SearchError { .. }));
    }

    #[test]
    fn test_grep_matcher_with_context() {
        let matcher = GrepMatcher::compile("MATCH").unwrap().with_context(3);

        // Create content with a match on line 5
        let content =
            "line 1\nline 2\nline 3\nline 4\nMATCH line 5\nline 6\nline 7\nline 8\nline 9";

        let matches = matcher.search(Source::Content(content)).unwrap();

        assert_eq!(matches.len(), 1, "Should find exactly one match");

        let m = &matches[0];
        assert_eq!(m.line_num, 5, "Match should be on line 5");
        assert_eq!(m.line_content, "MATCH line 5\n");

        // Check context before (lines 2, 3, 4)
        let before_lines: Vec<&str> = m.previous_lines.split_inclusive('\n').collect();
        assert_eq!(
            before_lines.len(),
            3,
            "Should have 3 lines of context before"
        );
        assert_eq!(before_lines[0], "line 2\n");
        assert_eq!(before_lines[1], "line 3\n");
        assert_eq!(before_lines[2], "line 4\n");

        // Check context after (lines 6, 7, 8)
        let after_lines: Vec<&str> = m.next_lines.split_inclusive('\n').collect();
        assert_eq!(after_lines.len(), 3, "Should have 3 lines of context after");
        assert_eq!(after_lines[0], "line 6\n");
        assert_eq!(after_lines[1], "line 7\n");
        assert_eq!(after_lines[2], "line 8\n");
    }
}
