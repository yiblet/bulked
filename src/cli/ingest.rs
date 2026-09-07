use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, BufWriter, IsTerminal, Read, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use clap::{Args, ValueEnum};
use miette::Diagnostic;

use crate::filesystem::physical::PhysicalFS;
use crate::filesystem::{FileSystem, ReadFs};
use crate::types::IngestInput;

use super::Exit;

/// Errors produced while decoding `(path, line)` records from ingest input.
///
/// These are specific to the CLI's input decoders (jsonl / json / csv / grep)
/// and are wrapped by a single transparent `cli::Error::IngestParse` variant, so
/// their `help` text reaches the user through miette.
#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum IngestParseError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// One jsonl / json / csv record could not be decoded.
    #[error("could not decode input line {line} as {format}: {message}\n  {text}")]
    #[diagnostic(code(ingest::decode))]
    Decode {
        format: &'static str,
        line: usize,
        text: String,
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("csv header row {found:?} has no path column and/or no line column")]
    #[diagnostic(
        code(ingest::csv_headers),
        help(
            "the header must name a path column (one of: path, file, filepath) and a line column (one of: line, lineno, linenum, ln, linenumber); plurals and -/_ separators are fine"
        )
    )]
    MissingHeaders { found: Vec<String> },

    #[error("csv row {line} is missing its {field} field")]
    #[diagnostic(code(ingest::csv_fields))]
    MissingFields { line: usize, field: &'static str },

    #[error("csv row {line}: could not parse {field} {value:?} as a line number")]
    #[diagnostic(
        code(ingest::csv_parse),
        help("line numbers are positive integers (1-based)")
    )]
    CouldNotParse {
        line: usize,
        field: &'static str,
        value: String,
    },

    /// The input had lines, but none of them contained a `path:line` location.
    #[error(
        "no `path:line` locations found in the input ({lines} non-blank lines); the first line was {first:?}"
    )]
    #[diagnostic(code(ingest::no_locations))]
    NoLocations {
        lines: usize,
        first: String,
        #[help]
        help: Option<String>,
    },
}

/// One decoded input line: a usable location, or a line the grep decoder could
/// not read as `path:line`. Skipped lines are data, not errors — `run` reports
/// them all at once, with a hint about what they probably are.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decoded {
    Location(IngestInput),
    Skipped(String),
}

/// A stream of decoded ingest records borrowing the underlying reader.
type RecordIter<'r> = Box<dyn Iterator<Item = Result<Decoded, IngestParseError>> + 'r>;

/// Normalized (see [`normalize_header`]) CSV header names accepted for the
/// file-path column.
const FILE_PATH_HEADERS: [&str; 3] = ["filepath", "file", "path"];

/// Normalized (see [`normalize_header`]) CSV header names accepted for the
/// line-number column.
const LINE_NUMBER_HEADERS: [&str; 5] = ["linenumber", "line", "linenum", "lineno", "ln"];

const JSON_SHAPE_HELP: &str = r#"expected one {"path": "src/a.rs", "line": 12} object per line ("file" and "line_number" are accepted too); use --format to override auto-detection"#;
const RG_JSON_HELP: &str =
    "this looks like `rg --json` output, which is not supported; use plain `rg -n` output instead";

/// Lowercase ASCII letters and drop whitespace, `-` and `_`, so that
/// `File_Path`, `file path` and `FILE-PATH` all normalize to `filepath`.
fn normalize_header(h: &str) -> String {
    h.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '_'))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Does a normalized header name match one of `names`, tolerating a plural `s`?
///
/// Uses `strip_suffix` rather than byte slicing so a header ending in a
/// multi-byte character can never split a UTF-8 sequence.
fn header_matches(norm: &str, names: &[&str]) -> bool {
    names.contains(&norm) || norm.strip_suffix('s').is_some_and(|s| names.contains(&s))
}

