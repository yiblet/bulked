//! Production pattern matcher using grep-regex
//!
//! This module provides GrepMatcher, which uses the grep-regex and grep-searcher
//! crates to perform fast regex matching. This is the production implementation
//! based on the same infrastructure used by ripgrep and Helix.

use super::{MatchInfo, Matcher};
use grep::matcher::Matcher as GrepMatcherTrait;
use grep::regex::RegexMatcher as GrepRegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, SearcherBuilder};

/// Production matcher using grep-regex
#[derive(Debug)]
pub struct GrepMatcher {
    matcher: GrepRegexMatcher,
}

impl Matcher for GrepMatcher {
    fn compile(pattern: &str) -> Result<Self, String>
    where
        Self: Sized,
    {
        let matcher = GrepRegexMatcher::new(pattern)
            .map_err(|e| format!("Invalid regex pattern '{}': {}", pattern, e))?;

        Ok(Self { matcher })
    }

    fn search_in_content(&self, content: &str) -> Vec<MatchInfo> {
        let mut matches = Vec::new();

        // Create a searcher with binary detection
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .line_number(true)
            .build();

        // Use UTF8 sink to collect matches
        let result = searcher.search_slice(
            &self.matcher,
            content.as_bytes(),
            UTF8(|line_num, line_content| {
                // Calculate approximate byte offset (sum of previous line lengths)
                let byte_offset = content
                    .lines()
                    .take(line_num as usize - 1)
                    .map(|l| l.len() + 1) // +1 for newline
                    .sum();

                matches.push(MatchInfo {
                    line_num: line_num as usize,
                    byte_offset,
                    line_content: line_content.trim_end().to_string(),
                });

                Ok(true) // Continue searching
            }),
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
}

#[cfg(test)]
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
        assert!(matcher.is_match("foobar"));   // Two o's
        assert!(matcher.is_match("fooooobar")); // Many o's
        assert!(matcher.is_match("fobar"));    // One o (minimum required by +)
        assert!(!matcher.is_match("fbar"));    // No o, should not match
        assert!(!matcher.is_match("f bar"));   // Space instead of o
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
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("Invalid regex pattern"));
    }
}
