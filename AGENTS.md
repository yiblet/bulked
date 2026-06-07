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

This is a grep-like tool built on a **hexagonal (ports & adapters) architecture**. The functional core has no I/O; all I/O is injected through traits, which is the primary test seam.

### The three ports (traits) and their adapters

`Searcher<FS, M, W>` (`src/searcher.rs`) is the generic functional core. It depends only on three traits:

| Trait (`port`) | Production adapter | Test double |
|---|---|---|
| `FileSystem` (`filesystem/mod.rs`) | `PhysicalFS` (real `std::fs`) | `MemoryFS` (in-memory) |
| `Matcher` (`matcher/mod.rs`) | `GrepMatcher` (wraps the `grep` crate) | `StubMatcher` (canned matches) |
| `Walker` (`walker/mod.rs`) | `IgnoreWalker` (`.gitignore`-aware via `ignore` crate) | `SimpleWalker` (fixed path list) |

`src/execute.rs` is the composition root that wires the three production adapters into a `Searcher` for the CLI. Tests instead construct a `Searcher` directly with test doubles. This is why most tests never touch the real filesystem.

`FileSystem::as_real_path` is a performance escape hatch: when it returns `Some`, the matcher can search the path directly (`Matcher::search_path`, e.g. memory-mapped grep) instead of reading the whole file into a `String`. `MemoryFS` returns `None`, forcing the read-into-string path.

### The chunk format is the spine of the whole tool

The custom text format is the data-interchange contract between the three subcommands and the user's editor:

```
@path/to/file.rs:<line>:<numlines>
<content>
@@@
```

- `@@@-` instead of `@@@` marks "no trailing newline at EOF".
- Inside content, `@` and `\` are escaped as `\@` and `\\` (`format/escaping.rs`).
- Text outside delimiters is treated as comments and ignored on parse.
- The format is round-trippable: serialize → hand-edit → parse must preserve content.

`Chunk` (`format/types.rs`) is the central data structure. `Format` is a `Vec<Chunk>` plus serialization (`Display`) and parsing (`FromStr` → nom parser in `format/parse.rs`, which produces rich `miette` diagnostics with source spans).

### Data flow (the three subcommands in `src/cli/`)

All three converge on the same `Format`/`Chunk` types:

- **search** → `Execute` walks + matches files → `MatchResult`s → `Format::from_matches` builds `Chunk`s (match line + context) → printed as the format.
- **ingest** (`src/ingest.rs`) → reads `(path, line)` pairs from stdin/file in **jsonl / json / csv / grep** formats (auto-detected in `cli/ingest.rs` by sniffing the first bytes), reads context lines around each line, → same `MatchResult` → `Format` output. This lets you pipe arbitrary tool output (e.g. `rg --json`, compiler errors) into the editable format.
- **apply** (`src/apply.rs`) → parses an (edited) `Format` → `apply_format` validates chunks (same path, sorted, non-overlapping, in-bounds, non-zero length — errors are **accumulated**, not fail-fast) → reconstructs each file by interleaving unmodified `Content` segments with modified `Chunk` segments → writes via `FileSystem`. `--dry-run` reports what would change without writing.

`Format::file_chunks()` groups sorted chunks by path so apply can process one file at a time. `Format::merge()` (currently `#[allow(dead_code)]`) combines adjacent/overlapping chunks.

### Error handling conventions

- Library modules define their own `thiserror` enum (`SearchError`, `MatcherError`, `FilesystemError`, `ApplyError`, `IngestError`, `FormatError`).
- `cli::Error` (`cli/error.rs`) is the root type that `#[from]`-converts all of them; `main.rs` prints it to **stdout** (logs go to stderr via `tracing`) and exits 1.
- `FormatError` carries `miette` source spans for human-friendly parse diagnostics — preserve the span/offset bookkeeping when touching `format/parse.rs`.
