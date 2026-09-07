//! Bring a stale chunk file back in line with the files it edits.
//!
//! `apply` refuses a chunk whose header fingerprint no longer matches the lines it
//! replaces ([`ApplyError::ContentChanged`]). When the files are right and the
//! chunk file is what is stale (the files were edited after `ingest`, or the chunks
//! come from an older run), `refresh` fixes the stale chunks so the `.bk` applies
//! again. What "fix" means depends on whether the chunk was edited, which the
//! fingerprint tells us: the header hash is the hash of the original lines, so a
//! chunk whose content still hashes to its header was never touched.
//!
//! - **Unedited** stale chunk: its content is replaced with the lines now in the
//!   file and the fingerprint updated, so the `.bk` mirrors the file again and is
//!   ready to edit. (Applying the old content would have reverted the file.)
//! - **Edited** stale chunk: the edit is kept and only the fingerprint is updated,
//!   so applying will overwrite the lines that changed. `--dry-run` shows these.
//! - **Already applied** chunk (its content is what the file has now): fingerprint
//!   only, and it is reported as such.
//!
//! The rewrite is textual. The chunk file is parsed once to learn where every
//! chunk keeps its fingerprint tag and its body ([`crate::format::parse::ChunkSpans`]), the stale ones
//! are found by running apply's own verification, and only those spans are
//! spliced. Comments, chunk order, escaping, and line endings elsewhere are
//! preserved byte for byte.
//!
//! This module is I/O-free apart from the injected [`ReadFs`]: it reads the
//! current lines through the port and returns the new text for the caller to
//! write wherever it likes.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::apply::{ApplyError, ApplyErrors, verify_plan};
use crate::filesystem::{FilesystemError, ReadFs};
use crate::format::parse::parse_format_with_spans;
use crate::format::types::{FormatError, chunk_body};
use crate::format::{Chunk, Fingerprint, LineRange};

/// Why a chunk file could not be refreshed.
///
/// Refresh only fixes stale chunks. A chunk file that does not parse, or whose
/// chunks overlap, run past the end of their file, or cannot be read, is reported
/// exactly as `apply` would report it.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum RefreshError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Format(#[from] FormatError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Apply(#[from] ApplyErrors),

    #[error(transparent)]
    Filesystem(#[from] FilesystemError),

    #[error("failed to reread lines {range} of {path}: {source}")]
    Read {
        path: PathBuf,
        range: LineRange,
        #[source]
        source: std::io::Error,
    },
}

/// What refresh did to one stale chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshKind {
    /// The chunk was never edited: its content was replaced with the file's
    /// current lines and its fingerprint updated.
    Reread,
    /// The chunk was edited: the edit is kept and only the fingerprint updated, so
    /// applying it will overwrite lines that changed since.
    KeptEdit,
    /// The chunk's content is exactly what the file has now (it was already
    /// applied): fingerprint updated, content unchanged.
    AlreadyApplied,
}

/// One chunk that was rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refreshed {
    pub path: PathBuf,
    pub range: LineRange,
    pub kind: RefreshKind,
    pub old_fingerprint: Fingerprint,
    pub new_fingerprint: Fingerprint,
    /// The chunk content before and after. Equal unless `kind` is [`RefreshKind::Reread`].
    pub old_content: String,
    pub new_content: String,
}

/// The outcome of [`refresh`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refresh {
    /// The chunk file with every stale chunk rewritten. Identical to the input
    /// when `refreshed` is empty.
    pub text: String,
    /// The chunks that were rewritten, in source order.
    pub refreshed: Vec<Refreshed>,
    /// How many chunks the file has.
    pub chunks: usize,
    /// How many of them carry a fingerprint (only those can be stale).
    pub fingerprinted: usize,
}