/// Split a `grep -n`-style line into `(path, line_number)`.
///
/// The path ends at the first `:` that is followed by a run of ASCII digits
/// terminated by another `:` or the end of the line. Colons not followed by
/// such a run (e.g. the drive letter in `C:\x:5:...`) are part of the path, and
/// so is a `:0:` — line numbers are 1-based, so zero is never a location.
fn split_grep_line(line: &str) -> Option<(&str, NonZeroUsize)> {
    for (idx, _) in line.match_indices(':') {
        let rest = &line[idx + 1..];
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            continue;
        }
        let terminated = digits_end == rest.len() || rest.as_bytes()[digits_end] == b':';
        if !terminated {
            continue;
        }
        if let Ok(line_number) = rest[..digits_end].parse() {
            return Some((&line[..idx], line_number));
        }
    }
    None
}

/// Does `line` look like a `@path:line:numlines` chunk header from bulked's own
/// format? (The user probably meant `bulked apply`.)
fn looks_like_chunk_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('@') else {
        return false;
    };
    let mut parts = rest.rsplitn(3, ':');
    let (Some(numlines), Some(line_no), Some(_path)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(line_no) && numlines.split_whitespace().next().is_some_and(digits)
}

/// Work out what the lines the grep decoder skipped probably are, and say what
/// to do about it. `exists` answers "is this a file the user could mean?" so the
/// heuristics can tell `src/main.rs:fn main() {` (a path with no line number)
/// from arbitrary text.
fn diagnose_skipped(skipped: &[String], exists: &dyn Fn(&Path) -> bool) -> String {
    let mut chunk_header = false;
    let mut rg_json = false;
    let mut zero_line = false;
    let mut no_filename = 0usize; // `12:text` — line number, no path
    let mut bare_path = 0usize; // `src/main.rs` alone (rg --heading)
    let mut no_line_number = 0usize; // `src/main.rs:text` — path, no line number

    for line in skipped {
        let line = line.trim_end();
        if looks_like_chunk_header(line) {
            chunk_header = true;
        } else if line.starts_with("{\"type\":") {
            rg_json = true;
        } else if line.contains(":0:") || line.ends_with(":0") {
            zero_line = true;
        } else if line
            .split_once([':', '-'])
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            no_filename += 1;
        } else if !line.is_empty() && !line.contains(':') && exists(Path::new(line)) {
            bare_path += 1;
        } else if line
            .split_once(':')
            .is_some_and(|(path, _)| exists(Path::new(path)))
        {
            no_line_number += 1;
        }
    }

    if chunk_header {
        "this input is bulked's own chunk format; did you mean `bulked apply`?".to_string()
    } else if rg_json {
        RG_JSON_HELP.to_string()
    } else if zero_line {
        "line numbers are 1-based, so a location at line 0 cannot exist".to_string()
    } else if no_filename > 0 && bare_path > 0 {
        "this looks like `rg --heading` output (file name on its own line, then `line:text`); pass --no-heading to rg".to_string()
    } else if no_filename > 0 {
        "these lines have a line number but no file name — grep/rg omit the path when searching a single file; add -H (--with-filename)".to_string()
    } else if no_line_number > 0 {
        "these lines name a file but have no line number; add -n (--line-number) to grep/rg"
            .to_string()
    } else {
        "each line must look like `path:line[:text]` (the output of grep -n / rg -n); for other shapes use --format jsonl, json or csv".to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Format {
    Jsonl,
    Json,
    Csv,
    Grep,
}

#[derive(Debug, Clone, Default)]
pub(crate) enum FormatOptions {
    #[default]
    Auto,
    Format(Format),
}

impl FormatOptions {
    fn parse<'r, R: Read + 'r>(self, mut r: R) -> RecordIter<'r> {
        if let Self::Format(format) = self {
            return format.parse(r);
        }

        let mut total = 0;
        let mut cur = [0u8; 1024];
        loop {
            match r.read(&mut cur[total..]) {
                Ok(0) => {
                    break;
                }
                Ok(n) => {
                    total += n;
                    if total == cur.len() {
                        break;
                    }
                }
                Err(e) => match e.kind() {
                    std::io::ErrorKind::UnexpectedEof => break,
                    _ => return Box::new(std::iter::once(Err(IngestParseError::Io(e)))),
                },
            }
        }

        let guess = Format::guess(&cur[..total]);
        let buf: VecDeque<u8> = VecDeque::from(cur[..total].to_vec());
        guess.parse(buf.chain(r))
    }
}

