use super::fingerprint::Fingerprint;
use super::range::LineRange;
use miette::{Diagnostic, SourceSpan};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

/// Errors that can occur while parsing the format.
#[derive(Debug, Error, Diagnostic)]
pub enum FormatError {
    #[error("Invalid start delimiter")]
    #[diagnostic(
        code(format::invalid_delimiter),
        help("Expected format: @<path>:<line>:<numlines>[ #<fingerprint>]")
    )]
    InvalidDelimiter {
        #[source_code]
        src: String,
        #[label("Invalid delimiter here")]
        span: SourceSpan,
    },

    #[error("Invalid line number: {value}")]
    #[diagnostic(
        code(format::invalid_line_number),
        help("Line number must be a positive integer")
    )]
    InvalidLineNumber {
        value: String,
        #[source_code]
        src: String,
        #[label("Expected a number here")]
        span: SourceSpan,
    },

    #[error("Invalid numlines: {value}")]
    #[diagnostic(
        code(format::invalid_numlines),
        help("numlines must be a positive integer")
    )]
    InvalidNumLines {
        value: String,
        #[source_code]
        src: String,
        #[label("Expected a number here")]
        span: SourceSpan,
    },

    #[error("Invalid fingerprint: {value}")]
    #[diagnostic(
        code(format::invalid_fingerprint),
        help(
            "A fingerprint is `#` followed by exactly 8 hex digits, as written by `bulked ingest`/`search`. Delete it to apply the chunk unchecked"
        )
    )]
    InvalidFingerprint {
        value: String,
        #[source_code]
        src: String,
        #[label("Expected 8 hex digits here")]
        span: SourceSpan,
    },

    #[error("Missing end delimiter @@@")]
    #[diagnostic(
        code(format::missing_end_delimiter),
        help("Each chunk must be terminated with @@@")
    )]
    MissingEndDelimiter {
        #[source_code]
        src: String,
        #[label("Chunk started here")]
        start_span: SourceSpan,
        #[label("Expected @@@ before end of file")]
        eof_span: SourceSpan,
    },

    #[error("A content line starts with `@` but is not the `@@@` terminator")]
    #[diagnostic(
        code(format::unescaped_at_line),
        help(
            "Every chunk ends with a line `@@@`. If this line starts a new chunk, add `@@@` above it; if it is content, write it as `\\@...`"
        )
    )]
    UnescapedAtLine {
        #[source_code]
        src: String,
        #[label("Chunk started here")]
        start_span: SourceSpan,
        #[label("Line starts with `@`")]
        span: SourceSpan,
    },

    #[error("No chunks found in input")]
    #[diagnostic(
        code(format::no_chunks),
        help("File must contain at least one chunk starting with @<path>:<line>:<numlines>")
    )]
    NoChunks {
        #[source_code]
        src: String,
    },
}

/// Format represents a structured file format for storing code chunks with metadata.
///
/// # File Format Specification
///
/// The format uses a simple text-based structure:
///
/// ```text
/// @/path/to/file.txt:line:numlines
/// <chunk content>
/// @@@
///
/// @/path/to/file.txt:line:numlines
/// <chunk content>
/// @@@
/// ```
///
/// ## Format Rules
///
/// - **Start delimiter**: `@<path>:<line>:<numlines>[ #<fingerprint>]` marks the
///   beginning of a chunk
///   - `<path>`: Absolute or relative file path
///   - `<line>`: Starting line number (1-indexed)
///   - `<numlines>`: Number of *original* lines the chunk replaces
///   - `#<fingerprint>` (optional): 8 hex digits, a [`Fingerprint`] of those
///     original lines. `ingest`/`search` write it; `apply` refuses the plan if the
///     lines no longer match (the file changed, or the chunk was already applied).
///     A chunk without one is applied unchecked.
///
/// - **End delimiter**: `@@@` marks the end of a chunk
///
/// - **Comments**: Text between chunks (outside delimiters) is ignored and can be used for comments
///
/// - **Escaping** (see [`super::escaping`]): only the *start* of a content line
///   is ever special. A content line starting with `@`, `\@` or `\\` is written
///   with one extra `\` in front; on parse a line starting with `\@` or `\\` drops
///   that first `\`. Nothing mid-line is escaped. An unescaped line starting with
///   `@` inside a chunk is a parse error, never content.
///
/// ## Example
///
/// ```text
/// @src/main.rs:10:3
/// fn main() {
///     println!("Hello, world!");
/// }
/// @@@
///
/// This is a comment - it will be ignored
///
/// @src/lib.rs:5:2
/// pub fn greet() {
///     println!("Hi from lib");
/// }
/// @@@
/// ```
/// Format is a collection of chunks.
///
/// The chunks are always sorted by [`ChunkRef`] (path, then line range): the only
/// way to build a `Format` is [`Format::new`], which sorts, so consumers such as
/// [`Format::file_chunks`] never reorder anything and never need `&mut self`.
#[derive(Debug)]
pub struct Format(Vec<Chunk>);

