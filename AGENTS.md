## Commands

- **Build**: `cargo build` (debug) / `cargo build --release` (binary at `target/release/bulked`)
- **Run**: `cargo run -- search "pattern" path` (args after `--` go to the CLI)
- **Test (all)**: `cargo test --all-features`
- **Test (single)**: `cargo test test_apply_single_chunk_replace` — substring match on the test name
- **Test (one module)**: `cargo test format::parse` / `cargo test apply::`
- **Lint**: `cargo clippy --all-features -- -D warnings` — CI treats clippy warnings as errors, so this must pass clean
- **Format**: `cargo fmt --all` (CI check: `cargo fmt --all -- --check`)
- **Type-check only**: `cargo check --all-features`

## Architecture

This is a grep-like tool built on a **hexagonal (ports & adapters) architecture**. The functional core has no I/O; all I/O is injected through traits, which is the primary test seam. Test doubles are compiled only under `#[cfg(test)]` and never ship in the binary.

### The ports (traits) and their adapters

`Searcher<FS, M, W>` (`src/searcher.rs`) is the generic functional core. It depends only on these traits:

| Trait (`port`) | Production adapter | Test double |
|---|---|---|
| `ReadFs` / `WriteFs` (`filesystem/mod.rs`) | `PhysicalFS` (real `std::fs`) | `MemoryFS` (in-memory, `#[cfg(test)]`) |
| `Matcher` (`matcher/mod.rs`) | `GrepMatcher` (wraps the `grep` crate) | `StubMatcher` (canned matches) |
| `Walker` (`walker/mod.rs`) | `IgnoreWalker` (`.gitignore`-aware via `ignore` crate) | `SimpleWalker` (fixed path list) |

The filesystem port is split by direction. `ReadFs` (`read` → streaming `Box<dyn Read>`, `as_real_path`) is all that search, ingest, and apply's verification phase need; `WriteFs` (`writer` → streaming `Box<dyn Write>`, `rename`, `remove_file`) is what apply's write phase needs on top. `FileSystem: ReadFs + WriteFs` has no methods of its own and is implemented by a blanket impl for every `ReadFs + WriteFs` type, so `&dyn FileSystem` names "both sides" and upcasts to `&dyn ReadFs` where only reads are needed. Take the narrowest trait a function actually uses.

`StagingFs` (`filesystem/staging.rs`) is a journaled, transactional `FileSystem` decorator: reads delegate to the inner FS (so staged writes are never visible through `read`); every write-side call streams into a temp file **beside** its target (same device → atomic `rename(2)`) or appends a `Rename`/`Remove` entry to an in-memory journal. `commit` replays the journal in order; dropping without commit deletes the temps and discards the journal, leaving the inner FS untouched. This is how apply is all-or-nothing.

`ReadFs::as_real_path` is a performance escape hatch: when it returns `Some`, the matcher is given `Source::Path` and can search the file in place (memory-mapped grep) instead of reading it into a `String` (`Source::Content`). `MemoryFS` returns `None`, forcing the read-into-string path. `Matcher` has a single method, `search(&self, src: Source<'_>) -> Result<Vec<MatchInfo>, MatcherError>`; `GrepMatcher::compile` is an inherent constructor, not part of the trait.

`src/execute.rs` is the composition root that wires the three production adapters into a `Searcher` for the `search` subcommand. Tests instead construct a `Searcher` directly with test doubles, which is why most tests never touch the real filesystem.

### The chunk format is the spine of the whole tool

The custom text format is the data-interchange contract between the three subcommands and the user's editor:

```
@path/to/file.rs:<line>:<numlines>
<content>
@@@
```