impl ValueEnum for FormatOptions {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            FormatOptions::Format(Format::Jsonl),
            FormatOptions::Format(Format::Json),
            FormatOptions::Format(Format::Grep),
            FormatOptions::Format(Format::Csv),
            FormatOptions::Auto,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Self::Format(Format::Jsonl) => clap::builder::PossibleValue::new("jsonl")
                .help("one {\"path\":..,\"line\":..} object per line"),
            Self::Format(Format::Json) => {
                clap::builder::PossibleValue::new("json").help("a JSON array of those objects")
            }
            Self::Format(Format::Grep) => clap::builder::PossibleValue::new("grep")
                .help("path:line:... lines, as from `grep -rn` or `rg -n`"),
            Self::Format(Format::Csv) => clap::builder::PossibleValue::new("csv")
                .help("header row with a path column and a line column"),
            Self::Auto => clap::builder::PossibleValue::new("auto")
                .help("detect from the first bytes of input"),
        })
    }
}

/// Column indices of the path and line-number fields in a CSV header row.
#[derive(Debug, Clone, Copy)]
struct HeaderLocs {
    file_path: usize,
    line_number: usize,
}

impl HeaderLocs {
    fn find(headers: &csv::StringRecord) -> Result<Self, IngestParseError> {
        let mut file_path = None;
        let mut line_number = None;

        for (i, h) in headers.iter().enumerate() {
            let norm = normalize_header(h);
            if header_matches(&norm, &FILE_PATH_HEADERS) {
                file_path = Some(i);
            } else if header_matches(&norm, &LINE_NUMBER_HEADERS) {
                line_number = Some(i);
            }
        }

        file_path
            .zip(line_number)
            .map(|(file_path, line_number)| Self {
                file_path,
                line_number,
            })
            .ok_or_else(|| IngestParseError::MissingHeaders {
                found: headers.iter().map(str::to_string).collect(),
            })
    }
}

/// Build the error for a JSON record that failed to decode, with a help line
/// that names the expected shape (or recognizes `rg --json`).
fn json_decode_error(
    format: &'static str,
    line: usize,
    text: &str,
    source: &serde_json::Error,
) -> IngestParseError {
    let help = if text.contains("\"type\":") {
        RG_JSON_HELP
    } else {
        JSON_SHAPE_HELP
    };
    IngestParseError::Decode {
        format,
        line,
        text: text.trim().to_string(),
        message: source.to_string(),
        help: Some(help.to_string()),
    }
}

