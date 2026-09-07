use super::escaping::unescape_line;
use super::fingerprint::Fingerprint;
use super::range::LineRange;
use super::types::{Chunk, Format, FormatError};
use nom::combinator::opt;
use nom::{
    IResult, Parser,
    branch::alt,
    bytes::complete::{tag, take_till1, take_while},
    character::complete::char,
    combinator::recognize,
    error::{ErrorKind, ParseError as NomParseError},
    multi::many0,
    sequence::preceded,
};
use std::num::NonZeroUsize;
use std::path::PathBuf;

fn space0(input: &str) -> ParseResult<'_, &str> {
    take_while(|c| " \t\r".contains(c))(input)
}

/// Parse a newline character (only '\n', not '\r\n')
fn newline(input: &str) -> ParseResult<'_, char> {
    char('\n')(input)
}

/// Parse everything except newline (only stops at '\n', not '\r')
fn not_newline(input: &str) -> ParseResult<'_, &str> {
    take_while(|c| c != '\n')(input)
}

/// Custom nom error type that carries context for generating `FormatError`
#[derive(Debug, Clone)]
pub(super) struct ParserError {
    suffix_len: usize,
    pub(super) kind: ParserErrorKind,
}

#[derive(Debug, Clone)]
pub(super) enum ParserErrorKind {
    InvalidDelimiter,
    InvalidLineNumber {
        value: String,
        len: usize,
    },
    InvalidNumLines {
        value: String,
        len: usize,
    },
    InvalidFingerprint {
        value: String,
        len: usize,
    },
    MissingEndDelimiter {
        start_suffix_len: usize,
        start_len: usize,
    },
    /// A content line starts with an unescaped `@` that is not the `@@@` terminator.
    UnescapedAtLine {
        start_suffix_len: usize,
        start_len: usize,
    },
    Nom {
        #[allow(dead_code)]
        kind: ErrorKind,
    },
}

impl<'a> NomParseError<&'a str> for ParserError {
    fn from_error_kind(input: &'a str, kind: ErrorKind) -> Self {
        ParserError {
            suffix_len: input.len(),
            kind: ParserErrorKind::Nom { kind },
        }
    }

    fn append(_input: &'a str, _kind: ErrorKind, other: Self) -> Self {
        other
    }
}

impl ParserError {
    pub(super) fn new(input: &str, kind: ParserErrorKind) -> Self {
        ParserError {
            suffix_len: input.len(),
            kind,
        }
    }

    pub(super) fn into_format_error(self, source: &str) -> FormatError {
        let src = source.to_string();
        match self.kind {
            ParserErrorKind::InvalidDelimiter => {
                let offset = source.len() - self.suffix_len;
                let end = source[offset..]
                    .find('\n')
                    .map_or(source.len(), |i| offset + i);
                FormatError::InvalidDelimiter {
                    src,
                    span: (offset, end - offset).into(),
                }
            }
            ParserErrorKind::InvalidLineNumber { value, len } => {
                // `suffix_len` is measured after the numeric segment was consumed,
                // so back up by its length to point the label at the segment itself.
                let offset = source.len() - self.suffix_len - len;
                FormatError::InvalidLineNumber {
                    value,
                    src,
                    span: (offset, len).into(),
                }
            }
            ParserErrorKind::InvalidNumLines { value, len } => {
                let offset = source.len() - self.suffix_len - len;
                FormatError::InvalidNumLines {
                    value,
                    src,
                    span: (offset, len).into(),
                }
            }
            ParserErrorKind::InvalidFingerprint { value, len } => {
                let offset = source.len() - self.suffix_len - len;
                FormatError::InvalidFingerprint {
                    value,
                    src,
                    span: (offset, len).into(),
                }
            }
            ParserErrorKind::MissingEndDelimiter {
                start_len,
                start_suffix_len,
            } => FormatError::MissingEndDelimiter {
                src: src.clone(),
                start_span: (source.len() - start_suffix_len, start_len).into(),
                eof_span: (src.len().saturating_sub(1), 1).into(),
            },
            ParserErrorKind::UnescapedAtLine {
                start_len,
                start_suffix_len,
            } => {
                let offset = source.len() - self.suffix_len;
                let end = source[offset..]
                    .find('\n')
                    .map_or(source.len(), |i| offset + i);
                FormatError::UnescapedAtLine {
                    src,
                    start_span: (source.len() - start_suffix_len, start_len).into(),
                    span: (offset, end - offset).into(),
                }
            }
            ParserErrorKind::Nom { .. } => FormatError::NoChunks { src },
        }
    }
}

