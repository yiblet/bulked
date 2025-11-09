# Bulked — Recursive Grep with Context Plan

## Operating Requirements

### PLANNING

1. **CONFIRM INTENT BEFORE GOING TO IMPLEMENTATION.** When you finish the draft, you MUST confirm intent and confirm that the plan is ready for implementation.
2. **SURFACE QUESTIONS AFTER DRAFTING.** When you finish the draft, you MUST list questions/concerns and point reviewers to the exact places to reviewers to look.

### EXECUTION

1. **UPDATE PHASES WITH PROGRESS CONTINUOUSLY.** As you begin or complete a phase, you MUST update the plan with what changed and which tests were added.
2. **ALWAYS TEST AND VERIFY COMPLETION.** Always test and verify completion of a phase before proceeding to the next section.
3. **CHECK TESTS AT PHASE START.** At the start of a new phase, check tests to ensure everything started by working.
4. **CREATE COMMIT PER PHASE.** Create a commit per phase at completion after you've updated the plan with a progress messsage in the document. use semantic commit message conventions for the message.

The phases below may or may not work out once you're in implementation. As always, this is a plan, and plans tend to only be 80% correct in the field. That is okay. If you find that you need to deviate from the architecture to meet the goals please continue on with your work using your best solution. Just make sure to add that info to the progress log.

---

## Problem

### Goal
Build a CLI tool called `bulked` that recursively searches directories for regex pattern matches and displays each match with 20 lines of context before and after, using the same grep/ignore infrastructure as the Helix editor.