impl Format {
    fn parse_jsonl<'r, R: Read + 'r>(r: R) -> RecordIter<'r> {
        Box::new(
            BufReader::new(r)
                .lines()
                .enumerate()
                .filter_map(|(idx, line)| {
                    let line = match line {
                        Ok(line) => line,
                        Err(e) => return Some(Err(IngestParseError::Io(e))),
                    };
                    if line.trim().is_empty() {
                        return None;
                    }
                    Some(
                        serde_json::from_str::<IngestInput>(&line)
                            .map(Decoded::Location)
                            .map_err(|e| json_decode_error("jsonl", idx + 1, &line, &e)),
                    )
                }),
        )
    }

    fn parse_json<'r, R: Read + 'r>(mut r: R) -> RecordIter<'r> {
        let mut content = String::new();
        if let Err(e) = r.read_to_string(&mut content) {
            return Box::new(std::iter::once(Err(IngestParseError::Io(e))));
        }
        match serde_json::from_str::<Vec<IngestInput>>(&content) {
            Ok(v) => Box::new(v.into_iter().map(|i| Ok(Decoded::Location(i)))),
            Err(e) => {
                let text = content
                    .lines()
                    .nth(e.line().saturating_sub(1))
                    .unwrap_or("");
                Box::new(std::iter::once(Err(json_decode_error(
                    "json",
                    e.line(),
                    text,
                    &e,
                ))))
            }
        }
    }

    fn parse_csv<'r, R: Read + 'r>(r: R) -> RecordIter<'r> {
        let mut rdr = csv::Reader::from_reader(r);

        let locs = match rdr
            .headers()
            .map_err(|e| IngestParseError::Decode {
                format: "csv",
                line: 1,
                text: String::new(),
                message: e.to_string(),
                help: None,
            })
            .and_then(HeaderLocs::find)
        {
            Ok(locs) => locs,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };

        Box::new(rdr.into_records().map(move |r| {
            let r = r.map_err(|e| IngestParseError::Decode {
                format: "csv",
                line: e
                    .position()
                    .and_then(|p| usize::try_from(p.line()).ok())
                    .unwrap_or(0),
                text: String::new(),
                message: e.to_string(),
                help: None,
            })?;
            let line = r
                .position()
                .and_then(|p| usize::try_from(p.line()).ok())
                .unwrap_or(0);

            let file_path = r
                .get(locs.file_path)
                .ok_or(IngestParseError::MissingFields {
                    line,
                    field: "path",
                })?;
            let raw_line = r
                .get(locs.line_number)
                .ok_or(IngestParseError::MissingFields {
                    line,
                    field: "line",
                })?;
            let line_number =
                raw_line
                    .trim()
                    .parse()
                    .map_err(|_| IngestParseError::CouldNotParse {
                        line,
                        field: "line",
                        value: raw_line.to_string(),
                    })?;

            Ok(Decoded::Location(IngestInput {
                file_path: PathBuf::from(file_path),
                line_number,
            }))
        }))
    }

    fn parse_grep<'r, R: Read + 'r>(r: R) -> RecordIter<'r> {
        Box::new(BufReader::new(r).lines().filter_map(|r| match r {
            Err(e) => Some(Err(IngestParseError::Io(e))),
            Ok(line) if line.trim().is_empty() => None,
            // `@path:line:len` would parse as a grep line with an `@path` path;
            // skip it so the diagnosis can say "did you mean `bulked apply`".
            Ok(line) if looks_like_chunk_header(&line) => Some(Ok(Decoded::Skipped(line))),
            Ok(line) => Some(Ok(match split_grep_line(&line) {
                Some((file, line_number)) => Decoded::Location(IngestInput {
                    file_path: PathBuf::from(file),
                    line_number,
                }),
                None => Decoded::Skipped(line),
            })),
        }))
    }

    pub fn parse<'r, R: Read + 'r>(self, r: R) -> RecordIter<'r> {
        match self {
            Self::Json => Self::parse_json(r),
            Self::Jsonl => Self::parse_jsonl(r),
            Self::Grep => Self::parse_grep(r),
            Self::Csv => Self::parse_csv(r),
        }
    }

    /// Guess the format from the first bytes of the input. Leading whitespace is
    /// ignored so a pretty-printed JSON array (`[\n  {`) is still JSON.
    pub fn guess(head: &[u8]) -> Self {
        let head = head
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map_or(&head[head.len()..], |i| &head[i..]);
        if head.starts_with(b"[") {
            let after = head[1..]
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .map_or(&head[head.len()..], |i| &head[1 + i..]);
            if after.is_empty() || after.starts_with(b"{") || after.starts_with(b"]") {
                return Self::Json;
            }
        }
        if head.starts_with(b"{") {
            return Self::Jsonl;
        }
        head.iter()
            .find_map(|c| match c {
                b',' => Some(Self::Csv),
                b':' => Some(Self::Grep),
                _ => None,
            })
            .unwrap_or(Self::Grep)
    }
}