type ParseResult<'a, T> = IResult<&'a str, T, ParserError>;

fn invalid_delimiter_error(input: &str) -> ParserError {
    ParserError::new(input, ParserErrorKind::InvalidDelimiter {})
}

/// Parse one numeric header segment (`<line>` or `<numlines>`) as a non-zero integer.
///
/// Zero is rejected exactly like a non-numeric value: `NonZeroUsize`'s `FromStr`
/// fails on `"0"`, so both cases produce the same `Failure` whose span (computed by
/// [`ParserError::into_format_error`] from `input`, the remainder *after* the
/// segment, and `len`) points at the offending segment.
fn parse_nonzero_segment<F>(
    segment: &str,
    input: &str,
    err_builder: F,
) -> Result<NonZeroUsize, nom::Err<ParserError>>
where
    F: FnOnce(String, usize) -> ParserErrorKind,
{
    segment.parse::<NonZeroUsize>().map_err(|_| {
        nom::Err::Failure(ParserError::new(
            input,
            err_builder(segment.to_string(), segment.len()),
        ))
    })
}

/// Main entry point - parses the entire format
pub fn parse_format(src: &str) -> Result<Format, FormatError> {
    let (format, _) = parse_format_with_spans(src)?;
    Ok(format)
}

/// Where a chunk lives in the source text it was parsed from.
///
/// `tag` is the byte range after `<numlines>` up to the end of the header line
/// (excluding the line ending): the ` #xxxxxxxx` fingerprint and any surrounding
/// whitespace, or an empty range where a tag could be inserted. `body` runs from
/// the first content byte through the `@@@` / `@@@-` terminator token (excluding
/// any trailing text on that line). Replacing `tag` with ` #<fingerprint>` and
/// `body` with [`super::types::chunk_body`] rewrites a chunk in place and leaves
/// every other byte of the file untouched. Spans are returned in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSpans {
    pub path: PathBuf,
    pub range: LineRange,
    pub fingerprint: Option<Fingerprint>,
    pub tag: std::ops::Range<usize>,
    pub body: std::ops::Range<usize>,
}

/// [`parse_format`], also returning one [`ChunkSpans`] per chunk in source order
/// (the `Format` itself is sorted, so the two are not index-aligned; match them on
/// `(path, range)`).
pub fn parse_format_with_spans(src: &str) -> Result<(Format, Vec<ChunkSpans>), FormatError> {
    let src_len = src.len();
    let chunk_with_spans = |input| chunk_parser(src_len, input);

    // Skip leading whitespace/comments
    let (input, ()) = skip_whitespace_and_comments(src).map_err(|e| match e {
        nom::Err::Error(e) | nom::Err::Failure(e) => e.into_format_error(src),
        nom::Err::Incomplete(_) => FormatError::NoChunks {
            src: src.to_string(),
        },
    })?;

    // Parse all chunks
    let (_, parsed) = many0(preceded(skip_whitespace_and_comments, chunk_with_spans))
        .parse(input)
        .map_err(|e| match e {
            nom::Err::Error(e) | nom::Err::Failure(e) => e.into_format_error(src),
            nom::Err::Incomplete(_) => FormatError::NoChunks {
                src: src.to_string(),
            },
        })?;

    let (chunks, spans): (Vec<Chunk>, Vec<ChunkSpans>) = parsed.into_iter().unzip();

    // `Format::new` sorts by (path, range), so a parsed format is sorted by construction.
    let format = Format::new(chunks);
    if format.is_empty() {
        return Err(FormatError::NoChunks {
            src: src.to_string(),
        });
    }

    Ok((format, spans))
}

