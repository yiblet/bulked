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

## Where to look

- `src/cli/*.rs` — `run()` is the testable core, `handle()` the one-line wiring.
- `src/integration_tests.rs` — the subcommands driven end to end on `MemoryFS`;
  copy its shape for new CLI tests.
- `src/execute.rs` — the only composition root for `search`.