/// Everything the decoders produced for one run.
#[derive(Debug, Default)]
struct DecodedInput {
    locations: Vec<IngestInput>,
    skipped: Vec<String>,
}

#[derive(Args, Debug)]
#[command(
    after_long_help = r#"Reads path:line locations from a file or stdin and prints the lines around each
one as an editable chunk for `bulked apply`. Anything that names a file and a
line number can feed it: grep, ripgrep, compiler or linter errors, a CSV export.

Grep-style lines with no path:line prefix are skipped and summarized on stderr
(usually a forgotten -n or -H). If no line yields a location, ingest fails.

EXAMPLES
  grep -rn 'TODO' src/ | bulked ingest > edits.bk    # or: -o edits.bk
  bulked ingest locations.csv -C 5 -o edits.bk       # less context
  bulked ingest --format json locations.json         # skip auto-detection

Then edit edits.bk and run `bulked apply -i edits.bk`."#
)]
pub(crate) struct IngestArgs {
    /// File of locations to read; omit or use '-' for stdin
    #[arg(default_value = None)]
    pub(crate) path: Option<PathBuf>,

    /// Input format
    #[arg(short, long = "format", default_value = "auto")]
    pub(crate) format: FormatOptions,

    /// Write the chunks to this file instead of stdout
    #[arg(short, long)]
    pub(crate) output: Option<PathBuf>,

    /// Lines of context before and after each location
    #[arg(short = 'C', long, default_value = "20")]
    pub(crate) context: usize,

    /// Print a readable listing instead of chunks (cannot be applied)
    #[arg(long)]
    pub(crate) plain: bool,
}

impl IngestArgs {
    fn reads_stdin(&self) -> bool {
        self.path.as_deref().is_none_or(|p| p.as_os_str() == "-")
    }

    /// Decode the `(path, line)` records from the locations file (via `fs`) or,
    /// when no path is given or it is `-`, from `input`.
    fn decode(&self, fs: &dyn ReadFs, input: &mut dyn Read) -> Result<DecodedInput, super::Error> {
        let mut file;

        let stream: &mut dyn Read = match &self.path {
            Some(buf) if buf.as_os_str() != "-" => {
                file = fs.read(buf)?;
                &mut file
            }
            _ => input,
        };

        let mut decoded = DecodedInput::default();
        for record in self.format.clone().parse(stream) {
            match record? {
                Decoded::Location(location) => decoded.locations.push(location),
                Decoded::Skipped(line) => decoded.skipped.push(line),
            }
        }
        Ok(decoded)
    }