/// Returns a parser that consumes a chunk with context for better diagnostics.
///
/// `src_len` is the length of the whole source text, used to turn the parser's
/// remaining-input lengths into byte offsets for the returned [`ChunkSpans`].
fn chunk_parser(src_len: usize, input: &str) -> ParseResult<'_, (Chunk, ChunkSpans)> {
    let chunk_start_suffix_len = input.len();
    let header_len = input.split_inclusive('\n').next().map_or(0, str::len);

    let (input, header) = start_delimiter(input)?;
    let body_start = src_len - input.len();

    let (input, mut content) = chunk_content(chunk_start_suffix_len, header_len)(input)?;

    let (input, (dash_terminated, after_terminator_len)) = parse_end_delimiter_nom(input)?;
    if dash_terminated && content.ends_with('\n') {
        content.pop();
    }

    let Header {
        path,
        range,
        fingerprint,
        slot_suffix_lens: (tag_start, tag_end),
    } = header;
    let spans = ChunkSpans {
        path: path.clone(),
        range,
        fingerprint,
        tag: (src_len - tag_start)..(src_len - tag_end),
        body: body_start..(src_len - after_terminator_len),
    };

    // `@@@-` is not stored on the chunk: the trailing newline was stripped above, so
    // the serializer derives the terminator from `content.ends_with('\n')`.
    Ok((
        input,
        (
            Chunk::new(path, range, content).with_fingerprint(fingerprint),
            spans,
        ),
    ))
}

/// A parsed chunk header.
struct Header {
    path: PathBuf,
    range: LineRange,
    fingerprint: Option<Fingerprint>,
    /// Remaining-input lengths at the start and end of the fingerprint slot: just
    /// after `numlines`, and at the end of the header line before its `\r?\n`.
    slot_suffix_lens: (usize, usize),
}

/// Parser for the start delimiter: `@path:line:numlines[ #fingerprint]`
///
/// Both numbers must be non-zero; `@f:0:1` and `@f:1:0` are failures whose spans
/// label the zero. Whitespace may follow `numlines` and the optional fingerprint;
/// any other trailing text is an invalid delimiter whose span labels that text.
fn start_delimiter(input: &str) -> ParseResult<'_, Header> {
    // Use closures to lazily construct errors with the correct suffix length
    let invalid_failure = || nom::Err::Failure(invalid_delimiter_error(input));
    let invalid_error = || nom::Err::Error(invalid_delimiter_error(input));

    let (input, _) = char('@')(input).map_err(|_: nom::Err<ParserError>| invalid_error())?;
    let (input, path_str) = take_till1(|c| c == ':' || c == '\n')(input)
        .map_err(|_: nom::Err<ParserError>| invalid_failure())?;
    let (input, _) = char(':')(input).map_err(|_: nom::Err<ParserError>| invalid_failure())?;

    let (input, line_str) = take_till1(|c| c == ':' || c == '\n')(input)
        .map_err(|_: nom::Err<ParserError>| invalid_failure())?;
    if !input.starts_with(':') {
        return Err(invalid_failure());
    }
    let line_number = parse_nonzero_segment(line_str, input, |value, len| {
        ParserErrorKind::InvalidLineNumber { value, len }
    })?;

    let (input, _) = char(':')(input).map_err(|_: nom::Err<ParserError>| invalid_failure())?;

    let (input, numlines_str) = take_till1(|c: char| c.is_whitespace() || c == '#')(input)
        .map_err(|_: nom::Err<ParserError>| invalid_failure())?;
    let numlines = parse_nonzero_segment(numlines_str, input, |value, len| {
        ParserErrorKind::InvalidNumLines { value, len }
    })?;
    // Everything from here to the line ending is the fingerprint slot.
    let slot_input = input;

    let (input, _) = space0(input)?;

    // Optional ` #xxxxxxxx` fingerprint of the original lines.
    let (input, fingerprint) = match char::<_, ParserError>('#')(input) {
        Ok((rest, _)) => {
            let (rest, hex) = take_while(|c: char| !c.is_whitespace())(rest)?;
            let fingerprint = hex.parse::<Fingerprint>().map_err(|_| {
                nom::Err::Failure(ParserError::new(
                    rest,
                    ParserErrorKind::InvalidFingerprint {
                        value: hex.to_string(),
                        len: hex.len(),
                    },
                ))
            })?;
            (rest, Some(fingerprint))
        }
        Err(_) => (input, None),
    };

    let (input, _) = space0(input)?;
    // `space0` also eats a `\r`; keep it out of the slot so a CRLF header stays CRLF.
    let slot_text = &slot_input[..slot_input.len() - input.len()];
    let slot_end = input.len() + usize::from(slot_text.ends_with('\r'));
    // Anything else before the end of the header line is an error labelled at
    // that text (e.g. `@f:1:1 oops`).
    let (input, _) = newline(input)
        .map_err(|_: nom::Err<ParserError>| nom::Err::Failure(invalid_delimiter_error(input)))?;

    Ok((
        input,
        Header {
            path: PathBuf::from(path_str),
            range: LineRange::new(line_number, numlines),
            fingerprint,
            slot_suffix_lens: (slot_input.len(), slot_end),
        },
    ))
}