impl Format {
    /// Builds a `Format` from `chunks`, sorting them by path and then line range.
    #[must_use]
    pub fn new(mut chunks: Vec<Chunk>) -> Self {
        chunks.sort_by(|c1, c2| c1.as_ref().cmp(&c2.as_ref()));
        Self(chunks)
    }

    /// Iterates over the chunks in sorted order.
    pub fn iter(&self) -> std::slice::Iter<'_, Chunk> {
        self.0.iter()
    }

    /// Returns the number of chunks in the format.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the format contains no chunks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Drops the fingerprint from every chunk, so `apply` will replace the lines
    /// without checking that they still match what the chunk was generated from.
    ///
    /// Chunks sort by `(path, range)` only, so the order is unchanged.
    #[must_use]
    pub fn without_fingerprints(mut self) -> Self {
        for chunk in &mut self.0 {
            chunk.fingerprint = None;
        }
        self
    }

    /// Converts a slice of match results into a Format.
    /// Each match result is converted to a chunk containing the match line
    /// along with its before and after context lines.
    pub fn from_matches(matches: &[crate::types::MatchResult]) -> Self {
        let chunks: Vec<Chunk> = matches
            .iter()
            .map(|match_result| {
                // The context strings are runs of '\n'-terminated lines, so the
                // number of context lines is the number of newline-delimited
                // pieces (zero for an empty string).
                let before_count = match_result.context_before.split_inclusive('\n').count();
                let after_count = match_result.context_after.split_inclusive('\n').count();

                // The chunk starts `before_count` lines above the match line and
                // spans the context before, the match line, and the context after.
                let start_line = match_result.line_number - before_count;
                let num_lines = before_count + 1 + after_count;

                // Build the content from context_before + match line + context_after.
                let mut content = String::with_capacity(
                    match_result.context_before.len()
                        + match_result.line_content.len()
                        + match_result.context_after.len(),
                );
                content.push_str(&match_result.context_before);

                // The match range is relative to the match line; shift it past the
                // context that precedes the line inside the chunk.
                let match_range = match_result
                    .line_match
                    .as_ref()
                    .map(|range| range.start + content.len()..range.end + content.len());

                content.push_str(&match_result.line_content);
                content.push_str(&match_result.context_after);

                // Invariant: a match always contributes its own line, so `num_lines >= 1`,
                // and matchers report 1-indexed line numbers with at most `line_number - 1`
                // lines of context before, so `start_line >= 1`.
                let range = LineRange::from_usize(start_line, num_lines)
                    .expect("a match contributes at least one line");

                // The content *is* the original text at this point, so its
                // fingerprint lets `apply` detect that the file has since changed.
                let fingerprint = Fingerprint::of(content.as_bytes());
                Chunk::new(match_result.file_path.clone(), range, content)
                    .with_match_range(match_range)
                    .with_fingerprint(Some(fingerprint))
            })
            .collect();

        Self::new(chunks)
    }

    /// Groups the chunks by file: one `(path, chunks)` entry per distinct path, in
    /// path order, each slice in line order. This is a view over an already-sorted
    /// `Format`, so it never mutates and calling it repeatedly yields the same groups.
    #[must_use]
    pub fn file_chunks(&self) -> Vec<(&Path, &[Chunk])> {
        let mut res = Vec::new();
        let mut cur_file = None;
        for (idx, chunk) in self.0.iter().enumerate() {
            match cur_file {
                None => {
                    cur_file = Some((0, chunk));
                }
                Some((start, start_chunk)) if chunk.path() != start_chunk.path() => {
                    res.push((start_chunk.path(), &self.0[start..idx]));
                    cur_file = Some((idx, chunk));
                }
                _ => {}
            }
        }

        if let Some((start, chunk)) = cur_file {
            res.push((chunk.path(), &self.0[start..]));
        }

        res
    }

    #[must_use]
    pub fn display(&self, plain: bool, highlight: bool) -> Display<'_> {
        Display {
            format: self,
            plain,
            highlight,
        }
    }
}