    /// Run `ingest` against an injected filesystem, input stream, and sinks.
    ///
    /// This owns all of the subcommand's behavior; [`IngestArgs::handle`] is
    /// the thin production wrapper. Locations are read from `fs` (or `input`
    /// when no path / `-` is given), context lines are read through `fs`, the
    /// chunk format is written to `--output` through `fs` or else to `out`, and
    /// status lines and skipped-line summaries go to `err`. `color` says whether
    /// to ANSI-highlight the target lines.
    ///
    /// Returns [`Exit::Nothing`] when the input was empty. Input that had lines
    /// but no locations at all is an error ([`IngestParseError::NoLocations`])
    /// with a hint about what the lines probably were.
    pub fn run(
        self,
        fs: &dyn FileSystem,
        input: &mut dyn Read,
        out: &mut dyn Write,
        err: &mut dyn Write,
        color: bool,
    ) -> Result<Exit, super::Error> {
        let DecodedInput { locations, skipped } = self.decode(fs, input)?;
        let exists = |p: &Path| fs.read(p).is_ok();

        if locations.is_empty() {
            if skipped.is_empty() {
                writeln!(err, "bulked ingest: no input")?;
                return Ok(Exit::Nothing);
            }
            let help = diagnose_skipped(&skipped, &exists);
            return Err(IngestParseError::NoLocations {
                lines: skipped.len(),
                first: skipped[0].clone(),
                help: Some(help),
            }
            .into());
        }

        if !skipped.is_empty() {
            writeln!(
                err,
                "bulked ingest: skipped {} of {} input lines with no `path:line` location, e.g. {:?}",
                skipped.len(),
                skipped.len() + locations.len(),
                skipped[0]
            )?;
            writeln!(err, "  hint: {}", diagnose_skipped(&skipped, &exists))?;
        }

        let result = crate::ingest::ingest(fs, locations, self.context)?;

        let format = crate::format::Format::from_matches(&result);

        let mut file;
        let sink: &mut dyn Write = match &self.output {
            Some(path) => {
                file = BufWriter::new(fs.writer(path)?);
                &mut file
            }
            None => out,
        };

        write!(sink, "{}", format.display(self.plain, color))?;
        sink.flush()?;

        // When the output went to a file, report a status line.
        if let Some(path) = &self.output {
            writeln!(
                err,
                "bulked ingest wrote {} to {}",
                super::plural(format.len(), "chunk"),
                path.display()
            )?;
        }

        Ok(if format.is_empty() {
            Exit::Nothing
        } else {
            Exit::Ok
        })
    }