/// Parser factory for chunk content until the @@@ end delimiter.
///
/// Content is collected line by line and each line is unescaped as it is read
/// (see [`super::escaping`]). A line that starts with `@` is never content: it is
/// either the `@@@` terminator or a mistake (a forgotten `@@@`, or an unescaped
/// `@decorator`), which is reported with spans on both the chunk header and the
/// offending line rather than silently swallowed into the chunk.
fn chunk_content<'a>(
    chunk_start_suffix_len: usize,
    header_len: usize,
) -> impl Fn(&'a str) -> ParseResult<'a, String> {
    move |mut current| {
        let mut content = String::new();

        loop {
            if current.starts_with("@@@") {
                return Ok((current, content));
            }

            if current.is_empty() {
                return Err(nom::Err::Failure(ParserError::new(
                    current,
                    ParserErrorKind::MissingEndDelimiter {
                        start_len: header_len,
                        start_suffix_len: chunk_start_suffix_len,
                    },
                )));
            }

            if current.starts_with('@') {
                return Err(nom::Err::Failure(ParserError::new(
                    current,
                    ParserErrorKind::UnescapedAtLine {
                        start_len: header_len,
                        start_suffix_len: chunk_start_suffix_len,
                    },
                )));
            }

            let (rest, line) = not_newline(current)?;

            // Add the line content, minus the one `\` that protected its start.
            content.push_str(unescape_line(line));

            current = match newline(rest) {
                Ok((rest, _)) => {
                    // Add the newline to content to preserve line endings
                    content.push('\n');
                    rest
                }
                Err(_) => rest,
            };
        }
    }
}

/// Parse end delimiter: @@@ or @@@- (no newline at end of file)
/// Allows any text after @@@ until the end of the line (which is ignored).
///
/// Also returns the remaining-input length just after the `@@@` / `@@@-` token, so
/// the caller can locate the end of the chunk body in the source.
fn parse_end_delimiter_nom(input: &str) -> ParseResult<'_, (bool, usize)> {
    let (input, _) = tag("@@@").parse(input)?;

    let (input, opt_tag) = opt(tag("-")).parse(input)?;
    let dash_terminated = opt_tag.is_some();
    let after_terminator_len = input.len();

    // Allow optional text after @@@ until end of line
    let (input, _) = opt(not_newline).parse(input)?;
    let (input, _) = alt((recognize(newline), recognize(nom::combinator::eof))).parse(input)?;
    Ok((input, (dash_terminated, after_terminator_len)))
}

/// Skip whitespace and comment lines
fn skip_whitespace_and_comments(input: &str) -> ParseResult<'_, ()> {
    let (input, _) = many0(
        // Skip comment lines (non-@ lines)
        recognize((
            nom::combinator::peek(nom::combinator::not(char('@'))),
            not_newline,
            newline,
        )),
    )
    .parse(input)?;

    Ok((input, ()))
}

#[cfg(test)]
mod tests {
    use crate::format::parse::start_delimiter;

    use super::super::types::{Chunk, Format, FormatError};
    use std::path::PathBuf;
    use std::str::FromStr;

    /// The chunks of `f` in sorted order, indexable for assertions.
    fn chunks(f: &Format) -> Vec<&Chunk> {
        f.iter().collect()
    }

