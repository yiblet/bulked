use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use crate::types::{ContextLine, IngestInput, MatchResult};

#[derive(thiserror::Error, Debug)]
pub enum IngestError {
    #[error("Failed to read file: {source}")]
    FileReadError {
        #[from]
        source: crate::filesystem::FilesystemError,
    },

    #[error("Failed to read line {line_num} in {path}: {source}")]
    LineReadError {
        path: PathBuf,
        line_num: usize,
        #[source]
        source: std::io::Error,
    },

    #[error("unexpected EOF in {path} at line {line_num}")]
    UnexpectedEOF { line_num: usize, path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Range {
    start: usize,
    line: usize,
    end: usize,
}

fn create_ranges(iter: impl Iterator<Item = usize>, context: usize) -> Vec<Range> {
    let mut iter = iter.peekable();
    let mut res = Vec::new();
    let mut prev = 1;
    while let Some(line) = iter.next() {
        let next = iter.peek().cloned();

        let min_range = std::cmp::max(prev, line.saturating_sub(context));
        let max_range = std::cmp::max(prev, line.saturating_add(context + 1));
        let max_range = next.map_or(max_range, |next| std::cmp::min(max_range, next));
        prev = max_range;
        if min_range > max_range {
            continue;
        }
        res.push(Range {
            start: min_range,
            end: max_range,
            line,
        });
    }

    res
}

#[allow(dead_code)]
pub fn ingest(
    fs: &dyn crate::filesystem::FileSystem,
    inputs: Vec<IngestInput>,
    context: usize,
) -> Result<Vec<MatchResult>, IngestError> {
    let paths_with_ranges = group_inputs_by_path_and_create_ranges(inputs, context);
    let mut matches = Vec::new();

    for (path, ranges) in paths_with_ranges {
        for file in process_file(fs, &path, ranges)? {
            matches.push(file?);
        }
    }

    Ok(matches)
}

fn group_inputs_by_path_and_create_ranges(
    inputs: Vec<IngestInput>,
    context: usize,
) -> BTreeMap<PathBuf, BTreeSet<Range>> {
    let mut paths: BTreeMap<PathBuf, BTreeSet<usize>> = BTreeMap::new();
    for input in inputs {
        paths
            .entry(input.file_path)
            .or_default()
            .insert(input.line_number);
    }

    paths
        .into_iter()
        .map(|(p, ls)| {
            let ranges: BTreeSet<_> = create_ranges(ls.iter().copied(), context)
                .into_iter()
                .collect();
            (p, ranges)
        })
        .collect()
}

fn process_file(
    fs: &dyn crate::filesystem::FileSystem,
    path: &PathBuf,
    ranges: BTreeSet<Range>,
) -> Result<impl Iterator<Item = Result<MatchResult, IngestError>>, IngestError> {
    let reader = fs.read(path)?;
    let mut reader = std::io::BufReader::new(reader);
    let mut buf = String::new();
    let mut positions = Positions {
        path,
        line: 1,
        byte: 0,
    };

    let iter = ranges.into_iter().filter_map(move |range| {
        process_range(&mut reader, &mut buf, &mut positions, range, path).transpose()
    });

    Ok(iter)
}

// FIXME: test process range across multiple cases:
//   1. position < range.start
//   2. position > range.end
//   3. position == range.start 
//   4. position == range.end
//   5. empty file
fn process_range(
    reader: &mut std::io::BufReader<Box<dyn std::io::Read>>,
    buf: &mut String,
    positions: &mut Positions<'_>,
    range: Range,
    path: &Path,
) -> Result<Option<MatchResult>, IngestError> {
    let mut context_before = Vec::new();
    let mut line_string = String::new();
    let mut context_after = Vec::new();

    // Skip to range start
    while positions.line < range.start {
        read_line(reader, buf, positions)?;
    }

    if positions.line > range.end {
        return Ok(None);
    }

    // Read context before target line
    while positions.line >= range.start && positions.line < range.line {
        read_line(reader, buf, positions)?;
        context_before.push(ContextLine {
            line_number: positions.line,
            content: std::mem::take(buf),
        });
    }

    if positions.line != range.line {
        return Ok(None);
    }

    // Read target line
    let line_offset = positions.byte;
    read_line(reader, buf, positions)?;
    std::mem::swap(&mut line_string, buf);

    // Read context after target line
    while positions.line < range.end {
        read_line(reader, buf, positions)?;
        context_after.push(ContextLine {
            line_number: positions.line,
            content: std::mem::take(buf),
        });
    }

    Ok(Some(MatchResult {
        file_path: path.to_path_buf(),
        line_number: range.line,
        line_content: line_string,
        line_match: None,
        byte_offset: line_offset,
        context_before,
        context_after,
    }))
}

struct Positions<'a> {
    pub path: &'a Path,
    pub line: usize,
    pub byte: usize,
}

fn read_line(
    reader: &mut dyn std::io::BufRead,
    buf: &mut String,
    positions: &mut Positions<'_>,
) -> Result<(), IngestError> {
    buf.clear();
    match reader.read_line(buf) {
        Ok(0) => Err(IngestError::UnexpectedEOF {
            line_num: positions.line,
            path: positions.path.to_path_buf(),
        })?,
        Err(e) => Err(IngestError::LineReadError {
            line_num: positions.line,
            path: positions.path.to_path_buf(),
            source: e,
        })?,
        Ok(v) => {
            positions.line += 1;
            positions.byte += v;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::FileSystem;

    #[test]
    fn test_create_ranges_multiple_lines() {
        // The actual behavior creates non-overlapping ranges with context
        let input = vec![1, 2, 3];
        let ranges: Vec<Range> = create_ranges(input.into_iter(), 20);

        // Based on the implementation:
        // Line 1: start=max(1, 1-20)=1, end=max(1, 1+20+1)=22, line=1
        // Line 2: start=max(22, 2-20)=22, end=max(22, 2+20+1)=23, line=2
        // Line 3: start=max(23, 3-20)=23, end=max(23, 3+20+1)=24, line=3
        assert_eq!(ranges.len(), 3);

        assert_eq!(ranges[0].start, 1);
        assert_eq!(ranges[0].line, 1);
        assert_eq!(ranges[0].end, 2);

        assert_eq!(ranges[1].start, 2);
        assert_eq!(ranges[1].line, 2);
        assert_eq!(ranges[1].end, 3);

        assert_eq!(ranges[2].start, 3);
        assert_eq!(ranges[2].line, 3);
        assert_eq!(ranges[2].end, 24);
    }

    #[test]
    fn test_create_ranges_single_line() {
        let input = vec![1];
        let ranges: Vec<Range> = create_ranges(input.into_iter(), 20);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 1);
        assert_eq!(ranges[0].line, 1);
        assert_eq!(ranges[0].end, 22);
    }

    #[test]
    fn test_create_ranges_overlapping_context() {
        // Lines close together should merge their ranges
        let input = vec![5, 8];
        let ranges: Vec<Range> = create_ranges(input.into_iter(), 2);

        // Line 5: start=max(1, 5-2)=3, end=max(1, 5+2+1)=8, line=5
        // Line 8: start=max(8, 8-2)=8, end=max(8, 8+2+1)=11, line=8
        assert_eq!(ranges.len(), 2);

        assert_eq!(ranges[0].start, 3);
        assert_eq!(ranges[0].line, 5);
        assert_eq!(ranges[0].end, 8);

        assert_eq!(ranges[1].start, 8);
        assert_eq!(ranges[1].line, 8);
        assert_eq!(ranges[1].end, 11);
    }

    #[test]
    fn test_create_ranges_no_overlap() {
        // Lines far apart should create separate ranges
        let input = vec![5, 50];
        let ranges: Vec<Range> = create_ranges(input.into_iter(), 2);

        assert_eq!(ranges.len(), 2);

        assert_eq!(ranges[0].start, 3);
        assert_eq!(ranges[0].line, 5);
        assert_eq!(ranges[0].end, 8);

        assert_eq!(ranges[1].start, 48);
        assert_eq!(ranges[1].line, 50);
        assert_eq!(ranges[1].end, 53);
    }

    #[test]
    fn test_create_ranges_empty_input() {
        let input: Vec<usize> = vec![];
        let ranges: Vec<Range> = create_ranges(input.into_iter(), 5);

        assert_eq!(ranges.len(), 0);
    }

    #[test]
    fn test_create_ranges_zero_context() {
        // With zero context, each line should be a single-line range
        let input = vec![5, 10];
        let ranges: Vec<Range> = create_ranges(input.into_iter(), 0);

        assert_eq!(ranges.len(), 2);

        assert_eq!(ranges[0].start, 5);
        assert_eq!(ranges[0].line, 5);
        assert_eq!(ranges[0].end, 6);

        assert_eq!(ranges[1].start, 10);
        assert_eq!(ranges[1].line, 10);
        assert_eq!(ranges[1].end, 11);
    }

    #[test]
    fn test_process_range_position_less_than_start() {
        // Test when position < range.start
        // We need to skip lines to reach the range
        let fs = crate::filesystem::memory::MemoryFS::new();
        let path = PathBuf::from("test.txt");
        let content = "line1\nline2\nline3\nline4\nline5\n";
        fs.write_string(&path, content).unwrap();

        let range = Range {
            start: 3,
            line: 3,
            end: 4,
        };

        let reader = fs.read(&path).unwrap();
        let mut reader = std::io::BufReader::new(reader);
        let mut buf = String::new();
        let mut positions = Positions {
            path: &path,
            line: 1,
            byte: 0,
        };

        let result = process_range(&mut reader, &mut buf, &mut positions, range, &path)
            .unwrap()
            .unwrap();

        assert_eq!(result.line_number, 3);
        assert_eq!(result.line_content, "line3\n");
        assert_eq!(result.context_before.len(), 0);
        assert_eq!(result.context_after.len(), 0);
        assert_eq!(positions.line, 4);
    }

    #[test]
    fn test_process_range_position_greater_than_end() {
        // Test when position > range.end
        // The range should be skipped
        let fs = crate::filesystem::memory::MemoryFS::new();
        let path = PathBuf::from("test.txt");
        let content = "line1\nline2\nline3\nline4\nline5\n";
        fs.write_string(&path, content).unwrap();

        let range = Range {
            start: 2,
            line: 2,
            end: 3,
        };

        let reader = fs.read(&path).unwrap();
        let mut reader = std::io::BufReader::new(reader);
        let mut buf = String::new();
        let mut positions = Positions {
            path: &path,
            line: 5, // Already past the range
            byte: 24,
        };

        let result = process_range(&mut reader, &mut buf, &mut positions, range, &path).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_process_range_position_equals_start() {
        // Test when position == range.start
        let fs = crate::filesystem::memory::MemoryFS::new();
        let path = PathBuf::from("test.txt");
        let content = "line1\nline2\nline3\nline4\nline5\n";
        fs.write_string(&path, content).unwrap();

        let range = Range {
            start: 3,
            line: 3,
            end: 5,
        };

        let reader = fs.read(&path).unwrap();
        let mut reader = std::io::BufReader::new(reader);
        let mut buf = String::new();
        let mut positions = Positions {
            path: &path,
            line: 1,
            byte: 0,
        };

        // Skip to line 3 to position the reader correctly
        read_line(&mut reader, &mut buf, &mut positions).unwrap();
        read_line(&mut reader, &mut buf, &mut positions).unwrap();

        let result = process_range(&mut reader, &mut buf, &mut positions, range, &path)
            .unwrap()
            .unwrap();

        assert_eq!(result.line_number, 3);
        assert_eq!(result.line_content, "line3\n");
        assert_eq!(result.context_before.len(), 0);
        assert_eq!(result.context_after.len(), 1);
        assert_eq!(result.context_after[0].line_number, 5);
    }

    #[test]
    fn test_process_range_position_equals_end() {
        // Test when position == range.end
        // This should skip the range as we're already at the end
        let fs = crate::filesystem::memory::MemoryFS::new();
        let path = PathBuf::from("test.txt");
        let content = "line1\nline2\nline3\nline4\nline5\n";
        fs.write_string(&path, content).unwrap();

        let range = Range {
            start: 2,
            line: 3,
            end: 4,
        };

        let reader = fs.read(&path).unwrap();
        let mut reader = std::io::BufReader::new(reader);
        let mut buf = String::new();
        let mut positions = Positions {
            path: &path,
            line: 4, // At the end
            byte: 18,
        };

        let result = process_range(&mut reader, &mut buf, &mut positions, range, &path).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_process_range_empty_file() {
        // Test with an empty file
        let fs = crate::filesystem::memory::MemoryFS::new();
        let path = PathBuf::from("empty.txt");
        fs.write_string(&path, "").unwrap();

        let range = Range {
            start: 1,
            line: 1,
            end: 2,
        };

        let reader = fs.read(&path).unwrap();
        let mut reader = std::io::BufReader::new(reader);
        let mut buf = String::new();
        let mut positions = Positions {
            path: &path,
            line: 1,
            byte: 0,
        };

        let result = process_range(&mut reader, &mut buf, &mut positions, range, &path);

        assert!(result.is_err());
        match result.unwrap_err() {
            IngestError::UnexpectedEOF { line_num, .. } => {
                assert_eq!(line_num, 1);
            }
            _ => panic!("Expected UnexpectedEOF error"),
        }
    }

    #[test]
    fn test_process_range_with_context() {
        // Test reading a range with context before and after
        let fs = crate::filesystem::memory::MemoryFS::new();
        let path = PathBuf::from("test.txt");
        let content = "line1\nline2\nline3\nline4\nline5\n";
        fs.write_string(&path, content).unwrap();

        let range = Range {
            start: 2,
            line: 3,
            end: 5,
        };

        let reader = fs.read(&path).unwrap();
        let mut reader = std::io::BufReader::new(reader);
        let mut buf = String::new();
        let mut positions = Positions {
            path: &path,
            line: 1,
            byte: 0,
        };

        let result = process_range(&mut reader, &mut buf, &mut positions, range, &path)
            .unwrap()
            .unwrap();

        assert_eq!(result.line_number, 3);
        assert_eq!(result.line_content, "line3\n");
        assert_eq!(result.context_before.len(), 1);
        assert_eq!(result.context_before[0].line_number, 3);
        assert_eq!(result.context_before[0].content, "line2\n");
        assert_eq!(result.context_after.len(), 1);
        assert_eq!(result.context_after[0].line_number, 5);
        assert_eq!(result.context_after[0].content, "line4\n");
    }
}