/// The identity of a chunk — where it lives — without its content.
///
/// Ordering is by path, then by [`LineRange`] (start, then length); `Format` sorts
/// its chunks by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkRef<'a> {
    pub path: &'a Path,
    pub range: LineRange,
}

/// Chunk represents a single code snippet with its metadata and content.
///
/// The lines it covers are a [`LineRange`], so a chunk with a zero start line or
/// zero length cannot be constructed; the parser rejects such input with a span.
#[derive(Debug)]
pub struct Chunk {
    path: PathBuf,
    range: LineRange,
    content: String,
    match_range: Option<Range<usize>>,
    fingerprint: Option<Fingerprint>,
}

impl Chunk {
    /// Creates a new Chunk covering `range` in `path` with the given content.
    pub fn new(path: PathBuf, range: LineRange, content: String) -> Self {
        Self {
            path,
            range,
            content,
            match_range: None,
            fingerprint: None,
        }
    }

    /// Test convenience: build a chunk from plain integers.
    ///
    /// # Panics
    /// Panics if `start` or `len` is zero — test fixtures must be valid ranges.
    #[cfg(test)]
    pub fn from_parts(
        path: impl Into<PathBuf>,
        start: usize,
        len: usize,
        content: impl Into<String>,
    ) -> Self {
        let range = LineRange::from_usize(start, len)
            .unwrap_or_else(|| panic!("invalid test chunk range: start={start}, len={len}"));
        Self::new(path.into(), range, content.into())
    }

    pub fn with_match_range(mut self, match_range: Option<Range<usize>>) -> Self {
        self.match_range = match_range;
        self
    }

    /// Attach the [`Fingerprint`] of the original lines this chunk replaces.
    #[must_use]
    pub fn with_fingerprint(mut self, fingerprint: Option<Fingerprint>) -> Self {
        self.fingerprint = fingerprint;
        self
    }

    /// Fingerprint of the original lines, if the header carried one.
    pub fn fingerprint(&self) -> Option<Fingerprint> {
        self.fingerprint
    }

    #[must_use]
    pub fn as_ref(&self) -> ChunkRef<'_> {
        ChunkRef {
            path: self.path.as_path(),
            range: self.range,
        }
    }

    /// The file this chunk belongs to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The lines this chunk covers.
    pub fn range(&self) -> LineRange {
        self.range
    }

    /// The 1-indexed first line of the chunk.
    pub fn start_line(&self) -> usize {
        self.range.start()
    }

    /// The number of lines the chunk covers (always `>= 1`).
    pub fn num_lines(&self) -> usize {
        self.range.len()
    }

    /// The chunk's (unescaped) text.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Byte range within `content` of the matched text, if this chunk came from a search.
    pub fn match_range(&self) -> Option<&Range<usize>> {
        self.match_range.as_ref()
    }
}

pub struct Display<'a> {
    pub format: &'a Format,
    pub plain: bool,
    pub highlight: bool,
}

/// Write `content` line by line with ambiguous line starts escaped (see
/// [`crate::format::escaping`]), optionally wrapping the bytes in `highlight` in
/// ANSI red. Escaping is decided per line *before* any color codes are inserted,
/// so a match that begins mid-line can never be mistaken for a line start.
/// Write a chunk body: `content` with each line escaped, then the terminator
/// token. Content that already ends with `'\n'` is closed by `@@@`; otherwise the
/// line is finished and "no trailing newline at EOF" is marked with `@@@-`. No
/// newline is written after the terminator.
pub(crate) fn write_body(
    f: &mut dyn fmt::Write,
    content: &str,
    highlight: Option<&Range<usize>>,
) -> fmt::Result {
    write_escaped_content(f, content, highlight)?;
    if content.ends_with('\n') {
        f.write_str("@@@")
    } else {
        f.write_str("\n@@@-")
    }
}