    pub fn handle(self) -> Result<Exit, super::Error> {
        if self.reads_stdin() && io::stdin().is_terminal() {
            eprintln!(
                "bulked ingest: reading locations from standard input; pipe in `grep -n` / `rg -n` output or pass a file (Ctrl-D to finish)"
            );
        }
        // When writing to a file, never colorize (it's not a terminal).
        let color = self.output.is_none() && io::stdout().is_terminal();
        self.run(
            &PhysicalFS,
            &mut io::stdin(),
            &mut io::stdout(),
            &mut io::stderr(),
            color,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("test line numbers are non-zero")
    }

    fn record(path: &str, line_number: usize) -> IngestInput {
        IngestInput {
            file_path: PathBuf::from(path),
            line_number: nz(line_number),
        }
    }

    fn collect(format: Format, input: &str) -> Result<Vec<Decoded>, IngestParseError> {
        format.parse(input.as_bytes()).collect()
    }

    fn locations(format: Format, input: &str) -> Result<Vec<IngestInput>, IngestParseError> {
        Ok(collect(format, input)?
            .into_iter()
            .filter_map(|d| match d {
                Decoded::Location(l) => Some(l),
                Decoded::Skipped(_) => None,
            })
            .collect())
    }

    #[test]
    fn test_header_matches_tolerates_plural_and_separators() {
        let paths = normalize_header("File_Paths");
        assert!(header_matches(&paths, &FILE_PATH_HEADERS));
        assert!(!header_matches(&paths, &LINE_NUMBER_HEADERS));

        let lines = normalize_header("Line-Nos");
        assert!(header_matches(&lines, &LINE_NUMBER_HEADERS));
        assert!(!header_matches(&lines, &FILE_PATH_HEADERS));

        // Singular forms and mixed separators still match.
        assert!(header_matches(
            &normalize_header("file path"),
            &FILE_PATH_HEADERS
        ));
        assert!(header_matches(
            &normalize_header("LINE_NUMBER"),
            &LINE_NUMBER_HEADERS
        ));
    }

    #[test]
    fn test_header_matches_multibyte_suffix_does_not_panic() {
        let norm = normalize_header("pathé");
        assert!(!header_matches(&norm, &FILE_PATH_HEADERS));
        assert!(!header_matches(&norm, &LINE_NUMBER_HEADERS));
    }

    #[test]
    fn test_parse_csv_multibyte_header_is_missing_headers_not_panic() {
        // The header row ends in a multi-byte character; the old byte-slice
        // normalization panicked here. The error names the headers it saw.
        let err = locations(Format::Csv, "pathé,line\nsrc/a.rs,3\n").unwrap_err();
        assert!(
            matches!(&err, IngestParseError::MissingHeaders { found } if found == &["pathé", "line"]),
            "{err:?}"
        );

        // A plural header with a separator still resolves both columns.
        let records = locations(Format::Csv, "Line-Nos,File_Paths\n3,src/a.rs\n").unwrap();
        assert_eq!(records, vec![record("src/a.rs", 3)]);
    }

    #[test]
    fn test_parse_csv_bad_line_number_names_row_and_value() {
        let err = locations(Format::Csv, "path,line\nsrc/a.rs,x\n").unwrap_err();
        assert!(
            matches!(&err, IngestParseError::CouldNotParse { line: 2, field: "line", value } if value == "x"),
            "{err:?}"
        );
        // Zero is not a line number either.
        assert!(matches!(
            locations(Format::Csv, "path,line\nsrc/a.rs,0\n").unwrap_err(),
            IngestParseError::CouldNotParse { .. }
        ));
    }

    #[test]
    fn test_parse_grep_yields_skipped_lines_and_drops_blank_ones() {
        let decoded = collect(
            Format::Grep,
            "src/a.rs:12:x\n--\n\nBinary file b matches\n   \n",
        )
        .unwrap();
        assert_eq!(
            decoded,
            vec![
                Decoded::Location(record("src/a.rs", 12)),
                Decoded::Skipped("--".to_string()),
                Decoded::Skipped("Binary file b matches".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_grep_skips_bulked_chunk_headers() {
        // A .bk file piped into ingest must not be read as `@src/a.rs` locations.
        let decoded = collect(Format::Grep, "@src/a.rs:1:2 #deadbeef\nfn x() {}\n@@@\n").unwrap();
        assert_eq!(
            decoded,
            vec![
                Decoded::Skipped("@src/a.rs:1:2 #deadbeef".to_string()),
                Decoded::Skipped("fn x() {}".to_string()),
                Decoded::Skipped("@@@".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_grep_accepts_single_digit_line_numbers() {
        let records = locations(Format::Grep, "src/main.rs:6:fn main\nsrc/a.rs:7\n").unwrap();
        assert_eq!(
            records,
            vec![record("src/main.rs", 6), record("src/a.rs", 7)]
        );
    }

    #[test]
    fn test_split_grep_line_keeps_drive_letter_colon_in_path() {
        assert_eq!(split_grep_line("C:\\x:5:foo"), Some(("C:\\x", nz(5))));
        assert_eq!(split_grep_line("a:b:c"), None);
        assert_eq!(split_grep_line("a:12x:3"), Some(("a:12x", nz(3))));
        // Line 0 is not a location; the line is skipped rather than mis-read.
        assert_eq!(split_grep_line("src/a.rs:0:foo"), None);
    }

    #[test]
    fn test_parse_jsonl_accepts_aliases_and_explains_failures() {
        let records = locations(
            Format::Jsonl,
            "{\"path\":\"a.rs\",\"line\":1}\n\n{\"file\":\"b.rs\",\"line_number\":2}\n",
        )
        .unwrap();
        assert_eq!(records, vec![record("a.rs", 1), record("b.rs", 2)]);

        let err = locations(
            Format::Jsonl,
            "{\"path\":\"a.rs\",\"line\":1}\n{\"nope\":1}\n",
        )
        .unwrap_err();
        match err {
            IngestParseError::Decode {
                format: "jsonl",
                line: 2,
                text,
                help: Some(help),
                ..
            } => {
                assert_eq!(text, "{\"nope\":1}");
                assert!(help.contains("\"path\""), "{help}");
            }
            other => panic!("unexpected {other:?}"),
        }

        // `rg --json` is recognized by its shape.
        let err = locations(Format::Jsonl, "{\"type\":\"begin\",\"data\":{}}\n").unwrap_err();
        assert!(
            matches!(&err, IngestParseError::Decode { help: Some(h), .. } if h.contains("rg --json")),
            "{err:?}"
        );

        // Zero is rejected at decode time, not silently dropped later.
        assert!(matches!(
            locations(Format::Jsonl, "{\"path\":\"a.rs\",\"line\":0}\n").unwrap_err(),
            IngestParseError::Decode { line: 1, .. }
        ));
    }

    #[test]
    fn test_parse_json_reports_the_offending_line() {
        let err = locations(
            Format::Json,
            "[\n  {\"path\":\"a.rs\",\"line\":1},\n  {\"path\":\"b.rs\"}\n]",
        )
        .unwrap_err();
        assert!(
            matches!(&err, IngestParseError::Decode { format: "json", line: 3, text, .. } if text == "{\"path\":\"b.rs\"}"),
            "{err:?}"
        );
    }

    #[test]
    fn test_guess_format() {
        assert_eq!(Format::guess(b"[{"), Format::Json);
        assert_eq!(Format::guess(b"[\n  {\"path\""), Format::Json);
        assert_eq!(Format::guess(b"  [ {"), Format::Json);
        assert_eq!(Format::guess(b"[]"), Format::Json);
        assert_eq!(Format::guess(b"{"), Format::Jsonl);
        assert_eq!(Format::guess(b"a,b"), Format::Csv);
        assert_eq!(Format::guess(b"a:1"), Format::Grep);
        assert_eq!(Format::guess(b"[1:2]"), Format::Grep);
    }

    #[test]
    fn test_looks_like_chunk_header() {
        assert!(looks_like_chunk_header("@src/lib.rs:1:3"));
        assert!(looks_like_chunk_header("@src/lib.rs:1:3 #deadbeef"));
        assert!(looks_like_chunk_header("@C:\\x\\y.rs:10:2"));
        assert!(!looks_like_chunk_header("@@@"));
        assert!(!looks_like_chunk_header("@dataclass"));
        assert!(!looks_like_chunk_header("src/lib.rs:1:3"));
    }

    #[test]
    fn test_diagnose_skipped_recognizes_common_mistakes() {
        let exists = |p: &Path| matches!(p.to_str(), Some("src/main.rs" | "./src/main.rs"));
        let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // rg/grep without -n
        let hint = diagnose_skipped(&lines(&["./src/main.rs:fn main() {"]), &exists);
        assert!(hint.contains("-n"), "{hint}");

        // rg -n on a single file: no file name
        let hint = diagnose_skipped(&lines(&["1:fn main() {", "7:fn compute"]), &exists);
        assert!(hint.contains("-H"), "{hint}");

        // rg --heading: bare path then numbered lines
        let hint = diagnose_skipped(&lines(&["src/main.rs", "2:    // TODO"]), &exists);
        assert!(hint.contains("--no-heading"), "{hint}");

        // bulked's own format
        let hint = diagnose_skipped(&lines(&["@src/main.rs:1:3", "fn main() {"]), &exists);
        assert!(hint.contains("bulked apply"), "{hint}");

        // rg --json
        let hint = diagnose_skipped(&lines(&["{\"type\":\"begin\",\"data\":{}}"]), &exists);
        assert!(hint.contains("rg --json"), "{hint}");

        // line 0
        let hint = diagnose_skipped(&lines(&["src/main.rs:0:x"]), &exists);
        assert!(hint.contains("1-based"), "{hint}");

        // nothing recognizable
        let hint = diagnose_skipped(&lines(&["Binary file x matches"]), &exists);
        assert!(hint.contains("--format"), "{hint}");
    }
}
