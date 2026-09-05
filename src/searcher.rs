//! Core search logic - functional core with no I/O dependencies
//!
//! This module provides the Searcher, which orchestrates the search operation
//! using abstract dependencies (`ReadFs`, Matcher, Walker traits). This
//! implements the functional core of the hexagonal architecture.

use crate::filesystem::{FilesystemError, ReadFs};
use crate::matcher::{MatchInfo, Matcher, Source};
use crate::types::{MatchResult, SearchError};
use crate::walker::Walker;
use std::path::Path;

/// Core search orchestrator
///
/// This struct is generic over the `ReadFs`, Matcher, and Walker traits.
/// This allows it to work with any combination of implementations (real or test).
pub struct Searcher<FS, M, W>
where
    FS: ReadFs,
    M: Matcher,
    W: Walker,
{
    fs: FS,
    matcher: M,
    walker: W,
}

impl<FS, M, W> Searcher<FS, M, W>
where
    FS: ReadFs,
    M: Matcher,
    W: Walker,
{
    /// Create a new searcher with the given dependencies
    pub fn new(fs: FS, matcher: M, walker: W) -> Self {
        Self {
            fs,
            matcher,
            walker,
        }
    }

    /// Search a single file for matches
    ///
    /// Returns Ok with matches if successful, or Err with a `SearchError` if the file
    /// couldn't be searched.
    fn search_file(&self, path: &Path) -> Result<Vec<MatchResult>, SearchError> {
        // Note: Binary file detection is handled by GrepMatcher via BinaryDetection::quit
        // which automatically stops searching when encountering null bytes

        // Prefer searching the real path in place when the filesystem exposes
        // one; otherwise read the file through the `ReadFs` port and hand the
        // matcher its contents. Either way it is a single `Matcher::search`.
        let match_infos = match self.fs.as_real_path(path) {
            Some(real) => self.matcher.search(Source::Path(&real)),
            None => {
                // Read file contents. There is no `exists`/`is_file` pre-check:
                // `ReadFs::read` returns the typed `FileNotFound`/`NotAFile`/
                // `ReadError` directly, which avoids a check-then-use race.
                let content = self.read_to_string(path).map_err(|source| {
                    tracing::warn!("Failed to read {}: {}", path.display(), source);
                    source
                })?;

                self.matcher.search(Source::Content(&content))
            }
        }
        .map_err(|source| {
            tracing::warn!("Search error in {}: {}", path.display(), source);
            source
        })?;

        // Convert to MatchResult
        let matches: Vec<MatchResult> = match_infos
            .into_iter()
            .map(|info: MatchInfo| MatchResult::from_match_info(info, path.to_path_buf()))
            .collect();

        Ok(matches)
    }

    /// Read the whole file at `path` into a `String` through the `ReadFs` port.
    ///
    /// Invalid UTF-8 surfaces as a `ReadError` whose source has kind `InvalidData`.
    fn read_to_string(&self, path: &Path) -> Result<String, FilesystemError> {
        use std::io::Read;

        let mut content = String::new();
        self.fs
            .read(path)?
            .read_to_string(&mut content)
            .map_err(|source| FilesystemError::ReadError {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(content)
    }

    /// Search all files, yielding one item per file that matched or failed
    ///
    /// This is the main entry point for searching. It walks all files and
    /// searches each one. Files with no matches are skipped; every other file
    /// yields `Ok(matches)` (non-empty, in file order) or `Err(SearchError)`.
    pub fn search_all(&self) -> impl Iterator<Item = Result<Vec<MatchResult>, SearchError>> + '_ {
        self.walker
            .files()
            .filter_map(move |path| match self.search_file(&path) {
                Err(err) => Some(Err(err)),
                Ok(matches) if matches.is_empty() => None,
                Ok(matches) => Some(Ok(matches)),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use crate::matcher::regex::GrepMatcher;
    use crate::matcher::stub::StubMatcher;
    use crate::walker::simple::SimpleWalker;
    use std::path::PathBuf;

    /// Test Searcher with all test doubles (solitary unit test)
    #[test]
    fn test_searcher_with_all_test_doubles() {
        // Setup MemoryFS
        let fs = MemoryFS::new();
        let test_path = PathBuf::from("/test/foo.txt");
        fs.add_file(&test_path, "line 1\nTARGET line\nline 3")
            .unwrap();

        // Setup StubMatcher
        let mut stub_matcher = StubMatcher::new();
        stub_matcher.add_match(crate::matcher::MatchInfo {
            line_num: 2,
            byte_offset: 7,
            line_match: None,
            line_content: "TARGET line\n".to_string(),
            previous_lines: String::new(),
            next_lines: String::new(),
        });

        // Setup SimpleWalker
        let walker = SimpleWalker::new(vec![test_path.clone()]);
        // Create searcher with all test doubles
        let searcher = Searcher::new(fs, stub_matcher, walker);

        // Execute search
        let results: Vec<_> = searcher.search_all().collect();
        assert_eq!(results.len(), 1);
        let result = results[0].as_ref().unwrap();

        // Assertions
        assert_eq!(result.len(), 1);

        let match_result = &result[0];
        assert_eq!(match_result.file_path, test_path);
        assert_eq!(match_result.line_number, 2);
        assert_eq!(match_result.line_content, "TARGET line\n");
    }

    /// Test Searcher with real `GrepMatcher` and `MemoryFS` (sociable integration test)
    #[test]
    fn test_searcher_with_real_grep_matcher() {
        // Setup MemoryFS with multiple files
        let fs = MemoryFS::new();
        let file1 = PathBuf::from("/src/main.rs");
        let file2 = PathBuf::from("/src/lib.rs");

        fs.add_file(&file1, "fn main() {\n    println!(\"hello\");\n}\n")
            .unwrap();
        fs.add_file(&file2, "pub fn hello() {\n    println!(\"hello\");\n}\n")
            .unwrap();

        // Use real GrepMatcher
        let matcher = GrepMatcher::compile("hello").unwrap();

        // Setup walker
        let walker = SimpleWalker::new(vec![file1.clone(), file2.clone()]);
        // Create searcher
        let searcher = Searcher::new(fs, matcher, walker);

        // Execute search
        let results: Vec<_> = searcher
            .search_all()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let all_matches: Vec<_> = results.iter().flatten().collect();

        // Should find "hello" in both files
        assert_eq!(all_matches.len(), 3); // main.rs line 2, lib.rs lines 1 and 2

        // Check that matches are from both files
        let files_with_matches: std::collections::HashSet<_> =
            all_matches.iter().map(|m| &m.file_path).collect();
        assert!(files_with_matches.contains(&file1));
        assert!(files_with_matches.contains(&file2));
    }

    /// Test Searcher handles binary files correctly
    /// Binary files are now automatically skipped by `GrepMatcher` via `BinaryDetection::quit`
    #[test]
    fn test_searcher_skips_binary_files() {
        let fs = MemoryFS::new();
        let binary_file = PathBuf::from("/test/binary.bin");
        let text_file = PathBuf::from("/test/text.txt");

        // Add binary file (contains null byte)
        fs.add_file(&binary_file, "binary\0data").unwrap();
        fs.add_file(&text_file, "text data with match").unwrap();

        let matcher = GrepMatcher::compile("match").unwrap();
        let walker = SimpleWalker::new(vec![binary_file.clone(), text_file.clone()]);

        let searcher = Searcher::new(fs, matcher, walker);
        let results: Vec<_> = searcher
            .search_all()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let all_matches: Vec<_> = results.iter().flatten().collect();

        // Should find match in text file only
        assert_eq!(all_matches.len(), 1);
        assert_eq!(all_matches[0].file_path, text_file);

        // Binary file is silently skipped by GrepMatcher (no error, no matches)
    }

    /// Test Searcher handles nonexistent files
    #[test]
    fn test_searcher_handles_nonexistent_files() {
        let fs = MemoryFS::new();
        let nonexistent = PathBuf::from("/nonexistent.txt");

        let matcher = GrepMatcher::compile("test").unwrap();
        let walker = SimpleWalker::new(vec![nonexistent.clone()]);

        let searcher = Searcher::new(fs, matcher, walker);
        let results: Vec<_> = searcher.search_all().collect();

        // Should return an error for nonexistent file
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
        match &results[0] {
            Err(SearchError::FileReadError { .. }) => {
                // Expected error
            }
            _ => panic!("Expected FileReadError"),
        }
    }

    /// A missing file must surface as the typed `FileNotFound` error coming out
    /// of `ReadFs::read`, now that the searcher no longer pre-checks `exists`.
    #[test]
    fn test_search_missing_file_yields_file_not_found() {
        let fs = MemoryFS::new();
        let missing = PathBuf::from("/does/not/exist.txt");

        let matcher = GrepMatcher::compile("test").unwrap();
        let walker = SimpleWalker::new(vec![missing.clone()]);

        let searcher = Searcher::new(fs, matcher, walker);
        let results: Vec<_> = searcher.search_all().collect();

        assert_eq!(results.len(), 1);
        match &results[0] {
            Err(SearchError::FileReadError {
                source: FilesystemError::FileNotFound { path },
            }) => assert_eq!(path, &missing),
            other => panic!("expected FileReadError(FileNotFound), got {other:?}"),
        }
    }

    /// Test Searcher with no matches
    #[test]
    fn test_searcher_no_matches() {
        let fs = MemoryFS::new();
        let file = PathBuf::from("/test/file.txt");
        fs.add_file(&file, "no matches here").unwrap();

        let matcher = GrepMatcher::compile("nonexistent").unwrap();
        let walker = SimpleWalker::new(vec![file]);

        let searcher = Searcher::new(fs, matcher, walker);
        let results: Vec<_> = searcher
            .search_all()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(results.len(), 0);
    }

    /// Test Searcher with context extraction (Phase 2)
    #[test]
    fn test_searcher_with_context() {
        let fs = MemoryFS::new();
        let file = PathBuf::from("/test/file.txt");
        let content = "line 1\nline 2\nMATCH here\nline 4\nline 5\nline 6";
        fs.add_file(&file, content).unwrap();

        let matcher = GrepMatcher::compile("MATCH").unwrap().with_context(2);
        let walker = SimpleWalker::new(vec![file.clone()]);

        let searcher = Searcher::new(fs, matcher, walker);
        let results: Vec<_> = searcher
            .search_all()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].len(), 1);
        let m = &results[0][0];

        // Verify match details
        assert_eq!(m.file_path, file);
        assert_eq!(m.line_number, 3);
        assert!(m.line_content.contains("MATCH"));

        // Verify context before (lines 1-2), as newline-terminated lines
        assert_eq!(m.context_before, "line 1\nline 2\n");

        // Verify context after (lines 4-5)
        assert_eq!(m.context_after, "line 4\nline 5\n");
    }
}