    #[test]
    fn test_format_from_str_single_chunk() {
        let input = "@src/main.rs:10:2\nfn main() {\n    println!(\"Hello\");\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.len(), 1);
        assert_eq!(chunks(&format)[0].path(), PathBuf::from("src/main.rs"));
        assert_eq!(chunks(&format)[0].start_line(), 10);
        assert_eq!(chunks(&format)[0].num_lines(), 2);
        assert_eq!(
            chunks(&format)[0].content(),
            "fn main() {\n    println!(\"Hello\");\n"
        );
    }

    #[test]
    fn test_format_from_str_multiple_chunks() {
        let input = r"@test.txt:5:1
line 5
@@@

@test.txt:10:1
line 10
@@@
";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.len(), 2);
        assert_eq!(chunks(&format)[0].path(), PathBuf::from("test.txt"));
        assert_eq!(chunks(&format)[0].start_line(), 5);
        assert_eq!(chunks(&format)[0].num_lines(), 1);
        assert_eq!(chunks(&format)[0].content(), "line 5\n");
        assert_eq!(chunks(&format)[1].path(), PathBuf::from("test.txt"));
        assert_eq!(chunks(&format)[1].start_line(), 10);
        assert_eq!(chunks(&format)[1].num_lines(), 1);
        assert_eq!(chunks(&format)[1].content(), "line 10\n");
    }

    #[test]
    fn test_format_from_str_with_comments() {
        let input = r"This is a comment at the start

@test.txt:1:1
content
@@@

This is a comment between chunks

@test.txt:5:1
more content
@@@
";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.len(), 2);
        assert_eq!(chunks(&format)[0].content(), "content\n");
        assert_eq!(chunks(&format)[1].content(), "more content\n");
    }