/// The body text `content` serializes to, as [`write_body`] writes it: what sits
/// between a chunk header line and the end of its `@@@` / `@@@-` token.
#[must_use]
pub fn chunk_body(content: &str) -> String {
    let mut body = String::with_capacity(content.len() + 5);
    write_body(&mut body, content, None).expect("writing to a String cannot fail");
    body
}

fn write_escaped_content(
    f: &mut dyn fmt::Write,
    content: &str,
    highlight: Option<&Range<usize>>,
) -> fmt::Result {
    const RED: &str = "\x1b[31m";
    const RESET: &str = "\x1b[0m";

    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let (start, end) = (offset, offset + line.len());
        offset = end;

        if crate::format::escaping::needs_escape(line) {
            f.write_str("\\")?;
        }

        match highlight {
            // The highlighted bytes overlap this line: split it around them.
            Some(range) if range.start < end && range.end > start => {
                let hl_start = range.start.max(start) - start;
                let hl_end = range.end.min(end) - start;
                f.write_str(&line[..hl_start])?;
                f.write_str(RED)?;
                f.write_str(&line[hl_start..hl_end])?;
                f.write_str(RESET)?;
                f.write_str(&line[hl_end..])?;
            }
            _ => f.write_str(line)?,
        }
    }
    Ok(())
}

fn display_format(f: &mut fmt::Formatter, format: &Format, highlight: bool) -> std::fmt::Result {
    for (idx, chunk) in format.iter().enumerate() {
        if idx != 0 {
            f.write_str("\n")?;
        };

        // Start delimiter: @path:line:numlines[ #fingerprint]
        write!(
            f,
            "@{}:{}:{}",
            chunk.path().display(),
            chunk.start_line(),
            chunk.num_lines()
        )?;
        if let Some(fingerprint) = chunk.fingerprint() {
            write!(f, " #{fingerprint}")?;
        }
        writeln!(f)?;

        let content = chunk.content();
        let highlight_range = chunk.match_range().filter(|_| highlight);
        write_body(f, content, highlight_range)?;
        writeln!(f)?;
    }

    Ok(())
}

fn display_plain(f: &mut fmt::Formatter, format: &Format, highlight: bool) -> std::fmt::Result {
    for chunk in format.iter() {
        writeln!(f, "\n{}:{}", chunk.path().display(), chunk.start_line())?;
        let mut bytes = 0;
        for (line_no, line) in (chunk.start_line()..).zip(chunk.content().split_inclusive('\n')) {
            let start = bytes;
            let end = bytes + line.len();
            match chunk.match_range() {
                Some(range) if start <= range.start && end > range.end => {
                    if highlight {
                        let start_red = "\x1b[31m";
                        let end_red = "\x1b[0m";

                        let line_start = range.start - start;
                        let line_end = range.end - start;

                        write!(
                            f,
                            "  {:4} > {}{}{}{}{}",
                            line_no,
                            line.get(..line_start).unwrap_or_default(),
                            start_red,
                            &line[line_start..line_end],
                            end_red,
                            line.get(line_end..).unwrap_or_default()
                        )?;
                    } else {
                        write!(f, "  {:4} > {}", line_no, line)?;
                    }
                }
                _ => {
                    write!(f, "  {:4} | {}", line_no, line)?;
                }
            }

            bytes += line.len();
        }
    }

    Ok(())
}

impl fmt::Display for Display<'_> {
    /// Serializes the Format to the file format string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.plain {
            display_plain(f, self.format, self.highlight)
        } else {
            display_format(f, self.format, self.highlight)
        }
    }
}

impl fmt::Display for Format {
    /// Serializes the Format to the file format string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        display_format(f, self, false)
    }
}

impl FromStr for Format {
    type Err = FormatError;

