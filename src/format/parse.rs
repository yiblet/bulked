use super::escaping::unescape_content;
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
    MissingEndDelimiter {
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
                let offset = source.len() - self.suffix_len;
                FormatError::InvalidLineNumber {
                    value,
                    src,
                    span: (offset, len).into(),
                }
            }
            ParserErrorKind::InvalidNumLines { value, len } => {
                let offset = source.len() - self.suffix_len;
                FormatError::InvalidNumLines {
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
            ParserErrorKind::Nom { .. } => FormatError::NoChunks { src },
        }
    }
}

type ParseResult<'a, T> = IResult<&'a str, T, ParserError>;

fn invalid_delimiter_error(input: &str) -> ParserError {
    ParserError::new(input, ParserErrorKind::InvalidDelimiter {})
}

fn parse_usize_segment<F>(
    segment: &str,
    input: &str,
    err_builder: F,
) -> Result<usize, nom::Err<ParserError>>
where
    F: FnOnce(String, usize) -> ParserErrorKind,
{
    segment.parse::<usize>().map_err(|_| {
        nom::Err::Failure(ParserError::new(
            input,
            err_builder(segment.to_string(), segment.len()),
        ))
    })
}

/// Main entry point - parses the entire format
pub fn parse_format(src: &str) -> Result<Format, FormatError> {
    // Skip leading whitespace/comments
    let (input, ()) = skip_whitespace_and_comments(src).map_err(|e| match e {
        nom::Err::Error(e) | nom::Err::Failure(e) => e.into_format_error(src),
        nom::Err::Incomplete(_) => FormatError::NoChunks {
            src: src.to_string(),
        },
    })?;

    // Parse all chunks
    let (_, chunks) = many0(preceded(skip_whitespace_and_comments, chunk_parser)).parse(input).map_err(
        |e| match e {
            nom::Err::Error(e) | nom::Err::Failure(e) => e.into_format_error(src),
            nom::Err::Incomplete(_) => FormatError::NoChunks {
                src: src.to_string(),
            },
        },
    )?;

    if chunks.is_empty() {
        return Err(FormatError::NoChunks {
            src: src.to_string(),
        });
    }

    Ok(Format(chunks))
}

/// Returns a parser that consumes a chunk with context for better diagnostics.
fn chunk_parser(input: &str) -> ParseResult<'_, Chunk> {
    let chunk_start_suffix_len = input.len();
    let header_len = input.split_inclusive('\n').next().map_or(0, str::len);

    let (input, (path, line_number, numlines)) = start_delimiter(input)?;

    let (input, mut content) = chunk_content(chunk_start_suffix_len, header_len)(input)?;

    let (input, no_newline_eol) = parse_end_delimiter_nom(input)?;
    if no_newline_eol && content.ends_with('\n') {
        content.pop();
    }

    let unescaped_content = unescape_content(&content);
    Ok((
        input,
        Chunk::new(
            path,
            line_number,
            numlines,
            unescaped_content,
            no_newline_eol,
        ),
    ))
}

/// Parser for the start delimiter: @path:line:numlines
fn start_delimiter(input: &str) -> ParseResult<'_, (PathBuf, usize, usize)> {
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
    let line_number = parse_usize_segment(line_str, input, |value, len| {
        ParserErrorKind::InvalidLineNumber { value, len }
    })?;

    let (input, _) = char(':')(input).map_err(|_: nom::Err<ParserError>| invalid_failure())?;

    let (input, numlines_str) = take_till1(|c| c == '\n' || c == '\r')(input)
        .map_err(|_: nom::Err<ParserError>| invalid_failure())?;
    let numlines = parse_usize_segment(numlines_str, input, |value, len| {
        ParserErrorKind::InvalidNumLines { value, len }
    })?;

    let (input, _) = space0(input)?;
    let (input, _) = newline(input)?;

    Ok((input, (PathBuf::from(path_str), line_number, numlines)))
}