    #[test]
    fn test_format_from_str_escapes_only_line_starts() {
        // Mid-line `@` and `\` are verbatim; only a leading `\@` / `\\` is unescaped.
        let input =
            "@test.txt:1:4\nuser@domain.com\\path\n\\@dataclass\n\\\\server\n\\begin\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(
            chunks(&format)[0].content(),
            "user@domain.com\\path\n@dataclass\n\\server\n\\begin\n"
        );
    }

    #[test]
    fn test_unescaped_at_line_inside_chunk_is_an_error_with_spans() {
        // The user deleted the `@@@` between two chunks: the second header must not
        // become content of the first.
        let input = "@a.rs:2:1\n    // one\n\n@a.rs:8:1\n    // two\n@@@\n";
        match Format::from_str(input).unwrap_err() {
            FormatError::UnescapedAtLine {
                start_span, span, ..
            } => {
                assert_eq!(start_span.offset(), 0);
                let second = input.find("@a.rs:8:1").unwrap();
                assert_eq!(span.offset(), second);
                assert_eq!(span.len(), "@a.rs:8:1".len());
            }
            other => panic!("Expected UnescapedAtLine, got {other:?}"),
        }

        // An unescaped decorator is the same mistake.
        let input = "@a.py:1:1\n@property\n@@@\n";
        assert!(matches!(
            Format::from_str(input).unwrap_err(),
            FormatError::UnescapedAtLine { .. }
        ));
    }

    #[test]
    fn test_format_from_str_invalid_delimiter() {
        let input = "@test.txt:invalid\ncontent\n@@@\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            FormatError::InvalidDelimiter { .. }
        ));
    }

    #[test]
    fn test_format_from_str_missing_end_delimiter() {
        let input = "@test.txt:1:1\ncontent without end delimiter\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            FormatError::MissingEndDelimiter { .. }
        ));
    }

    #[test]
    fn test_format_from_str_empty() {
        let input = "";
        let result = Format::from_str(input);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FormatError::NoChunks { .. }));
    }

    #[test]
    fn test_format_error_invalid_line_number() {
        let input = "@test.txt:not_a_number:5\ncontent\n@@@\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::InvalidLineNumber { value, span, .. } => {
                assert_eq!(value, "not_a_number");
                assert_eq!(span.offset(), input.find("not_a_number").unwrap());
                assert_eq!(span.len(), "not_a_number".len());
            }
            _ => panic!("Expected InvalidLineNumber error"),
        }
    }

    #[test]
    fn test_format_error_invalid_numlines() {
        let input = "@test.txt:10:invalid\ncontent\n@@@\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::InvalidNumLines { value, span, .. } => {
                assert_eq!(value, "invalid");
                assert_eq!(span.offset(), input.find("invalid").unwrap());
                assert_eq!(span.len(), "invalid".len());
            }
            _ => panic!("Expected InvalidNumLines error"),
        }
    }

    #[test]
    fn test_zero_line_number_is_parse_error_with_span() {
        // Line numbers are 1-indexed: a zero line is a parse error whose span labels
        // the "0" (offset 3 in "@f:0:1"), the same shape as a non-numeric value.
        let input = "@f:0:1\nx\n@@@\n";
        match Format::from_str(input).unwrap_err() {
            FormatError::InvalidLineNumber { value, span, .. } => {
                assert_eq!(value, "0");
                assert_eq!(span.offset(), 3);
                assert_eq!(span.len(), 1);
            }
            other => panic!("Expected InvalidLineNumber error, got {other:?}"),
        }
    }

    #[test]
    fn test_zero_numlines_is_parse_error_with_span() {
        // A chunk must cover at least one line: zero numlines is a parse error whose
        // span labels the "0" (offset 5 in "@f:1:0").
        let input = "@f:1:0\nx\n@@@\n";
        match Format::from_str(input).unwrap_err() {
            FormatError::InvalidNumLines { value, span, .. } => {
                assert_eq!(value, "0");
                assert_eq!(span.offset(), 5);
                assert_eq!(span.len(), 1);
            }
            other => panic!("Expected InvalidNumLines error, got {other:?}"),
        }
    }

    /// This test demonstrates the beautiful error messages from miette.
    /// Run with: cargo test `test_format_error_display` -- --nocapture
    #[test]
    fn test_format_error_display() {
        use miette::Report;

        let test_cases = vec![
            (
                "Invalid line number",
                "@src/main.rs:not_a_number:10\nfn main() {}\n@@@\n",
            ),
            ("Invalid delimiter", "@src/main.rs:10\nfn main() {}\n@@@\n"),
            ("Missing end delimiter", "@src/main.rs:10:1\nfn main() {}\n"),
            (
                "Unescaped @ line",
                "@src/main.rs:10:1\nfn main() {}\n@src/main.rs:20:1\nx\n@@@\n",
            ),
        ];

        for (name, input) in test_cases {
            println!("\n=== Test case: {name} ===");
            match Format::from_str(input) {
                Ok(_) => println!("Unexpectedly succeeded!"),
                Err(e) => {
                    let report = Report::new(e);
                    println!("{report:?}");
                }
            }
        }
    }

    #[test]
    fn test_format_from_str_with_trailing_text_after_delimiter() {
        // End delimiter can have trailing text/comments after @@@
        let input = "@test.txt:1:1\ncontent\n@@@ this is a comment\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.len(), 1);
        assert_eq!(chunks(&format)[0].content(), "content\n");
    }

    #[test]
    fn test_format_from_str_multiple_chunks_with_trailing_text() {
        let input = r"@test.txt:5:1
line 5
@@@ comment here

@test.txt:10:1
line 10
@@@ another comment
";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.len(), 2);
        assert_eq!(chunks(&format)[0].content(), "line 5\n");
        assert_eq!(chunks(&format)[1].content(), "line 10\n");
    }

    #[test]
    fn test_format_preserves_crlf() {
        // Test that Windows line endings (\r\n) are preserved in content
        let input = "@test.txt:1:2\r\nline1\r\nline2\r\n@@@\r\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.len(), 1);
        assert_eq!(chunks(&format)[0].path(), PathBuf::from("test.txt"));
        assert_eq!(chunks(&format)[0].start_line(), 1);
        assert_eq!(chunks(&format)[0].num_lines(), 2);
        // The \r should be preserved as part of the line content
        assert_eq!(chunks(&format)[0].content(), "line1\r\nline2\r\n");
    }

    #[test]
    fn test_format_crlf() {
        // Test that Windows line endings (\r\n) are preserved in content
        let input = "@test.txt:1:2\r\n";

        let (rest, header) = start_delimiter(input).unwrap();
        assert_eq!(rest, "");
        // The slot ends before the `\r`, so rewriting the tag keeps the CRLF.
        assert_eq!(header.slot_suffix_lens, ("\r\n".len(), "\r\n".len()));
    }

    #[test]
    fn test_header_fingerprint_and_trailing_whitespace() {
        let fp = "@f:3:2 #deadbeef\nx\n@@@\n";
        let format = Format::from_str(fp).unwrap();
        assert_eq!(
            chunks(&format)[0].fingerprint(),
            Some("deadbeef".parse().unwrap())
        );
        assert_eq!(
            (
                chunks(&format)[0].start_line(),
                chunks(&format)[0].num_lines()
            ),
            (3, 2)
        );

        // Trailing whitespace after numlines (or the fingerprint) is not an error.
        let ws = "@f:3:2   \nx\n@@@\n";
        assert_eq!(chunks(&Format::from_str(ws).unwrap())[0].num_lines(), 2);
        let ws_fp = "@f:3:2 #deadbeef \t\nx\n@@@\n";
        assert!(Format::from_str(ws_fp).is_ok());
        // Fingerprint directly after numlines, no space.
        let tight = "@f:3:2#deadbeef\nx\n@@@\n";
        assert!(Format::from_str(tight).is_ok());
    }

    #[test]
    fn test_header_bad_fingerprint_and_trailing_garbage_have_spans() {
        let input = "@f:3:2 #dead\nx\n@@@\n";
        match Format::from_str(input).unwrap_err() {
            FormatError::InvalidFingerprint { value, span, .. } => {
                assert_eq!(value, "dead");
                assert_eq!(span.offset(), input.find("dead").unwrap());
                assert_eq!(span.len(), 4);
            }
            other => panic!("Expected InvalidFingerprint, got {other:?}"),
        }

        let input = "@f:3:2 oops\nx\n@@@\n";
        match Format::from_str(input).unwrap_err() {
            FormatError::InvalidDelimiter { span, .. } => {
                assert_eq!(span.offset(), input.find("oops").unwrap());
                assert_eq!(span.len(), 4);
            }
            other => panic!("Expected InvalidDelimiter, got {other:?}"),
        }
    }

    #[test]
    fn test_chunk_spans_cover_the_tag_and_the_body() {
        use super::parse_format_with_spans;

        // Spans come back in source order even though the Format sorts.
        let src = "hi\n@z:1:1 #deadbeef  \nx\n@@@ trailing\n@a:2:3\ny\n\\@e\n@@@\n@m:4:1#cafebabe\r\nz\r\n@@@-\n";
        let (format, spans) = parse_format_with_spans(src).unwrap();
        assert_eq!(chunks(&format)[0].path(), PathBuf::from("a"));
        let tags: Vec<_> = spans
            .iter()
            .map(|s| (s.path.clone(), &src[s.tag.clone()], &src[s.body.clone()]))
            .collect();
        assert_eq!(
            tags,
            vec![
                (PathBuf::from("z"), " #deadbeef  ", "x\n@@@"),
                (PathBuf::from("a"), "", "y\n\\@e\n@@@"),
                (PathBuf::from("m"), "#cafebabe", "z\r\n@@@-"),
            ]
        );
        assert_eq!(
            spans[1].tag.start,
            src.find("@a:2:3").unwrap() + "@a:2:3".len()
        );
        assert_eq!(spans[0].fingerprint, Some("deadbeef".parse().unwrap()));
        assert_eq!(spans[1].fingerprint, None);
        assert_eq!(spans[2].range.start(), 4);

        // An untagged CRLF header: the empty tag slot sits before the `\r`.
        let src = "@f:1:1\r\nx\r\n@@@\n";
        let (_, spans) = parse_format_with_spans(src).unwrap();
        assert_eq!(spans[0].tag, "@f:1:1".len().."@f:1:1".len());
        assert_eq!(&src[spans[0].body.clone()], "x\r\n@@@");
    }

    #[test]
    fn test_format_mixed_line_endings() {
        // Test mixed line endings - some CRLF, some LF
        let input = "@test.txt:1:3\r\nline1\r\nline2\nline3\r\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.len(), 1);
        // Each line should preserve its original line ending
        assert_eq!(chunks(&format)[0].content(), "line1\r\nline2\nline3\r\n");
    }
}
