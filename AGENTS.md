# bulked — agent notes

A grep-like bulk editor: collect lines into an editable text file of chunks,
edit it, apply it back atomically. Rust 2024, hexagonal core. This file holds
only what you cannot infer from the code: the *why*, the invariants, and the
boundaries. Doc comments and `README.md` cover the rest.

## Commands

    cargo test --all-features                       # all; `cargo test apply::` for one module
    cargo clippy --all-features -- -D warnings      # CI gate; runs WITHOUT --tests, so a
                                                    # helper only tests still use fails it
    cargo fmt --all -- --check                      # CI gate
    cargo run -- search PATTERN PATH                # args after -- go to the CLI

## Boundaries

Never
- `File::create` / `std::fs` / `stdin()` / `stdout()` / `std::env` outside
  `cli/*::handle` and `cli/mod.rs`. Library modules take `&dyn ReadFs`,
  `&dyn FileSystem`, `&mut dyn Write`; that is the test seam.
- Write a user path directly. Every output, including `-o`, goes through
  `StagingFs` via `cli::write_file_atomically` (see below).
- Print anything but chunks/status to stdout. Diagnostics, logs, hints → stderr.
- Move `master`, push, or let `jj` open an editor (load the `jj` skill first).

Ask first
- Adding a method to a port trait (`ReadFs`/`WriteFs`/`Matcher`/`Walker`).
  Policy belongs in decorators or parameters, e.g. `StagingFs::new(fs, temp_dir)`.
- Changing the chunk grammar in `format/parse.rs` or `format/escaping.rs`.

## Why things are the way they are

**Staging in `$TMPDIR`, not beside the target.** `search` walks the tree, so a
sibling temp would show up in its own output, and an interrupted run must leave
the tree clean. `PhysicalFS::rename` covers the cross-device case (tmpfs) by
copying to a sibling and renaming that, so the target still flips in one step.
`filesystem/staging.rs` module docs have the journal semantics.

**The chunk format must round-trip through a human editor.** Serialize →
hand-edit → parse preserves content. Hence: only a line's *first character* is
ever escaped; an unescaped `@` line inside a chunk is a parse error rather than
content (it catches a deleted `@@@`); `@@@-` is derived from the content, never
stored; text outside chunks is a comment and `refresh` preserves it byte for
byte by splicing `ChunkSpans` instead of reserializing. Parse errors carry
`miette` spans; keep the offset bookkeeping when you touch `format/parse.rs`.

**Types carry the proofs.** `LineRange` owns all line arithmetic (`NonZeroUsize`
start/len, so zero is unrepresentable): no `usize` line math elsewhere.
`Format` is a private, always-sorted `Vec<Chunk>`. `Plan`/`FileEdits` exist
only via `Format::validate()` and mean "grouped, sorted, non-overlapping";
nothing downstream accepts raw chunks. Errors accumulate across files
(`ApplyErrors`) instead of failing fast, because the user fixes them in one pass.

**Fingerprints make apply refuse stale edits.** `#xxxxxxxx` is FNV-1a of the
original lines, checked against the very bytes apply skips, in both the verify
and the staging pass, so a commit only happens if reconstruction saw matching
bytes. No fingerprint = unchecked. `--force` is `Format::without_fingerprints()`
before validation, not a flag threaded through apply. `refresh` uses the
fingerprint to tell an unedited chunk (content hashes to its header → reread
from the file) from an edited one (keep the edit, update the tag).

**Exit codes follow grep**: 0 output produced, 1 nothing to do, 2 error.
`--color auto|always|never` is resolved per sink in `handle`, so `auto` never
colors a file.

## Where to look

- `src/cli/*.rs` — `run()` is the testable core, `handle()` the one-line wiring.
- `src/integration_tests.rs` — the subcommands driven end to end on `MemoryFS`;
  copy its shape for new CLI tests.
- `src/execute.rs` — the only composition root for `search`.