/// Parser factory for chunk content until the @@@ end delimiter.
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

            let (rest, line) = not_newline(current)?;

            // Add the line content
            content.push_str(line);

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
fn parse_end_delimiter_nom(input: &str) -> ParseResult<'_, bool> {
    let (input, _) = tag("@@@").parse(input)?;

    let (input, opt_tag) = opt(tag("-")).parse(input)?;
    let is_no_newline_eol = opt_tag.is_some();

    // Allow optional text after @@@ until end of line
    let (input, _) = opt(not_newline).parse(input)?;
    let (input, _) = alt((recognize(newline), recognize(nom::combinator::eof))).parse(input)?;
    Ok((input, is_no_newline_eol))
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
    ).parse(input)?;

    Ok((input, ()))
}

#[cfg(test)]
mod tests {
    use crate::format::parse::start_delimiter;

    use super::super::types::{Format, FormatError};
    use std::path::PathBuf;
    use std::str::FromStr;

    #[test]
    fn test_format_from_str_single_chunk() {
        let input = "@src/main.rs:10:2\nfn main() {\n    println!(\"Hello\");\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.0.len(), 1);
        assert_eq!(format.0[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(format.0[0].start_line, 10);
        assert_eq!(format.0[0].num_lines, 2);
        assert_eq!(
            format.0[0].content,
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
        assert_eq!(format.0.len(), 2);
        assert_eq!(format.0[0].path, PathBuf::from("test.txt"));
        assert_eq!(format.0[0].start_line, 5);
        assert_eq!(format.0[0].num_lines, 1);
        assert_eq!(format.0[0].content, "line 5\n");
        assert_eq!(format.0[1].path, PathBuf::from("test.txt"));
        assert_eq!(format.0[1].start_line, 10);
        assert_eq!(format.0[1].num_lines, 1);
        assert_eq!(format.0[1].content, "line 10\n");
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
        assert_eq!(format.0.len(), 2);
        assert_eq!(format.0[0].content, "content\n");
        assert_eq!(format.0[1].content, "more content\n");
    }

    #[test]
    fn test_format_from_str_with_escaped_chars() {
        let input = "@test.txt:1:1\nuser\\@domain.com\\\\path\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.0[0].content, "user@domain.com\\path\n");
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
            FormatError::InvalidLineNumber { value, .. } => {
                assert_eq!(value, "not_a_number");
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
            FormatError::InvalidNumLines { value, .. } => {
                assert_eq!(value, "invalid");
            }
            _ => panic!("Expected InvalidNumLines error"),
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
        assert_eq!(format.0.len(), 1);
        assert_eq!(format.0[0].content, "content\n");
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
        assert_eq!(format.0.len(), 2);
        assert_eq!(format.0[0].content, "line 5\n");
        assert_eq!(format.0[1].content, "line 10\n");
    }

    #[test]
    fn test_format_preserves_crlf() {
        // Test that Windows line endings (\r\n) are preserved in content
        let input = "@test.txt:1:2\r\nline1\r\nline2\r\n@@@\r\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.0.len(), 1);
        assert_eq!(format.0[0].path, PathBuf::from("test.txt"));
        assert_eq!(format.0[0].start_line, 1);
        assert_eq!(format.0[0].num_lines, 2);
        // The \r should be preserved as part of the line content
        assert_eq!(format.0[0].content, "line1\r\nline2\r\n");
    }

    #[test]
    fn test_format_crlf() {
        // Test that Windows line endings (\r\n) are preserved in content
        let input = "@test.txt:1:2\r\n";

        let res = start_delimiter(input).unwrap();
        assert_eq!(res.0, "");
    }

    #[test]
    fn test_format_mixed_line_endings() {
        // Test mixed line endings - some CRLF, some LF
        let input = "@test.txt:1:3\r\nline1\r\nline2\nline3\r\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.0.len(), 1);
        // Each line should preserve its original line ending
        assert_eq!(format.0[0].content, "line1\r\nline2\nline3\r\n");
    }
}
