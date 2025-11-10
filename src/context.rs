//! Context extraction - pure functions for getting surrounding lines
//!
//! This module provides functions for extracting context lines around matches.
//! It depends only on the FileSystem trait, making it fully testable.

use crate::filesystem::FileSystem;
use crate::types::{ContextLine, MatchResult};
use std::path::Path;

/// Extract context lines for a single match
///
/// Returns (context_before, context_after) for the given match.
/// Handles edge cases:
/// - Start of file: fewer lines before
/// - End of file: fewer lines after
/// - Empty/single-line files: empty contexts
///
/// # Arguments
///
/// * `fs` - Filesystem implementation
/// * `file_path` - Path to the file
/// * `match_line` - Line number of the match (1-indexed)
/// * `context_lines` - Number of context lines to extract before and after
pub fn extract_context<FS: FileSystem>(
    fs: &FS,
    file_path: &Path,
    match_line: usize,
    context_lines: usize,
) -> Result<(Vec<ContextLine>, Vec<ContextLine>), String> {
    if match_line == 0 {
        return Err("Line numbers are 1-indexed".to_string());
    }

    // Read entire file to determine line count
    let content = fs.read_to_string(file_path)?;
    let total_lines = content.lines().count();

    if match_line > total_lines {
        return Err(format!(
            "Match line {} exceeds file length {}",
            match_line, total_lines
        ));
    }

    // Calculate context range
    let start_line = match_line.saturating_sub(context_lines).max(1);
    let end_line = (match_line + context_lines).min(total_lines);

    // Extract context before (if any)
    let context_before = if start_line < match_line {
        let lines = fs.read_line_range(file_path, start_line, match_line - 1)?;
        lines
            .into_iter()
            .enumerate()
            .map(|(idx, content)| ContextLine {
                line_number: start_line + idx,
                content,
            })
            .collect()
    } else {
        Vec::new()
    };

    // Extract context after (if any)
    let context_after = if match_line < end_line {
        let lines = fs.read_line_range(file_path, match_line + 1, end_line)?;
        lines
            .into_iter()
            .enumerate()
            .map(|(idx, content)| ContextLine {
                line_number: match_line + 1 + idx,
                content,
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok((context_before, context_after))
}

/// Add context to a match result
///
/// This is a convenience function that extracts context and adds it to a MatchResult.
pub fn add_context_to_match<FS: FileSystem>(
    fs: &FS,
    mut match_result: MatchResult,
    context_lines: usize,
) -> MatchResult {
    match extract_context(
        fs,
        &match_result.file_path,
        match_result.line_number,
        context_lines,
    ) {
        Ok((context_before, context_after)) => {
            match_result.context_before = context_before;
            match_result.context_after = context_after;
            match_result
        }
        Err(e) => {
            tracing::warn!(
                "Failed to extract context for {}:{}: {}",
                match_result.file_path.display(),
                match_result.line_number,
                e
            );
            match_result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;
    use std::path::PathBuf;

    #[test]
    fn test_extract_context_middle_of_file() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        // Create a file with 10 lines
        let content = (1..=10)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        fs.add_file(&path, &content).unwrap();

        // Match on line 5, request 2 lines of context
        let (before, after) = extract_context(&fs, &path, 5, 2).unwrap();

        // Should get lines 3-4 before
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].line_number, 3);
        assert_eq!(before[0].content, "line 3");
        assert_eq!(before[1].line_number, 4);
        assert_eq!(before[1].content, "line 4");

        // Should get lines 6-7 after
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].line_number, 6);
        assert_eq!(after[0].content, "line 6");
        assert_eq!(after[1].line_number, 7);
        assert_eq!(after[1].content, "line 7");
    }

    #[test]
    fn test_extract_context_near_start() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";
        fs.add_file(&path, content).unwrap();

        // Match on line 2, request 5 lines of context
        let (before, after) = extract_context(&fs, &path, 2, 5).unwrap();

        // Should only get line 1 before (not 5 lines)
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].line_number, 1);
        assert_eq!(before[0].content, "line 1");

        // Should get lines 3-5 after
        assert_eq!(after.len(), 3);
    }

    #[test]
    fn test_extract_context_near_end() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";
        fs.add_file(&path, content).unwrap();

        // Match on line 4, request 5 lines of context
        let (before, after) = extract_context(&fs, &path, 4, 5).unwrap();

        // Should get lines 1-3 before (within bounds, but limited by line 1)
        assert_eq!(before.len(), 3);

        // Should only get line 5 after (not 5 lines)
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].line_number, 5);
        assert_eq!(after[0].content, "line 5");
    }

    #[test]
    fn test_extract_context_first_line() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        let content = "line 1\nline 2\nline 3";
        fs.add_file(&path, content).unwrap();

        // Match on line 1
        let (before, after) = extract_context(&fs, &path, 1, 2).unwrap();

        // No context before
        assert_eq!(before.len(), 0);

        // Should get lines 2-3 after
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].line_number, 2);
        assert_eq!(after[1].line_number, 3);
    }

    #[test]
    fn test_extract_context_last_line() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        let content = "line 1\nline 2\nline 3";
        fs.add_file(&path, content).unwrap();

        // Match on line 3 (last line)
        let (before, after) = extract_context(&fs, &path, 3, 2).unwrap();

        // Should get lines 1-2 before
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].line_number, 1);
        assert_eq!(before[1].line_number, 2);

        // No context after
        assert_eq!(after.len(), 0);
    }

    #[test]
    fn test_extract_context_single_line_file() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        fs.add_file(&path, "only line").unwrap();

        // Match on line 1
        let (before, after) = extract_context(&fs, &path, 1, 20).unwrap();

        // No context available
        assert_eq!(before.len(), 0);
        assert_eq!(after.len(), 0);
    }

    #[test]
    fn test_extract_context_empty_file() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        fs.add_file(&path, "").unwrap();

        // Try to match on line 1 of empty file (should error)
        let result = extract_context(&fs, &path, 1, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_context_invalid_line_number() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        fs.add_file(&path, "line 1\nline 2").unwrap();

        // Line 0 is invalid
        assert!(extract_context(&fs, &path, 0, 2).is_err());

        // Line beyond file length
        assert!(extract_context(&fs, &path, 100, 2).is_err());
    }

    #[test]
    fn test_add_context_to_match() {
        let fs = MemoryFS::new();
        let path = PathBuf::from("/test/file.txt");
        let content = "line 1\nline 2\nMATCH\nline 4\nline 5";
        fs.add_file(&path, content).unwrap();

        let match_result = MatchResult {
            file_path: path.clone(),
            line_number: 3,
            line_content: "MATCH".to_string(),
            byte_offset: 14,
            context_before: vec![],
            context_after: vec![],
        };

        let result = add_context_to_match(&fs, match_result, 1);

        assert_eq!(result.context_before.len(), 1);
        assert_eq!(result.context_before[0].content, "line 2");

        assert_eq!(result.context_after.len(), 1);
        assert_eq!(result.context_after[0].content, "line 4");
    }
}