- `@@@-` instead of `@@@` marks "no trailing newline at EOF". It is **derived** from the content on output (`types::write_body`, shared by `Display` and `chunk_body`), not stored as a flag.
- Only the *start* of a content line is escaped (`format/escaping.rs`): a line starting with `@`, `\@` or `\\` is written with one extra leading `\`, and on parse a leading `\@`/`\\` drops its first `\`. Nothing mid-line is escaped. An unescaped line starting with `@` inside a chunk is a parse error (`FormatError::UnescapedAtLine`), never content — this is what catches a deleted `@@@`.
- Text outside delimiters is treated as comments and ignored on parse.
- The format is round-trippable: serialize → hand-edit → parse must preserve content.

`LineRange` (`format/range.rs`) owns all line-range arithmetic: a 1-indexed `start` and a `len`, both `NonZeroUsize`, with `end_inclusive`/`end_exclusive`/`overlaps`. No `usize` line arithmetic should live outside it; zero line numbers and zero lengths are unrepresentable.

`Chunk` (`format/types.rs`) is the central data structure: private `path`, `range: LineRange`, `content`, and optional `match_range`, reached through accessors (`path()`, `range()`, `start_line()`, `num_lines()`, `content()`). `Format` wraps a **private, always-sorted** `Vec<Chunk>` (sorted by `(path, range)` on construction; `iter()`, `len()`, `file_chunks()` group by path). It serializes via `Display`/`display(plain, highlight)` and parses via `FromStr` → the nom parser in `format/parse.rs`, which produces rich `miette` diagnostics with source spans (a zero line number or length is a parse error with a span).

### Data flow (the subcommands in `src/cli/`)

All of them converge on the same `Format`/`Chunk` types. Each subcommand's `*Args` struct has a `run(self, …)` core that takes its filesystem, input, and output sinks as parameters (`&dyn FileSystem`, `&mut dyn Read`, `&mut dyn Write`) and owns all behavior, plus a one-line `handle(self, global: GlobalArgs)` that supplies `PhysicalFS`, stdin, stdout, stderr, and resolves the global flags. `GlobalArgs` (`cli/mod.rs`, flattened into `Cli`, every field `global = true` so it may follow the subcommand) holds `verbose` and `color`; the grep-style `--color auto|always|never` is resolved against the sink the colored text goes to with `ColorChoice::enabled(sink_is_terminal)`, so `auto` never colors a file. `std::fs`, `stdin()`, and `stdout()` appear only in `handle` (and `cli/mod.rs`). `integration_tests.rs` drives `ApplyArgs::run` and `IngestArgs::run` end to end on a `MemoryFS`.

- **search** → `Execute` walks + matches files → `MatchResult`s (`types.rs`; the match line plus `context_before`/`context_after` as raw newline-terminated `String`s) → `Format::from_matches` builds `Chunk`s (match line + context) → written as the format. `search` keeps `Execute` as its composition root; only its output is injected.
- **ingest** (`src/ingest.rs`) → reads `(path, line)` locations from stdin/file in **jsonl / json / csv / grep** formats (auto-detected in `cli/ingest.rs` by sniffing the first bytes; decoders deserialize straight into `types::IngestInput`, decode failures are `cli::ingest::IngestParseError`, the grep decoder yields `Decoded::Skipped(line)` for lines with no `path:line`; `IngestArgs::run` prints one summary plus a shape-based hint — forgotten `-n`, forgotten `-H`, `--heading`, `rg --json`, bulked's own chunk format — to stderr, and fails with `IngestParseError::NoLocations` when nothing at all decoded), reads context lines around each location through `ReadFs` → same `MatchResult` → `Format` output. This lets you pipe arbitrary tool output (e.g. `rg -n`, compiler errors) into the editable format.
- **apply** (`src/apply.rs`) → parses an (edited) `Format` → `Format::validate()` groups chunks by path and rejects overlaps (errors are **accumulated** across all files, not fail-fast) → yields a `Plan` of per-file `FileEdits` (a proof-of-validation type: sorted, non-overlapping chunks for one path) → `verify_plan(&Plan, &dyn ReadFs)` streams every file to a sink to catch out-of-bounds chunks → `apply_plan(&Plan, &dyn FileSystem)` reconstructs each file by streaming the original and interleaving chunk content in constant memory, staged via `StagingFs` and committed atomically. `--dry-run` stops after `verify_plan` and prints a unified-diff preview via `write_preview` (one `@@ -a,b +c,d @@` hunk per chunk whose content differs from the lines it replaces, its body a real line diff from `diff::write_line_diff`, which wraps the `similar` crate and is shared with `refresh --dry-run`; identical chunks are counted as unchanged, not printed) without writing. `apply_format_streaming` only streams and can only report `ChunkOutOfBounds`/`ContentChanged`/`Io`; it cannot receive unvalidated chunks. While skipping the original lines a chunk replaces it feeds them to a `FingerprintHasher` and compares the result with the `#xxxxxxxx` fingerprint in the chunk header (`format/fingerprint.rs`, FNV-1a folded to 32 bits, written by `Format::from_matches`); a mismatch means the file changed since ingest or the chunk was already applied. The check runs in both `verify_plan` and the staging pass, so a commit only happens if the bytes actually used for reconstruction matched. Chunks without a fingerprint apply unchecked; `--force` is implemented as `Format::without_fingerprints()` before validation, not as a flag threaded through apply. `ApplyErrors` implements `miette::Diagnostic` only to attach a `help:` pointing at `refresh`/`--force` when a `ContentChanged` is present.
- **refresh** (`src/refresh.rs`) → `parse::parse_format_with_spans` parses the `.bk` and also returns one `ChunkSpans` per chunk in source order (`tag`: the byte span after `<numlines>` up to the line ending, excluding a CRLF's `\r`; `body`: from the first content byte through the `@@@`/`@@@-` token) → `Format::validate` → `verify_plan`; every `ContentChanged` it reports marks a stale chunk. Per stale chunk the fingerprint decides what to do: content hashing to the *old* header hash means unedited → the lines are reread through `ReadFs` and both body (via `types::chunk_body`, the same serializer `Display` uses) and tag are replaced (`RefreshKind::Reread`); otherwise the content is kept and only the tag is replaced (`KeptEdit`, or `AlreadyApplied` when the content hashes to what the file has now). Only those spans are spliced, so comments, chunk order, escaping, and line endings elsewhere survive byte for byte; any other `ApplyError` (overlap, out of bounds, I/O) is returned as-is because no rewrite can fix it. `RefreshArgs::run` rewrites `PATH` in place through a `StagingFs` (exit 1 and no write if nothing was stale), writes to `-o` if given, or filters stdin to stdout; `--dry-run` prints an old-vs-new chunk diff to stdout instead of writing. Status always goes to stderr.

### Error handling conventions

- Library modules define their own `thiserror` enum (`SearchError`, `MatcherError`, `FilesystemError`, `ApplyError` accumulated into `ApplyErrors`, `IngestError`, `FormatError`); the CLI's input decoders use `cli::ingest::IngestParseError`.
- `cli::Error` (`cli/error.rs`) is the root type that `#[from]`-converts all of them and derives `miette::Diagnostic` (the `Format` variant is `#[diagnostic(transparent)]`); `main.rs` renders it as a `miette::Report` to **stderr** (the handler is installed by `cli::run` once `--color` is known, resolved against stderr) and exits 2. `cli::run` returns a `cli::Exit` (grep convention: 0 = output produced, 1 = nothing produced — no matches, no locations, empty input) which `main.rs` turns into the process exit code. Logs also go to **stderr** via `tracing` (`cli/mod.rs` installs the subscriber with `.with_writer(std::io::stderr)`). **stdout is reserved for the chunk format and status output**, so `TOOL | bulked ingest > edits.bk` never captures diagnostics.
- `FormatError` carries `miette` source spans for human-friendly parse diagnostics — preserve the span/offset bookkeeping when touching `format/parse.rs`.