/// Rewrite the stale chunks in the chunk file `src` against the files in `fs`.
///
/// A chunk is stale when the lines it replaces, read from `fs` now, no longer hash
/// to its header fingerprint. See the module docs for what is rewritten in each
/// case. Chunks with a matching fingerprint, chunks without one, and every byte
/// outside the rewritten tags and bodies are left untouched.
///
/// # Errors
/// [`RefreshError::Format`] when `src` is not a valid chunk file;
/// [`RefreshError::Apply`] for the problems refresh cannot fix (overlapping
/// chunks, chunks past the end of their file, unreadable files); and
/// [`RefreshError::Filesystem`] / [`RefreshError::Read`] when rereading a file
/// fails.
pub fn refresh(src: &str, fs: &dyn ReadFs) -> Result<Refresh, RefreshError> {
    let (format, spans) = parse_format_with_spans(src)?;
    let plan = format.validate()?;

    // Apply's verification is the one definition of "stale": every mismatch it
    // reports carries the fingerprint of the lines actually in the file.
    let mut stale: HashMap<(PathBuf, LineRange), Fingerprint> = HashMap::new();
    let mut fatal = Vec::new();
    if let Err(errors) = verify_plan(&plan, fs) {
        for error in errors {
            match error {
                ApplyError::ContentChanged {
                    path,
                    range,
                    actual,
                    ..
                } => {
                    stale.insert((path, range), actual);
                }
                other => fatal.push(other),
            }
        }
    }
    if !fatal.is_empty() {
        return Err(ApplyErrors::from(fatal).into());
    }

    // A validated plan has at most one chunk per (path, range).
    let chunks: HashMap<(&Path, LineRange), &Chunk> =
        format.iter().map(|c| ((c.path(), c.range()), c)).collect();

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut refreshed = Vec::new();
    for span in &spans {
        let Some(old_fingerprint) = span.fingerprint else {
            continue;
        };
        let Some(&file_now) = stale.get(&(span.path.clone(), span.range)) else {
            continue;
        };
        let chunk = chunks[&(span.path.as_path(), span.range)];
        let content_hash = Fingerprint::of(chunk.content().as_bytes());

        let (kind, new_content, new_fingerprint) = if content_hash == old_fingerprint {
            let lines = read_lines(fs, &span.path, span.range)?;
            let fingerprint = Fingerprint::of(lines.as_bytes());
            (RefreshKind::Reread, lines, fingerprint)
        } else if content_hash == file_now {
            (
                RefreshKind::AlreadyApplied,
                chunk.content().to_string(),
                file_now,
            )
        } else {
            (RefreshKind::KeptEdit, chunk.content().to_string(), file_now)
        };

        edits.push((span.tag.clone(), format!(" #{new_fingerprint}")));
        if kind == RefreshKind::Reread {
            edits.push((span.body.clone(), chunk_body(&new_content)));
        }
        refreshed.push(Refreshed {
            path: span.path.clone(),
            range: span.range,
            kind,
            old_fingerprint,
            new_fingerprint,
            old_content: chunk.content().to_string(),
            new_content,
        });
    }

    Ok(Refresh {
        text: splice(src, &edits),
        refreshed,
        chunks: spans.len(),
        fingerprinted: spans.iter().filter(|s| s.fingerprint.is_some()).count(),
    })
}

/// Read lines `range` of `path` through `fs`, with their line endings, exactly as
/// `ingest` would have captured them. The last line of a file without a trailing
/// newline comes back without one.
fn read_lines(fs: &dyn ReadFs, path: &Path, range: LineRange) -> Result<String, RefreshError> {
    let read_err = |source| RefreshError::Read {
        path: path.to_path_buf(),
        range,
        source,
    };
    let mut reader = BufReader::new(fs.read(path)?);
    let mut lines = String::new();
    let mut line = String::new();
    for line_no in 1..=range.end_inclusive() {
        line.clear();
        if reader.read_line(&mut line).map_err(read_err)? == 0 {
            // `verify_plan` proved the range is in bounds; the file shrank since.
            return Err(read_err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("file has only {} lines", line_no - 1),
            )));
        }
        if line_no >= range.start() {
            lines.push_str(&line);
        }
    }
    Ok(lines)
}

/// Rebuild `src` with each `(span, replacement)` applied. Spans must be in
/// ascending order and non-overlapping, which is how [`refresh`] produces them.
fn splice(src: &str, edits: &[(Range<usize>, String)]) -> String {
    let mut text = String::with_capacity(src.len());
    let mut copied_up_to = 0;
    for (span, replacement) in edits {
        text.push_str(&src[copied_up_to..span.start]);
        text.push_str(replacement);
        copied_up_to = span.end;
    }
    text.push_str(&src[copied_up_to..]);
    text
}

