use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    str::FromStr,
};

use clap::{Args, ValueEnum};

#[derive(Debug, Clone)]
enum Format {
    Jsonl,
    Json,
    Csv,
    Grep,
}

#[derive(Debug, Clone, Default)]
enum FormatOptions {
    #[default]
    Auto,
    Format(Format),
}

impl FormatOptions {
    fn parse<R: Read>(self, mut r: R) -> impl Iterator<Item = Result<IngestRecord, super::Error>> {
        if let Self::Format(format) = self {
            return EitherIter::Right(EitherIter::Left(format.parse(r)));
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
                    _ => return EitherIter::Left(std::iter::once(Err(super::Error::Io(e)))),
                },
            }
        }

        let guess = Format::guess(&cur[..total]);
        let buf: VecDeque<u8> = VecDeque::from(cur);
        EitherIter::Right(EitherIter::Right(guess.parse(buf.chain(r))))
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
            Self::Format(Format::Jsonl) => {
                clap::builder::PossibleValue::new("jsonl").help("parse as a jsonl")
            }
            Self::Format(Format::Json) => {
                clap::builder::PossibleValue::new("json").help("parse as a json")
            }
            Self::Format(Format::Grep) => {
                clap::builder::PossibleValue::new("grep").help("parse as a grep output")
            }
            Self::Format(Format::Csv) => {
                clap::builder::PossibleValue::new("csv").help("parse as a csv")
            }
            Self::Auto => clap::builder::PossibleValue::new("auto").help("auto-detect format"),
        })
    }
}

pub enum EitherIter<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> Iterator for EitherIter<L, R>
where
    L: Iterator,
    R: Iterator<Item = L::Item>,
{
    type Item = L::Item;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Left(l) => l.next(),
            Self::Right(r) => r.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Left(l) => l.size_hint(),
            Self::Right(r) => r.size_hint(),
        }
    }
}

impl Format {
    fn parse_jsonl<R: Read>(r: R) -> impl Iterator<Item = Result<IngestRecord, super::Error>> {
        BufReader::new(r).lines().map(|r| {
            let line = r?;
            Ok(serde_json::from_str(&line)?)
        })
    }

    fn parse_json<R: Read>(mut r: R) -> impl Iterator<Item = Result<IngestRecord, super::Error>> {
        let mut content = String::new();
        let res: Result<Vec<IngestRecord>, _> = r
            .read_to_string(&mut content)
            .map_err(Into::into)
            .map(move |_| content)
            .and_then(|content| Ok(serde_json::from_str(&content)?));

        match res {
            Ok(v) => EitherIter::Left(v.into_iter().map(Ok)),
            Err(v) => EitherIter::Right(std::iter::once(Err(v))),
        }
    }

    fn parse_csv<R: Read>(r: R) -> impl Iterator<Item = Result<IngestRecord, super::Error>> {
        let mut rdr = csv::Reader::from_reader(r);

        #[derive(Debug, Clone)]
        struct HeaderLocs {
            file_path: usize,
            line_number: usize,
        }

        let headers = rdr
            .headers()
            .cloned()
            .map_err(super::Error::Csv)
            .and_then(|r| {
                let file_path = ["filepath", "file", "path", "filepath", "filepath"];
                let line_number = [
                    "linenumber",
                    "line",
                    "linenumber",
                    "linenum",
                    "lineno",
                    "ln",
                ];

                let mut file_path_loc = None;
                let mut line_number_loc = None;

                for (i, h) in r.iter().enumerate() {
                    let h = h
                        .chars()
                        .map(|c| c.to_ascii_lowercase())
                        .filter(|c| !c.is_whitespace())
                        .filter(|c| !['-', '_'].contains(c))
                        .collect::<String>();

                    if h.is_empty() {
                        continue;
                    }

                    if file_path.contains(&h.as_str())
                        || file_path.contains(&&h.as_str()[..h.len() - 1])
                    {
                        file_path_loc = Some(i);
                    } else if line_number.contains(&h.as_str())
                        || line_number.contains(&&h.as_str()[..h.len() - 1])
                    {
                        line_number_loc = Some(i);
                    }
                }

                file_path_loc
                    .zip(line_number_loc)
                    .map(|(fp, ln)| HeaderLocs {
                        file_path: fp,
                        line_number: ln,
                    })
                    .ok_or(super::Error::CsvMissingHeaders)
            });

        rdr.into_records().map(move |r| {
            let headers = headers
                .as_ref()
                .ok()
                .cloned()
                .ok_or(super::Error::CsvMissingHeaders)?;
            let r = r.map_err(super::Error::Csv)?;

            Ok(IngestRecord {
                path: PathBuf::from(
                    r.get(headers.file_path)
                        .ok_or(super::Error::CsvMissingFields("file path"))?
                        .to_string(),
                ),
                line: r
                    .get(headers.line_number)
                    .ok_or(super::Error::CsvMissingFields("line number"))?
                    .parse()
                    .map_err(|_| super::Error::CsvCouldNotParse("line number"))?,
            })
        })
    }

