# Bulked

**Bulked** (short for **Bulk Editor**) is a Rust command-line tool that lets you
edit many files at once by treating search results as a single editable
document. You collect the lines you want to change, edit them as one plain-text
file, then apply the edits back to every source file in one shot.

## The core flow

The heart of bulked is two commands — **`ingest`** and **`apply`**:

1. **`bulked ingest`** — collect the lines you want to edit and print them, with
   context, as an editable text format. Pipe in the output of *any* tool
   (`grep`, ripgrep, compiler/linter errors, a CSV of locations, …), or use
   `bulked search` to find matches yourself.
2. **Edit** — open that text in your editor (or pipe it through a script / an
   LLM) and change the content however you like.
3. **`bulked apply`** — feed the edited text back to bulked. It validates the
   chunks and writes every change back to the right place in every file at once.

```bash
# 1. Find the lines you care about and save them to a file
grep -rn 'TODO' src/ | bulked ingest > edits.bk

# 2. Edit edits.bk in your editor — change the content inside the chunks

# 3. Preview, then apply your changes back to the files
bulked apply --input edits.bk --dry-run
bulked apply --input edits.bk
```

## The chunk format

`ingest`, `search`, and `apply` all speak the same plain-text format. Each
editable block is a `chunk`:

```
@src/main.rs:10:3
fn main() {
    println!("hello");
}
@@@
```

- The header is `@<path>:<start-line>:<num-lines>`.
- Edit the lines between the header and the closing `@@@`.
- Everything outside chunks is treated as comments and ignored on apply, so
  notes you leave in the file are harmless.
- Use `@@@-` instead of `@@@` to mean "no trailing newline at end of file".
- Inside content, write `\@` for a literal `@` and `\\` for a literal `\`.
- You may add, remove, or change lines freely inside a chunk — the line count in
  the header describes the *original* lines being replaced.

Conventionally these files use the `.bk` extension.

## Installation

```bash
cargo build --release
```

The binary will be available at `target/release/bulked`.

## Commands

Every command has detailed built-in help — run `bulked --help`, or
`bulked <command> --help` for examples and the full option list.

### `ingest` — locations in, editable chunks out

`ingest` reads a list of `(path, line)` locations and, for each one, prints the
surrounding lines as an editable chunk. This is how you get the output of any
tool into bulked.

```bash
# plain `grep -n` style output straight into the editable format
grep -rn 'TODO' src/ | bulked ingest > edits.bk

# write to a file with -o (instead of redirecting)
grep -rn 'TODO' src/ | bulked ingest -o edits.bk

# a CSV exported from somewhere else, 5 lines of context
bulked ingest locations.csv -C 5 -o edits.bk

# a JSON array of {"path", "line"} objects
bulked ingest --format json locations.json -o edits.bk
```

Input formats are auto-detected (override with `--format`):

| Format | Description |
|---|---|
| `jsonl` | one JSON object per line, e.g. `{"path":"src/a.rs","line":12}` |
| `json`  | a JSON array of those same objects |
| `csv`   | a header row naming a path column and a line column, then rows |
| `grep`  | classic `path:line:...` lines, e.g. `grep -n` / `rg -n` output |

### `search` — find matches yourself

`search` is a grep-like recursive search that emits the same editable chunk
format. It's the self-contained way to start a bulk edit when you want bulked to
do the finding. By default it respects `.gitignore`, skips hidden files, and
skips bulked's own `.bk` output files (so search never matches the files it
produced). You can write the result straight to a file with `-o`/`--output`.

```bash
# find matches and save the editable format (redirect, or -o)
bulked search 'TODO' src/ > edits.bk
bulked search 'TODO' src/ -o edits.bk

# tighter context, include hidden files, ignore .gitignore
bulked search 'fn main' . -C 5 --hidden --no-ignore

# also search previously generated .bk files
bulked search 'TODO' . --include-bk

# human-readable view (not meant for `apply`)
bulked search 'TODO' src/ --plain
```

### `apply` — write the edits back

`apply` parses the (edited) chunk format and writes each change back into the
right place in each file. Before writing, every chunk is validated together
(errors are reported all at once, not one at a time): chunks must stay sorted,
must not overlap, must point at lines that exist, and must have a non-zero
length. If anything fails, nothing is written.

```bash
# preview what would change, without touching anything
bulked apply --input edits.bk --dry-run

# apply the edits from a file
bulked apply --input edits.bk

# apply edits straight from a pipe
bulked ingest locations.csv | my-edit-script | bulked apply
```

## Options

### Global

- `-v, --verbose`: Enable verbose (DEBUG-level) logging to stderr

### `ingest`

- `path`: File of locations to read (default: stdin; use `-` to force stdin)
- `-f, --format <FORMAT>`: Input format — `auto` (default), `jsonl`, `json`, `csv`, `grep`
- `-o, --output <FILE>`: Write the editable format to this file (default: stdout)
- `-C, --context <LINES>`: Lines of context before and after each location (default: 20)
- `--plain`: Print human-readable text instead of the editable chunk format

### `search`

- `pattern`: Regex pattern to search for (required)
- `path`: Directory or file to search (default: current directory)
- `-o, --output <FILE>`: Write the editable format to this file (default: stdout)
- `-C, --context <LINES>`: Lines of context before and after each match (default: 20)
- `--no-ignore`: Search files normally excluded by `.gitignore`
- `--hidden`: Include hidden files and directories in the search
- `--include-bk`: Also search bulked's own `.bk` output files (excluded by default)
- `--plain`: Print human-readable text instead of the editable chunk format

### `apply`

- `-i, --input <FILE>`: Edited chunk file to apply (reads from stdin if not specified)
- `-d, --dry-run`: Validate and report what would change, without writing any files

## Use cases

- **Bulk code refactoring**: ingest matches, edit results, apply changes across many files
- **Acting on tool output**: pipe compiler errors, linter findings, or `grep` hits into an editable form
- **LLM-driven edits**: ingest → let a model rewrite the chunks → apply
- **TODO management**: collect TODOs with context and resolve them in one pass

## License

See LICENSE file for details.