/// Render the chunk header `@path:range` the way the file writes it, for reports.
pub fn header(path: &Path, range: LineRange) -> String {
    format!("@{}:{}:{}", path.display(), range.start(), range.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::memory::MemoryFS;

    fn fp(bytes: &[u8]) -> Fingerprint {
        Fingerprint::of(bytes)
    }

    fn fs_with(files: &[(&str, &str)]) -> MemoryFS {
        let fs = MemoryFS::new();
        for (path, content) in files {
            fs.add_file(Path::new(path), content).unwrap();
        }
        fs
    }

    fn lr(start: usize, len: usize) -> LineRange {
        LineRange::new(start.try_into().unwrap(), len.try_into().unwrap())
    }

    /// The refreshed text must parse back to the same chunk set and apply cleanly.
    fn assert_applies(text: &str, fs: &MemoryFS) {
        let format: crate::format::Format = text.parse().unwrap();
        crate::apply::verify_plan(&format.validate().unwrap(), fs).unwrap();
    }

    #[test]
    fn test_refresh_rereads_an_unedited_stale_chunk() {
        // The chunk still holds ingest-time `b`; the file now has `X`. Applying the
        // old chunk would revert the file, so its content is replaced.
        let fs = fs_with(&[("/f.txt", "a\nX\nc\nd\n")]);
        let src = format!(
            "@/f.txt:2:1 #{}\nb\n@@@\n@/f.txt:4:1 #{}\nd\n@@@\n",
            fp(b"b\n"),
            fp(b"d\n")
        );

        let result = refresh(&src, &fs).unwrap();

        assert_eq!(
            result.text,
            format!(
                "@/f.txt:2:1 #{}\nX\n@@@\n@/f.txt:4:1 #{}\nd\n@@@\n",
                fp(b"X\n"),
                fp(b"d\n")
            )
        );
        assert_eq!(
            result.refreshed,
            vec![Refreshed {
                path: PathBuf::from("/f.txt"),
                range: lr(2, 1),
                kind: RefreshKind::Reread,
                old_fingerprint: fp(b"b\n"),
                new_fingerprint: fp(b"X\n"),
                old_content: "b\n".into(),
                new_content: "X\n".into(),
            }]
        );
        assert_eq!((result.chunks, result.fingerprinted), (2, 2));
        assert_applies(&result.text, &fs);
    }

    #[test]
    fn test_refresh_keeps_an_edited_stale_chunk_and_updates_its_tag() {
        let fs = fs_with(&[("/f.txt", "a\nX\nc\n")]);
        let src = format!("@/f.txt:2:1 #{}\nB (my edit)\n@@@\n", fp(b"b\n"));

        let result = refresh(&src, &fs).unwrap();

        assert_eq!(
            result.text,
            format!("@/f.txt:2:1 #{}\nB (my edit)\n@@@\n", fp(b"X\n"))
        );
        assert_eq!(result.refreshed[0].kind, RefreshKind::KeptEdit);
        assert_eq!(result.refreshed[0].old_content, "B (my edit)\n");
        assert_eq!(result.refreshed[0].new_content, "B (my edit)\n");
        assert_applies(&result.text, &fs);
    }

    #[test]
    fn test_refresh_marks_an_already_applied_chunk() {
        // The edit `B` was already written to the file.
        let fs = fs_with(&[("/f.txt", "a\nB\nc\n")]);
        let src = format!("@/f.txt:2:1 #{}\nB\n@@@\n", fp(b"b\n"));

        let result = refresh(&src, &fs).unwrap();

        assert_eq!(
            result.text,
            format!("@/f.txt:2:1 #{}\nB\n@@@\n", fp(b"B\n"))
        );
        assert_eq!(result.refreshed[0].kind, RefreshKind::AlreadyApplied);
        assert_applies(&result.text, &fs);
    }

    #[test]
    fn test_refresh_reread_serializes_like_ingest() {
        // Rereading must escape `@` lines, handle a multi-line range, keep CRLF
        // bytes, and switch to `@@@-` when the file's last line has no newline.
        let fs = fs_with(&[
            ("/a.txt", "@decorator\r\nfn f() {}\r\n"),
            ("/b.txt", "one\ntwo"),
        ]);
        let src = format!(
            "@/a.txt:1:2 #{}\nold\nlines\n@@@\n@/b.txt:2:1 #{}\nold\n@@@\n",
            fp(b"old\nlines\n"),
            fp(b"old\n")
        );

        let result = refresh(&src, &fs).unwrap();

        assert_eq!(
            result.text,
            format!(
                "@/a.txt:1:2 #{}\n\\@decorator\r\nfn f() {{}}\r\n@@@\n@/b.txt:2:1 #{}\ntwo\n@@@-\n",
                fp(b"@decorator\r\nfn f() {}\r\n"),
                fp(b"two")
            )
        );
        assert_eq!(
            result.refreshed[0].new_content,
            "@decorator\r\nfn f() {}\r\n"
        );
        assert_eq!(result.refreshed[1].new_content, "two");
        assert_applies(&result.text, &fs);
    }

    #[test]
    fn test_refresh_preserves_everything_around_the_rewritten_chunks() {
        // Comments, a chunk listed out of sorted order, trailing header
        // whitespace, a tag with no space before it, text after `@@@`, and a CRLF
        // header all survive; only the stale tags and the unedited body change.
        let fs = fs_with(&[("/z.txt", "1\n2\n"), ("/a.txt", "p\nq\n")]);
        let src = format!(
            "note to self\n@/z.txt:1:1   #{}  \nzero\n@@@ done\nmore notes\n@/a.txt:2:1#{}\r\nQ edited\r\n@@@\n",
            fp(b"zero\n"),
            fp(b"stale")
        );

        let result = refresh(&src, &fs).unwrap();

        assert_eq!(
            result.text,
            format!(
                "note to self\n@/z.txt:1:1 #{}\n1\n@@@ done\nmore notes\n@/a.txt:2:1 #{}\r\nQ edited\r\n@@@\n",
                fp(b"1\n"),
                fp(b"q\n")
            )
        );
        assert_eq!(
            result.refreshed.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![RefreshKind::Reread, RefreshKind::KeptEdit]
        );
        assert_applies(&result.text, &fs);
    }

    #[test]
    fn test_refresh_leaves_unfingerprinted_chunks_alone() {
        let fs = fs_with(&[("/f.txt", "a\nX\n")]);
        let src = "@/f.txt:2:1\nB\n@@@\n";

        let result = refresh(src, &fs).unwrap();

        assert_eq!(result.text, src);
        assert!(result.refreshed.is_empty());
        assert_eq!((result.chunks, result.fingerprinted), (1, 0));
    }

    #[test]
    fn test_refresh_with_nothing_stale_returns_input_verbatim() {
        let fs = fs_with(&[("/f.txt", "a\nb\n")]);
        let src = format!("# keep me\n@/f.txt:2:1 #{}\nB\n@@@\n", fp(b"b\n"));

        let result = refresh(&src, &fs).unwrap();

        assert_eq!(result.text, src);
        assert!(result.refreshed.is_empty());
        assert_eq!((result.chunks, result.fingerprinted), (1, 1));
    }

    #[test]
    fn test_refresh_reports_what_it_cannot_fix() {
        // Out of bounds: no rewrite can make line 9 exist.
        let fs = fs_with(&[("/f.txt", "a\nb\n")]);
        let src = format!("@/f.txt:9:1 #{}\nB\n@@@\n", fp(b"b\n"));
        match refresh(&src, &fs).unwrap_err() {
            RefreshError::Apply(errors) => assert!(matches!(
                errors.0.as_slice(),
                [ApplyError::ChunkOutOfBounds { .. }]
            )),
            other => panic!("expected an apply error, got {other:?}"),
        }

        // A stale chunk alongside an out-of-bounds one: refresh does not half-fix.
        let src = format!(
            "@/f.txt:1:1 #{}\nA\n@@@\n@/f.txt:9:1 #{}\nB\n@@@\n",
            fp(b"stale"),
            fp(b"b\n")
        );
        assert!(matches!(
            refresh(&src, &fs).unwrap_err(),
            RefreshError::Apply(_)
        ));

        // Not a chunk file at all.
        assert!(matches!(
            refresh("@/f.txt:zero:1\nx\n@@@\n", &fs).unwrap_err(),
            RefreshError::Format(_)
        ));
    }

    #[test]
    fn test_splice_applies_ordered_edits() {
        let edits = vec![(1..2, "BB".to_string()), (3..3, "+".to_string())];
        assert_eq!(splice("abcd", &edits), "aBBc+d");
        assert_eq!(splice("abcd", &[]), "abcd");
    }
}