    fn parse_grep<R: Read>(r: R) -> impl Iterator<Item = Result<IngestRecord, super::Error>> {
        BufReader::new(r).lines().filter_map(|r| {
            r.map(|line| {
                let line_no_split = line
                    .split_inclusive(':')
                    .scan(0usize, |bytes, c| {
                        let prev = *bytes;
                        *bytes += c.len();
                        Some((prev, c))
                    })
                    .find_map(|(pos, st)| match st.chars().nth(1)? {
                        '0'..='9' => Some(pos),
                        _ => None,
                    })?;

                let file = &line[..line_no_split - 1];
                let nums = line[line_no_split..]
                    .split_once(|c: char| !c.is_numeric())?
                    .0;

                let line_no: usize = nums.parse().ok()?;
                let file = PathBuf::from_str(file).ok()?;
                Some(IngestRecord {
                    path: file,
                    line: line_no,
                })
            })
            .map_err(Into::into)
            .transpose()
        })
    }

    pub fn parse<R: Read>(self, r: R) -> impl Iterator<Item = Result<IngestRecord, super::Error>> {
        match self {
            Self::Json => EitherIter::Left(EitherIter::Left(Self::parse_json(r))),
            Self::Jsonl => EitherIter::Left(EitherIter::Right(Self::parse_jsonl(r))),
            Self::Grep => EitherIter::Right(EitherIter::Left(Self::parse_grep(r))),
            Self::Csv => EitherIter::Right(EitherIter::Right(Self::parse_csv(r))),
        }
    }

    pub fn guess(line: &[u8]) -> Self {
        if line.starts_with(b"[{") {
            Self::Json
        } else if line.starts_with(b"{") {
            Self::Jsonl
        } else {
            line.iter()
                .find_map(|c| match c {
                    b',' => Some(Self::Csv),
                    b':' => Some(Self::Grep),
                    _ => None,
                })
                .unwrap_or(Self::Grep)
        }
    }
}

#[derive(Args, Debug)]
pub(super) struct IngestArgs {
    /// Directory or file to search (default: current directory)
    /// use '-' to read from stdin
    #[arg(default_value = None)]
    path: Option<PathBuf>,

    #[arg(short, long = "format", default_value = "auto")]
    format: FormatOptions,

    /// Lines of context before and after each match
    #[arg(short = 'C', long, default_value = "20")]
    context: usize,

    /// output as plain text (human-readable format)
    #[arg(long)]
    plain: bool,
}

#[derive(Debug, serde::Deserialize)]
struct IngestRecord {
    path: PathBuf,
    line: usize,
}

impl From<IngestRecord> for crate::types::IngestInput {
    fn from(value: IngestRecord) -> Self {
        Self {
            file_path: value.path,
            line_number: value.line,
        }
    }
}

impl IngestArgs {
    fn get_inputs(&self) -> Result<Vec<crate::types::IngestInput>, super::Error> {
        let mut stdin;
        let mut file;

        let stream: &mut dyn std::io::Read = match &self.path {
            Some(buf) if buf.as_os_str() != "-" => {
                file = std::fs::File::open(buf)?;
                &mut file
            }
            _ => {
                stdin = std::io::stdin();
                &mut stdin
            }
        };

        self.format
            .clone()
            .parse(stream)
            .map(|r| r.map(|i| i.into()))
            .collect()
    }

    pub fn handle(self) -> Result<(), super::Error> {
        let is_tty = atty::is(atty::Stream::Stdout);
        let inputs = self.get_inputs()?;

        let result = crate::ingest::ingest(
            &crate::filesystem::physical::PhysicalFS,
            inputs,
            self.context,
        )?;

        let format = crate::format::Format::from_matches(&result);
        print!("{}", format.display(self.plain, is_tty));

        Ok(())
    }
}
