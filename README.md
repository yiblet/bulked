# Bulked

A Rust command-line tool for searching code with context and applying modifications. An enhanced grep-like tool designed for bulk code editing workflows.

## Features

### Search Command

Search for regex patterns in files recursively with rich context:

- Shows configurable lines of context (default: 20 lines) before and after each match
- Colored output with highlighted matches
- Respects `.gitignore` files by default
- Can include/exclude hidden files
- Two output formats:
  - **Plain text**: Human-readable format with line numbers and context markers
  - **Structured format**: Machine-readable format for piping to the apply command

### Apply Command

Apply modifications back to the filesystem:

- Takes structured format output from search command
- Supports dry-run mode to preview changes without applying
- Can read from stdin or from a file
- Batch applies changes across multiple files

## Installation

```bash
cargo build --release
```

The binary will be available at `target/release/bulked`.

## Usage

### Search for patterns

Basic search with default context (20 lines):
```bash
bulked search "pattern" /path/to/search
```

Custom context lines:
```bash
bulked search "pattern" /path/to/search -C 10
```

Plain text output (human-readable):
```bash
bulked search "pattern" /path/to/search --plain
```

Include hidden files:
```bash
bulked search "pattern" /path/to/search --hidden
```

Don't respect .gitignore:
```bash
bulked search "pattern" /path/to/search --no-ignore
```

### Apply modifications

Preview changes (dry-run):
```bash
bulked apply -i changes.txt --dry-run
```

Apply changes from file:
```bash
bulked apply -i changes.txt
```

Apply changes from stdin:
```bash
bulked search "pattern" | bulked apply
```

### Workflow Example

1. **Search**: Find code patterns and save results
```bash
bulked search "TODO" . > todos.txt
```

2. **Edit**: Manually edit the structured format output to make desired changes

3. **Apply**: Apply the changes back to files
```bash
bulked apply -i todos.txt
```

Or use dry-run first to preview:
```bash
bulked apply -i todos.txt --dry-run
```

## Options

### Global Options

- `-v, --verbose`: Enable verbose logging

### Search Options

- `pattern`: Regex pattern to search for (required)
- `path`: Directory or file to search (default: current directory)
- `-C, --context <LINES>`: Lines of context before and after each match (default: 20)
- `--no-ignore`: Don't respect .gitignore files
- `--hidden`: Include hidden files in search
- `--plain`: Output as plain text (human-readable format)

### Apply Options

- `-i, --input <FILE>`: Input file containing the format to apply (reads from stdin if not specified)
- `--dry-run`: Preview changes without applying them

## Use Cases

- **Bulk code refactoring**: Search for patterns, edit results, apply changes across multiple files
- **Code review**: Find and examine code patterns with surrounding context
- **TODO management**: Search for TODOs and track them with context
- **Pattern analysis**: Understand how certain patterns are used throughout a codebase

## License

See LICENSE file for details.