    /// Parses a Format from the file format string using nom parser combinators.
    ///
    /// # Errors
    ///
    /// Returns a detailed error with source location if the format is invalid.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        crate::format::parse::parse_format(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MatchResult;
    use std::str::FromStr;

    fn match_result(
        line_number: usize,
        line_content: &str,
        context_before: &str,
        context_after: &str,
    ) -> MatchResult {
        MatchResult {
            file_path: PathBuf::from("src/file.rs"),
            line_number,
            line_content: line_content.to_string(),
            line_match: None,
            byte_offset: 0,
            context_before: context_before.to_string(),
            context_after: context_after.to_string(),
        }
    }

    #[test]
    fn test_from_matches_computes_range_from_context_strings() {
        let format = Format::from_matches(&[match_result(10, "MATCH\n", "l8\nl9\n", "l11\n")]);

        assert_eq!(format.len(), 1);
        let chunk = format.iter().next().unwrap();
        assert_eq!(chunk.path(), Path::new("src/file.rs"));
        assert_eq!((chunk.start_line(), chunk.num_lines()), (8, 4));
        assert_eq!(chunk.content(), "l8\nl9\nMATCH\nl11\n");
        // The fingerprint covers exactly the original bytes of the range.
        assert_eq!(
            chunk.fingerprint(),
            Some(Fingerprint::of(b"l8\nl9\nMATCH\nl11\n"))
        );
    }

    #[test]
    fn test_fingerprint_roundtrips_through_header() {
        let fp = Fingerprint::of(b"orig\n");
        let format = Format::new(vec![
            Chunk::from_parts("t.txt", 5, 1, "new\n").with_fingerprint(Some(fp)),
        ]);

        let output = format.to_string();
        assert_eq!(output, format!("@t.txt:5:1 #{fp}\nnew\n@@@\n"));

        let parsed = Format::from_str(&output).unwrap();
        assert_eq!(chunks(&parsed)[0].fingerprint(), Some(fp));
        assert_eq!(parsed.to_string(), output);

        // A chunk without a fingerprint serializes without the tag.
        let plain = Format::new(vec![Chunk::from_parts("t.txt", 5, 1, "new\n")]);
        assert_eq!(plain.to_string(), "@t.txt:5:1\nnew\n@@@\n");
        assert_eq!(
            chunks(&Format::from_str(&plain.to_string()).unwrap())[0].fingerprint(),
            None
        );
    }

    #[test]
    fn test_from_matches_without_context() {
        let format = Format::from_matches(&[match_result(10, "MATCH\n", "", "")]);

        assert_eq!(format.len(), 1);
        let chunk = format.iter().next().unwrap();
        assert_eq!((chunk.start_line(), chunk.num_lines()), (10, 1));
        assert_eq!(chunk.content(), "MATCH\n");
    }

    #[test]
    fn test_from_matches_shifts_match_range_past_context() {
        let mut m = match_result(2, "say hello\n", "intro\n", "");
        m.line_match = Some(4..9);

        let format = Format::from_matches(&[m]);
        let chunk = format.iter().next().unwrap();
        // "intro\n" is 6 bytes, so the range moves by 6 within the chunk content.
        assert_eq!(chunk.match_range(), Some(&(10..15)));
        assert_eq!(&chunk.content()[10..15], "hello");
    }

    /// The chunks of `f` in sorted order, indexable for assertions.
    fn chunks(f: &Format) -> Vec<&Chunk> {
        f.iter().collect()
    }

    #[test]
    fn test_chunk_new() {
        let range = LineRange::from_usize(42, 1).unwrap();
        let chunk = Chunk::new(PathBuf::from("test.txt"), range, "test content".to_string());
        assert_eq!(chunk.range(), range);
        assert_eq!(chunk.start_line(), 42);
        assert_eq!(chunk.num_lines(), 1);
        assert_eq!(chunk.content(), "test content");
        assert_eq!(chunk.path(), Path::new("test.txt"));
        assert!(!chunk.content().ends_with('\n'));
        assert_eq!(chunk.match_range(), None);
    }

