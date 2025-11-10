//! Stub matcher implementation for testing
//!
//! This module provides StubMatcher, a test double that returns predefined
//! match results. This allows testing search logic without depending on
//! regex engine behavior.

use super::{MatchInfo, Matcher};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

/// Stub matcher for testing
///
/// This is a test double that returns predefined matches.
/// It allows complete control over match behavior in tests.
#[allow(dead_code)]
pub(crate) struct StubMatcher {
    matches: Arc<Mutex<Vec<MatchInfo>>>,
    predicate: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

#[allow(dead_code)]
impl StubMatcher {
    /// Create a new stub matcher with no predefined matches
    pub fn new() -> Self {
        Self {
            matches: Arc::new(Mutex::new(Vec::new())),
            predicate: Arc::new(|_| false),
        }
    }

    /// Add a predefined match that will be returned by search_in_content
    pub fn add_match(&mut self, match_info: MatchInfo) {
        if let Ok(mut matches) = self.matches.lock() {
            matches.push(match_info);
        }
    }

    /// Set a predicate function for is_match
    pub fn set_predicate<F>(&mut self, predicate: F)
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.predicate = Arc::new(predicate);
    }

    /// Create a stub matcher that always matches
    pub fn always_match() -> Self {
        let mut matcher = Self::new();
        matcher.set_predicate(|_| true);
        matcher
    }

    /// Create a stub matcher that never matches
    pub fn never_match() -> Self {
        let mut matcher = Self::new();
        matcher.set_predicate(|_| false);
        matcher
    }

    /// Create a stub matcher that matches lines containing specific text
    pub fn match_containing(text: String) -> Self {
        let mut matcher = Self::new();
        matcher.set_predicate(move |line| line.contains(&text));
        matcher
    }
}

impl Default for StubMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Matcher for StubMatcher {
    fn compile(_pattern: &str) -> Result<Self, String>
    where
        Self: Sized,
    {
        // Stub matcher always succeeds compilation
        Ok(Self::new())
    }

    fn search_in_content(&self, _content: &str) -> Vec<MatchInfo> {
        // Return predefined matches, ignoring actual content
        self.matches
            .lock()
            .map(|matches| matches.clone())
            .unwrap_or_default()
    }

    fn is_match(&self, text: &str) -> bool {
        (self.predicate)(text)
    }

    fn search_path(
        &self,
    ) -> Option<impl FnMut(&std::path::Path) -> Result<Vec<MatchInfo>, String>> {
        None::<Box<dyn FnMut(&Path) -> Result<Vec<MatchInfo>, String>>>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_matcher_new_has_no_matches() {
        let matcher = StubMatcher::new();
        let matches = matcher.search_in_content("any content");
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_stub_matcher_add_match() {
        let mut matcher = StubMatcher::new();
        matcher.add_match(MatchInfo {
            line_num: 5,
            byte_offset: 42,
            line_content: "test line".to_string(),
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        let matches = matcher.search_in_content("ignored");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_num, 5);
        assert_eq!(matches[0].byte_offset, 42);
        assert_eq!(matches[0].line_content, "test line");
    }

    #[test]
    fn test_stub_matcher_multiple_matches() {
        let mut matcher = StubMatcher::new();
        matcher.add_match(MatchInfo {
            line_num: 1,
            byte_offset: 0,
            line_content: "first".to_string(),
            previous_lines: String::new(),
            next_lines: String::new(),
        });
        matcher.add_match(MatchInfo {
            line_num: 2,
            byte_offset: 10,
            line_content: "second".to_string(),
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        let matches = matcher.search_in_content("ignored");

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_content, "first");
        assert_eq!(matches[1].line_content, "second");
    }

    #[test]
    fn test_stub_matcher_always_match() {
        let matcher = StubMatcher::always_match();

        assert!(matcher.is_match("anything"));
        assert!(matcher.is_match(""));
        assert!(matcher.is_match("whatever"));
    }

    #[test]
    fn test_stub_matcher_never_match() {
        let matcher = StubMatcher::never_match();

        assert!(!matcher.is_match("anything"));
        assert!(!matcher.is_match(""));
        assert!(!matcher.is_match("whatever"));
    }

    #[test]
    fn test_stub_matcher_match_containing() {
        let matcher = StubMatcher::match_containing("target".to_string());

        assert!(matcher.is_match("this has target in it"));
        assert!(matcher.is_match("target"));
        assert!(!matcher.is_match("no match here"));
    }

    #[test]
    fn test_stub_matcher_custom_predicate() {
        let mut matcher = StubMatcher::new();
        matcher.set_predicate(|text| text.len() > 5);

        assert!(matcher.is_match("longer than five"));
        assert!(!matcher.is_match("short"));
    }

    #[test]
    fn test_stub_matcher_compile_always_succeeds() {
        let result = StubMatcher::compile("any pattern");
        assert!(result.is_ok());
    }
}