### Success Criteria (as a test)
**Integration Test:** `test_bulked_search_with_context` uses a virtual filesystem (similar to Go's fs.FS pattern) to test search functionality. The test creates an in-memory filesystem with test files and verifies that:
- Matches are found in correct files
- Each match includes exactly 20 lines before and 20 lines after (or up to file boundaries)
- Line numbers are accurate
- Gitignored files are excluded by default

The virtual filesystem abstraction allows testing without touching the real filesystem.

### Non-Goals
- Interactive UI or TUI
- Match highlighting/coloring (phase 1)
- Replacing matches
- Performance optimizations beyond basic parallel walking
- Custom ignore patterns beyond .gitignore

### Constraints
- Must use `grep-regex` and `grep-searcher` crates (proven stable)
- Must use `ignore` crate for directory walking (respects .gitignore)
- Must handle binary file detection gracefully
- Must be testable with isolated components

---

## Discovery

### Relevant Code Map
Current codebase:
- `Cargo.toml` — Basic project with anyhow, serde dependencies
- `src/main.rs` — Empty Hello World
- `snippet.rs` — Reference implementation from Helix showing grep usage (lines 92-94)

Reference from snippet.rs:
- Uses `grep_regex::RegexMatcherBuilder` to compile patterns
- Uses `grep_searcher::SearcherBuilder` with `BinaryDetection`
- Uses `ignore::WalkBuilder` for recursive directory traversal with gitignore support
- Uses `grep_searcher::sinks` for collecting results

### Prior Examples/Patterns
From Helix snippet.rs (lines 92-94):
```rust
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{sinks, BinaryDetection, SearcherBuilder};
use ignore::{DirEntry, WalkBuilder, WalkState};
```

Pattern: Build matcher → Build searcher → Walk directories → Search each file → Collect results

### Areas of Uncertainty
1. **Context line collection:** grep_searcher has line number info but unclear if it directly provides surrounding context lines. May need to read file separately or use custom sink.
2. **Binary file handling:** Need to verify BinaryDetection behavior (follow Helix's approach).
3. **Parallel search:** grep-searcher doesn't have built-in parallelism, but ripgrep achieves it through lock-free work distribution and parallel directory traversal (not including in phase 1).

### Architectural Principles

**Hexagonal Architecture (Ports and Adapters):**
- **Functional Core**: Business logic depends only on abstract interfaces (traits)
- **Imperative Shell**: I/O adapters implement interfaces, injected at runtime
- **Seams**: Every interface boundary is a test seam

**Dependency Flow:**
```
┌─────────────────────────────────────┐
│   Imperative Shell (Adapters)      │
│  - PhysicalFileSystem               │
│  - GrepMatcher                      │
│  - IgnoreWalker                     │
│  - MemoryFileSystem (test)          │
│  - StubMatcher (test)               │
└───────────┬─────────────────────────┘
            │ implements
            ↓
┌─────────────────────────────────────┐
│   Ports (Abstract Interfaces)      │
│  - FileSystem trait                 │
│  - Matcher trait                    │
│  - Walker trait                     │
└───────────┬─────────────────────────┘
            │ depends on
            ↓
┌─────────────────────────────────────┐
│   Functional Core (Business Logic)  │
│  - Searcher                         │
│  - ContextExtractor                 │
│  - ResultFormatter                  │
└─────────────────────────────────────┘
```

**Testing Strategy:**
1. **Solitary Unit Tests**: Test each core module with all dependencies as test doubles
2. **Sociable Integration Tests**: Test multiple modules together with controlled fakes
3. **Contract Tests**: Test that each adapter satisfies its trait contract
4. **End-to-End Tests**: Full CLI test with MemoryFileSystem

### Architecture: Module Interfaces (OCaml-style .mli)

**Note:** Using OCaml `.mli` syntax for clarity, pretending OCaml has Rust-style trait inheritance.

#### Core Types (`src/types.mli`)
```ocaml
(* Core domain types - no I/O dependencies *)

type path = string

type match_result = {
  file_path: path;
  line_number: int;  (* 1-indexed *)
  line_content: string;
  byte_offset: int;
  context_before: context_line list;
  context_after: context_line list;
}

and context_line = {
  line_number: int;
  content: string;
}

type search_error =
  | FileReadError of { path: path; error: string }
  | BinaryFileSkipped of path
  | PatternError of string

type search_config = {
  pattern: string;
  root_path: path;
  context_lines: int;
  respect_gitignore: bool;
}

type search_result = {
  matches: match_result list;
  errors: search_error list;
}
```

#### FileSystem Port (`src/filesystem.mli`)
```ocaml
(* Abstract filesystem interface - the primary test seam *)

module type FILESYSTEM = sig
  type t
  type file_handle

  (* File operations *)
  val open_file : t -> path -> (file_handle, string) result
  val read_lines : file_handle -> string list
  val read_line_at : file_handle -> int -> (string, string) result
  val read_line_range : file_handle -> int -> int -> (string list, string) result
  val close : file_handle -> unit

  (* Metadata *)
  val exists : t -> path -> bool
  val is_file : t -> path -> bool
  val is_binary : t -> path -> bool

  (* For testing *)
  val create : unit -> t
end

(* Concrete implementations - adapters *)
module PhysicalFS : FILESYSTEM
module MemoryFS : FILESYSTEM

(* Test helper for MemoryFS *)
module MemoryFSBuilder : sig
  val create : unit -> MemoryFS.t
  val add_file : MemoryFS.t -> path -> string -> unit
  val add_binary_file : MemoryFS.t -> path -> bytes -> unit
end
```

#### Matcher Port (`src/matcher.mli`)
```ocaml
(* Abstract pattern matching interface *)

module type MATCHER = sig
  type t
  type match_position = {
    line: int;
    start_byte: int;
    end_byte: int;
  }

  val compile : string -> (t, string) result
  val find_matches : t -> string -> match_position list
  val is_match : t -> string -> bool
end

(* Adapters *)
module GrepMatcher : MATCHER  (* Uses grep-regex *)
module StubMatcher : MATCHER  (* For testing - returns predefined matches *)
```

#### Walker Port (`src/walker.mli`)
```ocaml
(* Abstract directory traversal interface *)

module type WALKER = sig
  type t
  type walk_entry = {
    path: path;
    is_file: bool;
    is_dir: bool;
  }

  val create :
    fs:('fs module type FILESYSTEM) ->
    root:path ->
    respect_gitignore:bool ->
    t

  val walk : t -> walk_entry Seq.t  (* Lazy sequence of entries *)
end

(* Adapters *)
module IgnoreWalker : WALKER     (* Uses ignore crate *)
module SimpleWalker : WALKER     (* For testing - walks MemoryFS *)
```

#### Searcher Core (`src/searcher.mli`)
```ocaml
(* Functional core - no I/O, depends only on abstract interfaces *)

module Make
  (FS : FILESYSTEM)
  (M : MATCHER)
  (W : WALKER) : sig

  type t = {
    fs: FS.t;
    matcher: M.t;
    walker: W.t;
    config: search_config;
  }

  val create :
    fs:FS.t ->
    matcher:M.t ->
    walker:W.t ->
    config:search_config ->
    t

  (* Pure functions - testable without I/O *)
  val search_file : t -> path -> (match_result list, search_error) result
  val search_all : t -> search_result
end
```

#### Context Extractor Core (`src/context.mli`)
```ocaml
(* Functional core for extracting context lines *)

module Make (FS : FILESYSTEM) : sig
  type t = {
    fs: FS.t;
    context_lines: int;
  }

  val create : fs:FS.t -> context_lines:int -> t

  (* Pure functions *)
  val extract_context :
    t ->
    path ->
    int (* match line *) ->
    (context_line list * context_line list, string) result

  val extract_all_contexts :
    t ->
    path ->
    int list (* match lines *) ->
    ((int * (context_line list * context_line list)) list, string) result
end
```

#### Result Formatter (`src/formatter.mli`)
```ocaml
(* Pure formatting logic - no I/O *)

type format_config = {
  show_line_numbers: bool;
  show_file_paths: bool;
  separator: string;
}

val format_match : format_config -> match_result -> string
val format_results : format_config -> search_result -> string
val format_error : search_error -> string
```

#### Main Orchestrator (`src/lib.mli`)
```ocaml
(* High-level API - wires everything together *)

(* Production entry point - uses real adapters *)
val search : search_config -> search_result

(* Testable entry point - accepts any implementations *)
module SearchWith
  (FS : FILESYSTEM)
  (M : MATCHER)
  (W : WALKER) : sig
  val search : fs:FS.t -> search_config -> search_result
end
```

### Sketch: Dependency Injection Flow

```
main.ml (imperative shell)
  ↓
  creates: PhysicalFS, GrepMatcher, IgnoreWalker
  ↓
  injects into: Searcher.Make(PhysicalFS)(GrepMatcher)(IgnoreWalker)
  ↓
  calls: search_all
  ↓
  returns: search_result (pure data)
  ↓
  formats and prints


test.ml (test suite)
  ↓
  creates: MemoryFS, StubMatcher, SimpleWalker
  ↓
  injects into: Searcher.Make(MemoryFS)(StubMatcher)(SimpleWalker)
  ↓
  calls: search_all
  ↓
  asserts on: search_result
```

### Test Double Strategy

**Fakes (Working Implementations):**
- `MemoryFS` - Full filesystem in memory
- `SimpleWalker` - Naive directory walker for MemoryFS

**Stubs (Canned Responses):**
- `StubMatcher` - Returns predefined match positions
- `StubFS` - Returns specific file contents

**Mocks (Behavior Verification):**
- `MockFS` - Records which files were accessed
- `MockMatcher` - Counts pattern compilation calls

**Strategy:**
- Prefer fakes for most tests (MemoryFS is fast and complete)
- Use stubs for edge cases (simulating specific error conditions)
- Use mocks sparingly (only when behavior verification is critical)

---

## Build Out

### Phase 1 — Core Search Engine with Abstractions (Matcher + Searcher + Walker)
**Status:** ✅ Complete

**Progress:**
- Created all trait abstractions (FileSystem, Matcher, Walker)
- Implemented production adapters (PhysicalFS, GrepMatcher, IgnoreWalker)
- Implemented test adapters (MemoryFS, StubMatcher, SimpleWalker)
- Implemented Searcher core with generic trait bounds
- All 42 unit/integration tests passing + 1 doctest
- Tests demonstrate: solitary unit tests, contract tests, sociable integration tests
- CLI functional with basic search capability

#### A) Feature Slice
Implement three core components as independent, testable modules with trait abstractions for test doubles:
1. **Matcher module** — Wraps grep_regex for pattern compilation
2. **FileSystem abstraction** — Trait over file I/O, backed by `vfs` crate (MemoryFS for tests, PhysicalFS for production)
3. **Walker module** — Wraps ignore crate for directory traversal, uses FileSystem trait
4. **Searcher module** — Wraps grep_searcher to find matches, uses FileSystem trait

**Key Architecture Decision:** All filesystem operations go through a `FileSystem` trait, allowing us to:
- Unit test each module in isolation with MemoryFS
- Create test doubles for integration testing across modules
- Test without touching the real filesystem

At this phase, no CLI interface yet. Components return structured data (file paths, line numbers, match positions). No context extraction yet — just match positions.

#### B) Detailed Design

**Dependencies** (add to `Cargo.toml`):
```toml
[dependencies]
vfs = "0.12"  # Virtual filesystem abstraction
grep-regex = "0.1"
grep-searcher = "0.1"
ignore = "0.4"
tracing = "0.1"  # Structured logging
```

**Module Structure:**
```
src/
  types.rs          - Core domain types (no I/O)
  filesystem/
    mod.rs          - FileSystem trait definition
    physical.rs     - PhysicalFS adapter
    memory.rs       - MemoryFS adapter (uses vfs crate)
  matcher/
    mod.rs          - Matcher trait definition
    grep.rs         - GrepMatcher adapter
    stub.rs         - StubMatcher for tests
  walker/
    mod.rs          - Walker trait definition
    ignore.rs       - IgnoreWalker adapter
    simple.rs       - SimpleWalker for tests
  searcher.rs       - Core search logic (functional)
  lib.rs            - Public API
```

**Core Types Implementation** (`src/types.rs`):
```ocaml
(* Simplified from architecture section - just the essentials for Phase 1 *)

type path = string

type match_result = {
  file_path: path;
  line_number: int;
  line_content: string;
  byte_offset: int;
  (* context fields added in Phase 2 *)
}

type search_error =
  | FileReadError of { path: path; error: string }
  | BinaryFileSkipped of path
  | PatternError of string

type search_config = {
  pattern: string;
  root_path: path;
  respect_gitignore: bool;
}

type search_result = {
  matches: match_result list;
  errors: search_error list;
}
```

**FileSystem Trait** (`src/filesystem/mod.rs` pseudocode):
```ocaml
module type FILESYSTEM = sig
  type t
  type file_handle

  (* Minimal operations needed for Phase 1 *)
  val read_to_string : t -> path -> (string, string) result
  val is_binary : t -> path -> bool
  val exists : t -> path -> bool

  val create : unit -> t
end
```

**Matcher Trait** (`src/matcher/mod.rs` pseudocode):
```ocaml
module type MATCHER = sig
  type t

  type match_info = {
    line_num: int;
    byte_offset: int;
    line_content: string;
  }

  val compile : string -> (t, string) result
  val search_in_file : t -> string (* file contents *) -> match_info list
end
```

**Walker Trait** (`src/walker/mod.rs` pseudocode):
```ocaml
module type WALKER = sig
  type t

  val create : root:path -> respect_gitignore:bool -> t
  val files : t -> path list  (* Returns all files to search *)
end
```

**Searcher Core** (`src/searcher.rs` pseudocode):
```ocaml
(* Parameterized by abstract dependencies *)
module Make (FS : FILESYSTEM) (M : MATCHER) (W : WALKER) = struct
  type t = {
    fs: FS.t;
    matcher: M.t;
    walker: W.t;
    config: search_config;
  }

  let create ~fs ~matcher ~walker ~config =
    { fs; matcher; walker; config }

  let search_file t path =
    match FS.read_to_string t.fs path with
    | Error err -> Error (FileReadError { path; error = err })
    | Ok contents ->
        if FS.is_binary t.fs path then
          Error (BinaryFileSkipped path)
        else
          let matches = M.search_in_file t.matcher contents in
          Ok (List.map (fun m -> {
            file_path = path;
            line_number = m.line_num;
            line_content = m.line_content;
            byte_offset = m.byte_offset;
          }) matches)

  let search_all t =
    let files = W.files t.walker in
    let results = List.map (search_file t) files in
    let (matches, errors) = partition_results results in
    { matches; errors }
end
```

**Error Handling:**
- Invalid regex pattern → Log with `tracing::warn!`, return PatternError
- Invalid path → Log with `tracing::warn!`, return FileReadError
- Unreadable file → Log with `tracing::debug!`, add to errors list, continue
- Binary file → Log with `tracing::debug!`, add to errors list, continue

#### C) Testing Plan (Unit & Integration Tests)

**Test Organization:**
```
tests/
  unit/
    matcher_test.rs      - Solitary tests for Matcher
    walker_test.rs       - Solitary tests for Walker
    filesystem_test.rs   - Contract tests for FileSystem
    searcher_test.rs     - Solitary tests for Searcher core
  integration/
    search_integration_test.rs  - Sociable tests with multiple modules
  fixtures/
    test_helpers.rs      - Test double builders
```

**1. Solitary Unit Tests (Each Module Isolated)**

**Test:** `tests/unit/matcher_test.rs::test_grep_matcher_compiles_valid_pattern`
```ocaml
(* Test GrepMatcher in isolation *)
let test_grep_matcher_compiles_valid_pattern () =
  let result = GrepMatcher.compile "foo.*bar" in
  match result with
  | Ok matcher ->
      assert (GrepMatcher.is_match matcher "fooXXXbar");
      assert (not (GrepMatcher.is_match matcher "baz"))
  | Error _ -> failwith "Expected successful compilation"
```

**Test:** `tests/unit/matcher_test.rs::test_grep_matcher_rejects_invalid_pattern`
```ocaml
let test_grep_matcher_rejects_invalid_pattern () =
  let result = GrepMatcher.compile "[unclosed" in
  match result with
  | Ok _ -> failwith "Should have rejected invalid pattern"
  | Error msg -> assert (String.contains msg "unclosed")
```

**Test:** `tests/unit/walker_test.rs::test_simple_walker_with_memory_fs`
```ocaml
(* Test SimpleWalker with MemoryFS - both test doubles, no real I/O *)
let test_simple_walker_with_memory_fs () =
  let fs = MemoryFS.create () in
  MemoryFS.add_file fs "/root/file1.txt" "content1";
  MemoryFS.add_file fs "/root/subdir/file2.txt" "content2";
  MemoryFS.add_file fs "/root/.gitignore" "ignored.txt";
  MemoryFS.add_file fs "/root/ignored.txt" "should be ignored";

  let walker = SimpleWalker.create ~fs ~root:"/root" ~respect_gitignore:true in
  let files = SimpleWalker.files walker in

  assert_equal 2 (List.length files);
  assert (List.mem "/root/file1.txt" files);
  assert (List.mem "/root/subdir/file2.txt" files);
  assert (not (List.mem "/root/ignored.txt" files))
```

**Test:** `tests/unit/searcher_test.rs::test_searcher_with_all_test_doubles`
```ocaml
(* Test Searcher core logic in complete isolation - all deps are fakes *)
let test_searcher_with_all_test_doubles () =
  (* Setup test doubles *)
  let fs = MemoryFS.create () in
  MemoryFS.add_file fs "/test/foo.txt" "line 1\nTARGET line\nline 3";

  let stub_matcher = StubMatcher.create () in
  StubMatcher.set_match stub_matcher {
    line_num = 2;
    byte_offset = 7;
    line_content = "TARGET line";
  };

  let stub_walker = StubWalker.create () in
  StubWalker.set_files stub_walker ["/test/foo.txt"];

  (* Create searcher with test doubles *)
  module TestSearcher = Searcher.Make(MemoryFS)(StubMatcher)(StubWalker) in
  let searcher = TestSearcher.create
    ~fs
    ~matcher:stub_matcher
    ~walker:stub_walker
    ~config:{ pattern = "TARGET"; root_path = "/test"; respect_gitignore = false }
  in

  (* Execute search *)
  let result = TestSearcher.search_all searcher in

  (* Assertions *)
  assert_equal 1 (List.length result.matches);
  let m = List.hd result.matches in
  assert_equal "/test/foo.txt" m.file_path;
  assert_equal 2 m.line_number;
  assert_equal "TARGET line" m.line_content;
  assert_equal [] result.errors
```

**Test:** `tests/unit/filesystem_test.rs::test_memory_fs_handles_binary`
```ocaml
let test_memory_fs_handles_binary () =
  let fs = MemoryFS.create () in
  MemoryFS.add_binary_file fs "/test/image.png" (Bytes.of_string "\x89PNG\r\n");

  assert (MemoryFS.is_binary fs "/test/image.png");
  assert (MemoryFS.exists fs "/test/image.png")
```

**2. Contract Tests (Verify Adapters Satisfy Traits)**

**Test:** `tests/unit/filesystem_test.rs::test_memory_fs_satisfies_filesystem_contract`
```ocaml
(* Polymorphic test - works for ANY FileSystem implementation *)
module TestFSContract (FS : FILESYSTEM) = struct
  let test_read_existing_file () =
    let fs = FS.create () in
    (* Setup depends on implementation - use builder pattern *)
    match FS.read_to_string fs "/test/file.txt" with
    | Ok content -> assert (String.length content > 0)
    | Error _ -> failwith "Should read existing file"

  let test_read_nonexistent_file () =
    let fs = FS.create () in
    match FS.read_to_string fs "/nonexistent.txt" with
    | Ok _ -> failwith "Should error on nonexistent file"
    | Error _ -> () (* Expected *)

  let test_binary_detection () =
    let fs = FS.create () in
    (* Both text and binary files should be detectable *)
    assert (FS.exists fs "/test/text.txt");
    assert (FS.exists fs "/test/binary.bin")
end

(* Run contract tests for each implementation *)
module TestMemoryFS = TestFSContract(MemoryFS)
module TestPhysicalFS = TestFSContract(PhysicalFS)
```

**3. Sociable Integration Tests (Multiple Real Modules)**

**Test:** `tests/integration/search_integration_test.rs::test_searcher_with_real_grep_and_memory_fs`
```ocaml
(* Test Searcher + GrepMatcher together, MemoryFS for I/O control *)
let test_searcher_with_real_grep_and_memory_fs () =
  (* Use REAL GrepMatcher, FAKE MemoryFS, FAKE SimpleWalker *)
  let fs = MemoryFS.create () in
  MemoryFS.add_file fs "/src/main.rs"
    "fn main() {\n    println!(\"hello\");\n}\n";
  MemoryFS.add_file fs "/src/lib.rs"
    "pub fn hello() {\n    println!(\"hello\");\n}\n";

  let matcher = match GrepMatcher.compile "hello" with
    | Ok m -> m
    | Error e -> failwith e
  in

  let walker = SimpleWalker.create_with_files ["/src/main.rs"; "/src/lib.rs"] in

  module IntegrationSearcher = Searcher.Make(MemoryFS)(GrepMatcher)(SimpleWalker) in
  let searcher = IntegrationSearcher.create
    ~fs ~matcher ~walker
    ~config:{ pattern = "hello"; root_path = "/src"; respect_gitignore = false }
  in

  let result = IntegrationSearcher.search_all searcher in

  (* Should find "hello" in both files *)
  assert_equal 2 (List.length result.matches);
  assert (List.exists (fun m -> m.file_path = "/src/main.rs") result.matches);
  assert (List.exists (fun m -> m.file_path = "/src/lib.rs") result.matches)
```

**Test:** `tests/integration/search_integration_test.rs::test_full_stack_with_memory_fs`
```ocaml
(* Integration test using ALL real implementations except filesystem *)
let test_full_stack_with_memory_fs () =
  let fs = MemoryFS.create () in
  (* Create realistic directory structure *)
  MemoryFS.add_file fs "/project/.gitignore" "*.tmp\ntarget/\n";
  MemoryFS.add_file fs "/project/src/main.rs" "fn main() {}\n";
  MemoryFS.add_file fs "/project/src/lib.rs" "pub fn lib() {}\n";
  MemoryFS.add_file fs "/project/test.tmp" "temporary\n";
  MemoryFS.add_file fs "/project/target/debug/app" "binary\n";

  (* Use REAL implementations *)
  let matcher = GrepMatcher.compile "fn " |> Result.get_ok in
  let walker = SimpleWalker.create ~fs ~root:"/project" ~respect_gitignore:true in

  module FullSearcher = Searcher.Make(MemoryFS)(GrepMatcher)(SimpleWalker) in
  let searcher = FullSearcher.create ~fs ~matcher ~walker
    ~config:{ pattern = "fn "; root_path = "/project"; respect_gitignore = true }
  in

  let result = FullSearcher.search_all searcher in

  (* Should find matches in .rs files, not in .gitignore'd files *)
  assert (List.length result.matches >= 2);
  assert (not (List.exists (fun m -> String.contains m.file_path ".tmp") result.matches));
  assert (not (List.exists (fun m -> String.contains m.file_path "target") result.matches))
```

**4. Test Helpers** (`tests/fixtures/test_helpers.rs`)

```ocaml
(* Builder pattern for creating complex test scenarios *)
module MemoryFSBuilder = struct
  type t = MemoryFS.t

  let create () = MemoryFS.create ()

  let with_rust_project fs =
    MemoryFS.add_file fs "/Cargo.toml" "[package]\nname = \"test\"\n";
    MemoryFS.add_file fs "/src/main.rs" "fn main() {}\n";
    MemoryFS.add_file fs "/.gitignore" "target/\n";
    fs

  let with_gitignore fs patterns =
    MemoryFS.add_file fs "/.gitignore" (String.concat "\n" patterns);
    fs

  let with_file fs path content =
    MemoryFS.add_file fs path content;
    fs
end

module StubMatcherBuilder = struct
  let create () = StubMatcher.create ()

  let with_matches stub matches =
    List.iter (StubMatcher.add_match stub) matches;
    stub

  let always_match stub =
    StubMatcher.set_predicate stub (fun _ -> true);
    stub
end
```

**Summary of Test Coverage:**

| Module | Solitary Unit | Contract | Sociable Integration |
|--------|---------------|----------|---------------------|
| GrepMatcher | ✓ Isolated | ✓ MATCHER contract | ✓ With Searcher |
| MemoryFS | ✓ Isolated | ✓ FILESYSTEM contract | ✓ With Searcher |
| SimpleWalker | ✓ Isolated | ✓ WALKER contract | ✓ With Searcher |
| Searcher | ✓ All deps stubbed | N/A | ✓ Multiple combos |

**Assertions:**
- Use `assert_equal` for exact matches
- Use `assert` for boolean conditions
- Use `assert_raises` for error cases
- All tests must be deterministic (no timing, no randomness, no external state)

---

### Phase 2 — Context Extraction
**Status:** Planned

#### A) Feature Slice
Add context extraction capability to get N lines before and after each match. Extend `match_result` type to include context lines. Handle edge cases: file start/end boundaries, empty files, single-line files.

**Architectural Approach:** Context extraction is a pure function that takes file contents and match positions, returns context lines. It depends only on the FileSystem trait for reading line ranges. Overlapping contexts (when matches are close) are shown twice (simpler, per user feedback).

This phase makes the tool actually useful by showing surrounding code. Still no CLI — testing through direct function calls with MemoryFS.

#### B) Detailed Design

**Types/Schema Updates** (`src/search.rs`):
```rust
pub struct Match {
    pub file_path: PathBuf,
    pub line_number: usize,
    pub line_content: String,
    pub byte_offset: usize,
    pub context_before: Vec<ContextLine>,  // NEW
    pub context_after: Vec<ContextLine>,   // NEW
}

pub struct ContextLine {
    pub line_number: usize,
    pub content: String,
}

pub struct SearchConfig {
    pub pattern: String,
    pub path: PathBuf,
    pub respect_gitignore: bool,
    pub context_lines: usize,  // NEW: default 20
}
```

**Interfaces/Contracts** (`src/context.rs`):
```rust
pub fn extract_context(
    file_path: &Path,
    match_line: usize,
    context_lines: usize,
) -> anyhow::Result<(Vec<ContextLine>, Vec<ContextLine>)>;

// Helper for efficient multi-match context extraction
pub fn extract_all_contexts(
    file_path: &Path,
    match_lines: &[usize],
    context_lines: usize,
) -> anyhow::Result<HashMap<usize, (Vec<ContextLine>, Vec<ContextLine>)>>;
```

**Edge Cases:**
- Match at line 5, context_lines = 20 → Only return lines 1-4 before
- Match at line (total_lines - 5), context_lines = 20 → Only return remaining lines after
- Two matches 10 lines apart with context_lines = 20 → Don't duplicate overlapping context
- Empty file → Return empty contexts
- Single line file → Return empty contexts

#### C) Testing Plan (Unit Tests)

**Unit Tests to Add:**

1. **`test_context_extraction_middle_of_file`** — Match at line 50 of 100-line file, verify exactly 20 lines before and after
2. **`test_context_extraction_near_start`** — Match at line 5, verify only 4 lines before returned
3. **`test_context_extraction_near_end`** — Match at line (total-5), verify only remaining lines after returned
4. **`test_context_overlapping_matches`** — Two matches 10 lines apart, verify contexts don't duplicate shared lines
5. **`test_context_empty_file`** — Empty file, verify empty contexts returned without error
6. **`test_context_single_line_file`** — Single line file with match, verify empty contexts

**Assertions:**
- `assert_eq!(context_before.len(), expected_len)`
- `assert_eq!(context_before[0].line_number, expected_start_line)`
- `assert_eq!(context_after.last().unwrap().line_number, expected_end_line)`
- Verify no duplicate line numbers across overlapping contexts

---

### Phase 3 — CLI Interface and Output Formatting
**Status:** Planned

#### A) Feature Slice
Add CLI argument parsing using `clap` and format search results for terminal display. User can now run `bulk <pattern> <path>` to see matches with context. Add basic flags: `--no-ignore`, `--context <N>`.

This phase makes bulk a usable CLI tool. Focus on clean, readable output format.

#### B) Detailed Design

**Dependencies** (add to `Cargo.toml`):
```toml
[dependencies]
clap = { version = "4.5", features = ["derive"] }
grep-regex = "0.1"
grep-searcher = "0.1"
ignore = "0.4"
```

**CLI Interface** (`src/main.rs`):
```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "bulk")]
#[command(about = "Recursive grep with context", long_about = None)]
struct Cli {
    /// Regex pattern to search for
    pattern: String,

    /// Directory or file to search (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Lines of context before and after each match
    #[arg(short = 'C', long, default_value = "20")]
    context: usize,

    /// Don't respect .gitignore files
    #[arg(long)]
    no_ignore: bool,
}
```

**Output Format:**
```
path/to/file.rs:42
   22 │ context line before
   23 │ more context
  ...
   41 │ line before match
   42 │ THIS LINE MATCHES THE PATTERN
   43 │ line after match
  ...
   61 │ more context
   62 │ context line after

path/to/other.rs:15
   1 │ match near start of file
  ...
```

Format specs:
- File path and match line number on first line
- Line numbers right-aligned with `│` separator
- Ellipsis (`...`) when context is truncated at file boundaries
- Blank line between matches
- Errors printed to stderr

**Error Handling:**
- Invalid pattern → Print error to stderr, exit code 1
- Invalid path → Print error to stderr, exit code 1
- No matches found → Exit code 0, no output
- File read errors → Print to stderr, continue searching

#### C) Testing Plan (Integration Tests)

**Integration Tests to Add** (`tests/integration_test.rs`):

1. **`test_bulk_search_with_context`** (SUCCESS CRITERIA TEST) — Creates test directory with multiple files, runs bulk CLI, verifies output contains:
   - Correct file paths
   - Correct line numbers
   - Exactly 20 lines context (or file boundaries)
   - Gitignored file excluded

2. **`test_bulk_no_ignore_flag`** — Creates test dir with .gitignore, runs with `--no-ignore`, verifies ignored files are searched

3. **`test_bulk_custom_context`** — Runs with `--context 5`, verifies only 5 lines context shown

4. **`test_bulk_invalid_pattern`** — Runs with invalid regex, verifies exit code 1 and error message

5. **`test_bulk_no_matches`** — Searches for pattern that doesn't exist, verifies exit code 0 and no output

**Test Setup:**
```rust
// Helper to run bulk CLI and capture output
fn run_bulk(args: &[&str]) -> (i32, String, String) {
    // Returns (exit_code, stdout, stderr)
}

// Helper to create test directory with known content
fn create_test_dir() -> TempDir {
    // Creates structure:
    // test_dir/
    //   file1.txt (50 lines, pattern on line 25)
    //   subdir/
    //     file2.txt (100 lines, pattern on lines 10 and 90)
    //   .gitignore (ignores ignored.txt)
    //   ignored.txt (pattern on line 5)
}
```

**Assertions:**
- `assert_eq!(exit_code, expected_code)`
- `assert!(stdout.contains("file1.txt:25"))`
- `assert_eq!(context_lines.len(), expected_context_count)`
- `assert!(!stdout.contains("ignored.txt"))` when using default ignore behavior

---

## Questions and Areas for Review

### Decisions Made (from user feedback):

1. **Output/Error Format** ✓ — Use tracing/env_logger for errors and output. Log warnings for unreadable files, continue searching (don't fail-fast).

2. **Binary File Detection** ✓ — Follow Helix's approach using grep-searcher's BinaryDetection.

3. **Context Overlap Handling** ✓ — Show overlapping context twice when matches are close (simpler implementation).

4. **Parallel Search** ✓ — Not including in Phase 1. grep-searcher doesn't have built-in parallelism; ripgrep achieves it at the orchestration layer. Defer until performance is an issue.

5. **Tool Name** ✓ — Called "bulked" not "bulk".

6. **Virtual Filesystem** ✓ — Use vfs crate with MemoryFS for all testing. No tests should touch real filesystem.

### Open Questions for Review:

1. **FileSystem Trait Granularity** (Discovery, Section "Architecture"): The FileSystem trait currently has methods like `read_to_string`, `read_line_at`, `read_line_range`. Is this the right level of abstraction, or should it be simpler (just `read_to_string`) and let higher levels handle line parsing?

2. **Test Organization** (Phase 1, Section C): Tests are organized into `tests/unit/`, `tests/integration/`, and `tests/fixtures/`. Should we also have `tests/contract/` as a separate directory, or keep contract tests in `tests/unit/`?

3. **Walker Simplification** (Phase 1, Section B): The Walker trait currently returns `path list`. For large directories, should it return an iterator/sequence instead to avoid loading all paths into memory?

### Review Checklist:

- [ ] **Architecture Section** — Does the OCaml-style `.mli` pseudocode clearly communicate the interface boundaries and dependency flow?
- [ ] **Phase 1 Testing** — Do the three levels of tests (solitary, contract, sociable) provide adequate coverage while remaining maintainable?
- [ ] **Dependency Injection** — Is the functor-based approach (`Searcher.Make(FS)(M)(W)`) clear enough, or should we use a different pattern for Rust translation?
- [ ] **Test Doubles Strategy** — Is the distinction between Fakes (MemoryFS), Stubs (StubMatcher), and Mocks clear and appropriate?

---

## Intent Confirmation

**Architectural Decisions:**

1. ✓ **Hexagonal Architecture** — Functional core depends on abstract ports (traits), imperative shell provides concrete adapters
2. ✓ **Virtual Filesystem** — All I/O goes through FileSystem trait, backed by vfs crate's MemoryFS for testing
3. ✓ **Test Strategy** — Three levels: Solitary (all deps stubbed), Contract (verify trait satisfaction), Sociable (multiple real modules)
4. ✓ **OCaml-style Interfaces** — Using `.mli` pseudocode to clearly show interface boundaries without implementation details
5. ✓ **Dependency Injection via Generic Traits** — Pseudocode shows `Searcher.Make(FS)(M)(W)` functor pattern, which maps to Rust as generic structs with trait bounds: `struct Searcher<FS: FileSystem, M: Matcher, W: Walker>`

**Three-Phase Breakdown:**

- **Phase 1** — Core search engine with abstract interfaces and adapters (Matcher, FileSystem, Walker, Searcher)
- **Phase 2** — Context extraction as a pure function over FileSystem trait
- **Phase 3** — CLI interface and output formatting with end-to-end MemoryFS test (SUCCESS CRITERIA)

**Implementation Decisions (from user feedback):**

1. ✓ **FileSystem trait granularity** — Use whatever methods are necessary for the work; be pragmatic during implementation
2. ✓ **Test organization** — Use inline test modules (`#[cfg(test)] mod tests`) within each source file, not separate test directories
3. ✓ **Walker memory** — Use iterator for memory efficiency with large directories