    #[test]
    fn test_roundtrip_chunk_without_trailing_newline() {
        let format = Format::new(vec![Chunk::from_parts("t.txt", 5, 1, "line 5")]);

        let output = format.to_string();
        assert_eq!(output, "@t.txt:5:1\nline 5\n@@@-\n");

        let parsed = Format::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(chunks(&parsed)[0].path(), Path::new("t.txt"));
        assert_eq!(chunks(&parsed)[0].start_line(), 5);
        assert_eq!(chunks(&parsed)[0].num_lines(), 1);
        assert_eq!(chunks(&parsed)[0].content(), "line 5");
        assert!(!chunks(&parsed)[0].content().ends_with('\n'));
        // Serializing the parsed chunk reproduces the input exactly.
        assert_eq!(parsed.to_string(), output);
    }

    #[test]
    fn test_roundtrip_chunk_with_trailing_newline() {
        let format = Format::new(vec![Chunk::from_parts("t.txt", 5, 1, "line 5\n")]);

        let output = format.to_string();
        assert_eq!(output, "@t.txt:5:1\nline 5\n@@@\n");

        let parsed = Format::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(chunks(&parsed)[0].content(), "line 5\n");
        assert!(chunks(&parsed)[0].content().ends_with('\n'));
        assert_eq!(parsed.to_string(), output);
    }

    #[test]
    fn test_format_to_string_single_chunk() {
        let format = Format::new(vec![Chunk::from_parts(
            "src/main.rs",
            10,
            3,
            "fn main() {\n    println!(\"Hello\");\n}",
        )]);

        let output = format.to_string();
        assert!(output.contains("@src/main.rs:10:3"));
        assert!(output.contains("fn main()"));
        assert!(output.contains("@@@"));
    }

    #[test]
    fn test_format_to_string_multiple_chunks() {
        let format = Format::new(vec![
            Chunk::from_parts("test.txt", 5, 1, "line 5"),
            Chunk::from_parts("test.txt", 10, 1, "line 10"),
        ]);

        let output = format.to_string();
        assert!(output.contains("@test.txt:5:1"));
        assert!(output.contains("@test.txt:10:1"));
        assert!(output.contains("line 5"));
        assert!(output.contains("line 10"));
        // Should have two @@@ delimiters
        assert_eq!(output.matches("@@@").count(), 2);
    }

    #[test]
    fn test_format_to_string_with_special_chars() {
        let format = Format::new(vec![Chunk::from_parts(
            "test.txt",
            1,
            1,
            "user@domain.com\\path",
        )]);

        let output = format.to_string();
        // Mid-line `@` and `\` are not syntax, so they are written verbatim.
        assert!(output.contains("\nuser@domain.com\\path\n"));
    }

    #[test]
    fn test_format_roundtrip() {
        let original = Format::new(vec![
            Chunk::from_parts("src/lib.rs", 1, 3, "pub fn test() {\n    // test\n}\n"),
            Chunk::from_parts("src/lib.rs", 20, 1, "fn another() {}\n"),
        ]);

        let serialized = original.to_string();
        let deserialized = Format::from_str(&serialized).unwrap();

        assert_eq!(deserialized.len(), original.len());
        for (i, chunk) in deserialized.iter().enumerate() {
            assert_eq!(chunk.path(), chunks(&original)[i].path());
            assert_eq!(chunk.start_line(), chunks(&original)[i].start_line());
            assert_eq!(chunk.num_lines(), chunks(&original)[i].num_lines());
            assert_eq!(chunk.content(), chunks(&original)[i].content());
        }
    }

    #[test]
    fn test_format_roundtrip_with_special_chars() {
        let original = Format::new(vec![Chunk::from_parts(
            "test.txt",
            1,
            2,
            "@ symbol and \\ backslash\nuser@email.com\\path\\to\\file\n",
        )]);

        let serialized = original.to_string();
        let deserialized = Format::from_str(&serialized).unwrap();

        assert_eq!(
            chunks(&deserialized)[0].content(),
            chunks(&original)[0].content()
        );
    }

    #[test]
    fn test_file_chunks_multiple_files() {
        // This test verifies the fix for the bug where file_chunks would panic
        // when grouping chunks from multiple files. The bug was on line 232 where
        // it used chunk.path() instead of start_chunk.path() when transitioning between files.
        let format = Format::new(vec![
            Chunk::from_parts("src/main.rs", 1, 2, "fn main() {\n    println!(\"Hello\");"),
            Chunk::from_parts("src/main.rs", 10, 1, "// comment"),
            Chunk::from_parts("src/lib.rs", 5, 3, "pub fn test() {\n    // test\n}"),
            Chunk::from_parts("src/lib.rs", 20, 2, "pub fn another() {\n}"),
            Chunk::from_parts("tests/integration.rs", 1, 1, "#[test]"),
        ]);

        let file_chunks = format.file_chunks();

        // Should have 3 different files
        assert_eq!(file_chunks.len(), 3);

        // Verify first file (src/lib.rs comes first alphabetically after sorting)
        assert_eq!(file_chunks[0].0, Path::new("src/lib.rs"));
        assert_eq!(file_chunks[0].1.len(), 2);
        assert_eq!(file_chunks[0].1[0].start_line(), 5);
        assert_eq!(file_chunks[0].1[1].start_line(), 20);

        // Verify second file (src/main.rs)
        assert_eq!(file_chunks[1].0, Path::new("src/main.rs"));
        assert_eq!(file_chunks[1].1.len(), 2);
        assert_eq!(file_chunks[1].1[0].start_line(), 1);
        assert_eq!(file_chunks[1].1[1].start_line(), 10);

        // Verify third file (tests/integration.rs)
        assert_eq!(file_chunks[2].0, Path::new("tests/integration.rs"));
        assert_eq!(file_chunks[2].1.len(), 1);
        assert_eq!(file_chunks[2].1[0].start_line(), 1);

        // Verify that all chunks in each group have the correct path
        for (path, chunks) in file_chunks {
            for chunk in chunks {
                assert_eq!(
                    chunk.path(),
                    path,
                    "Chunk path mismatch: expected {:?}, got {:?}",
                    path,
                    chunk.path()
                );
            }
        }
    }

    #[test]
    fn test_format_new_sorts_by_path_then_range() {
        // Constructed out of order: b:1, a:5, a:1 → must come out a:1, a:5, b:1.
        let format = Format::new(vec![
            Chunk::from_parts("b", 1, 1, "b1\n"),
            Chunk::from_parts("a", 5, 1, "a5\n"),
            Chunk::from_parts("a", 1, 1, "a1\n"),
        ]);

        let order: Vec<(&Path, usize)> =
            format.iter().map(|c| (c.path(), c.start_line())).collect();
        assert_eq!(
            order,
            vec![
                (Path::new("a"), 1),
                (Path::new("a"), 5),
                (Path::new("b"), 1),
            ]
        );
        assert_eq!(format.len(), 3);
        assert!(!format.is_empty());
        assert!(Format::new(vec![]).is_empty());
    }

    #[test]
    fn test_file_chunks_groups_without_mutation() {
        let format = Format::new(vec![
            Chunk::from_parts("b", 1, 1, "b1\n"),
            Chunk::from_parts("a", 5, 1, "a5\n"),
            Chunk::from_parts("a", 1, 1, "a1\n"),
        ]);

        // `file_chunks` takes `&self`: two calls on the same immutable value.
        let first = format.file_chunks();
        let second = format.file_chunks();

        assert_eq!(first.len(), 2);
        assert_eq!(first[0].0, Path::new("a"));
        assert_eq!(first[0].1.len(), 2);
        assert_eq!(first[0].1[0].start_line(), 1);
        assert_eq!(first[0].1[1].start_line(), 5);
        assert_eq!(first[1].0, Path::new("b"));
        assert_eq!(first[1].1.len(), 1);
        assert_eq!(first[1].1[0].start_line(), 1);

        // Identical result the second time: same paths, same lengths, same lines.
        let shape = |groups: &[(&Path, &[Chunk])]| -> Vec<(PathBuf, Vec<usize>)> {
            groups
                .iter()
                .map(|(p, cs)| (p.to_path_buf(), cs.iter().map(|c| c.start_line()).collect()))
                .collect()
        };
        assert_eq!(shape(&first), shape(&second));
    }
}
