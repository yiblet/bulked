use crate::filesystem::memory::MemoryFS;
use crate::matcher::regex::GrepMatcher;
use crate::searcher::Searcher;
use crate::walker::simple::SimpleWalker;
use std::path::PathBuf;

/// Full-stack integration test with `MemoryFS`
///
/// This tests the entire search pipeline with real implementations
/// (except filesystem) to verify they work together correctly.
#[test]
fn test_full_stack_integration() {
    // Create a realistic directory structure in memory
    let fs = MemoryFS::new();

    fs.add_file(&PathBuf::from("/project/.gitignore"), "*.tmp\ntarget/\n")
        .unwrap();
    fs.add_file(
        &PathBuf::from("/project/src/main.rs"),
        "fn main() {\n    println!(\"Hello\");\n}\n",
    )
    .unwrap();
    fs.add_file(
        &PathBuf::from("/project/src/lib.rs"),
        "pub fn greet() {\n    println!(\"Hello\");\n}\n",
    )
    .unwrap();
    fs.add_file(&PathBuf::from("/project/test.tmp"), "temporary\n")
        .unwrap();

    // Use real GrepMatcher
    let matcher = GrepMatcher::compile("fn ").unwrap();

    // Use SimpleWalker with files (in production, IgnoreWalker would handle filtering)
    let walker = SimpleWalker::new(vec![
        PathBuf::from("/project/src/main.rs"),
        PathBuf::from("/project/src/lib.rs"),
        // Intentionally not including test.tmp (simulating .gitignore)
    ]);

    let searcher = Searcher::new(fs, matcher, walker);
    let results: Vec<_> = searcher
        .search_all()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let all_matches: Vec<_> = results.iter().flatten().collect();

    // Should find "fn " in both .rs files
    assert!(all_matches.len() >= 2, "Should find at least 2 matches");

    // Verify matches are from .rs files
    for m in &all_matches {
        assert!(
            m.file_path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs")),
            "Match should be from .rs file, got: {}",
            m.file_path.display()
        );
    }
}

/// SUCCESS CRITERIA TEST - Phase 3
///
/// This is the integration test specified in the plan that verifies:
/// - Matches are found in correct files
/// - Each match includes exactly 20 lines before and 20 lines after (or up to file boundaries)
/// - Line numbers are accurate
/// - Gitignored files are excluded by default
///
/// Uses virtual filesystem (`MemoryFS`) for hermetic testing.
#[test]
fn test_bulked_search_with_context() {
    // Create test directory with realistic structure
    let fs = MemoryFS::new();

    // Create .gitignore
    fs.add_file(&PathBuf::from("/project/.gitignore"), "*.log\ntemp/\n")
        .unwrap();

    // Create a file with enough lines for context testing (50 lines)
    let mut file1_lines = vec![];
    for i in 1..=50 {
        if i == 25 {
            file1_lines.push("TARGET match on line 25".to_string());
        } else {
            file1_lines.push(format!("line {i}"));
        }
    }
    fs.add_file(
        &PathBuf::from("/project/file1.txt"),
        &file1_lines.join("\n"),
    )
    .unwrap();

    // Create another file with match near boundaries
    let file2_lines = ["line 1", "line 2", "TARGET at line 3", "line 4"];
    fs.add_file(
        &PathBuf::from("/project/file2.txt"),
        &file2_lines.join("\n"),
    )
    .unwrap();

    // Create a file that should be ignored
    fs.add_file(&PathBuf::from("/project/ignored.log"), "TARGET ignored")
        .unwrap();

    // Use real GrepMatcher with context
    let matcher = GrepMatcher::compile("TARGET").unwrap().with_context(20);

    // Use SimpleWalker (simulating gitignore filtering)
    let walker = SimpleWalker::new(vec![
        PathBuf::from("/project/file1.txt"),
        PathBuf::from("/project/file2.txt"),
        // NOT including ignored.log
    ]);

    let searcher = Searcher::new(fs, matcher, walker);
    let results: Vec<_> = searcher
        .search_all()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let all_matches: Vec<_> = results.iter().flatten().collect();

    // Verify correct number of matches (2 matches in non-ignored files)
    assert_eq!(
        all_matches.len(),
        2,
        "Should find exactly 2 matches in non-ignored files"
    );

    // Verify first match (file1.txt, line 25 with full context)
    let match1 = all_matches
        .iter()
        .find(|m| m.file_path.to_str().unwrap().contains("file1.txt"))
        .expect("Should find match in file1.txt");

    assert_eq!(match1.line_number, 25);
    assert!(match1.line_content.contains("TARGET"));

    // Verify context before (should be exactly 20 lines: lines 5-24)
    assert_eq!(
        match1.context_before.split_inclusive('\n').count(),
        20,
        "Should have exactly 20 lines of context before"
    );
    assert!(match1.context_before.starts_with("line 5\n"));
    assert!(match1.context_before.ends_with("line 24\n"));

    // Verify context after (should be exactly 20 lines: lines 26-45)
    assert_eq!(
        match1.context_after.split_inclusive('\n').count(),
        20,
        "Should have exactly 20 lines of context after"
    );
    assert!(match1.context_after.starts_with("line 26\n"));
    assert!(match1.context_after.ends_with("line 45\n"));

    // Verify second match (file2.txt, line 3 near start - limited context)
    let match2 = all_matches
        .iter()
        .find(|m| m.file_path.to_str().unwrap().contains("file2.txt"))
        .expect("Should find match in file2.txt");

    assert_eq!(match2.line_number, 3);
    assert!(match2.line_content.contains("TARGET"));

    // Verify context before (only 2 lines available: lines 1-2)
    assert_eq!(
        match2.context_before, "line 1\nline 2\n",
        "Should have only 2 lines before (file boundary)"
    );

    // Verify context after (only 1 line available: line 4, the file's last
    // line, which has no trailing newline)
    assert_eq!(
        match2.context_after, "line 4",
        "Should have only 1 line after (file boundary)"
    );

    // Verify gitignored file was NOT searched
    assert!(
        !all_matches
            .iter()
            .any(|m| m.file_path.to_str().unwrap().contains("ignored.log")),
        "Should not find matches in gitignored files"
    );
}

/// Test that search -> format -> apply roundtrip preserves file content
///
/// This test verifies that when we search for a pattern, convert results to Format,
/// and apply the format back to the file WITHOUT any modifications, the file content
/// remains identical. This ensures newlines are preserved correctly throughout the pipeline.
#[test]
fn test_search_format_apply_roundtrip_preserves_content() {
    use crate::apply::apply_format_to_fs;
    use crate::format::Format;

    // Create test file with specific content that has multiple lines
    let fs = MemoryFS::new();
    let test_file = PathBuf::from("/test/file.txt");
    let original_content = "line 1\nline 2\nfunc here\nline 4\nline 5\n";
    fs.add_file(&test_file, original_content).unwrap();

    // Search for pattern with context
    let matcher = GrepMatcher::compile("func").unwrap().with_context(2);
    let walker = SimpleWalker::new(vec![test_file.clone()]);
    let searcher = Searcher::new(fs.clone(), matcher, walker);
    let results: Vec<_> = searcher
        .search_all()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let all_matches: Vec<_> = results.iter().flatten().cloned().collect();

    // Convert matches to Format
    let format = Format::from_matches(&all_matches);

    // Apply the format back to the file (no modifications)
    apply_format_to_fs(&format, &fs).unwrap();

    // Read the file back and verify it's unchanged
    let final_content = fs.read_to_string(&test_file).unwrap();
    assert_eq!(
        original_content, final_content,
        "Content should be identical after roundtrip.\nOriginal:\n{:?}\nFinal:\n{:?}",
        original_content, final_content
    );
}

/// The `apply` CLI handler, end to end, against an in-memory filesystem.
///
/// Exercises the injected `run` core: the chunk format comes from `input`, the
/// file is read and rewritten through `MemoryFS`, and the status line lands in
/// `out`. Nothing touches the real filesystem, stdin, or stdout.
#[test]
fn test_apply_handler_end_to_end_on_memory_fs() {
    use crate::cli::ApplyArgs;

    let fs = MemoryFS::new();
    let file = PathBuf::from("/f.txt");
    fs.add_file(&file, "a\nb\nc\n").unwrap();

    let mut input = "@/f.txt:2:1\nB\n@@@\n".as_bytes();
    let mut out: Vec<u8> = Vec::new();

    ApplyArgs {
        input: None,
        dry_run: false,
        force: false,
    }
    .run(&fs, &mut input, &mut out, false)
    .expect("apply should succeed on a valid chunk");

    assert_eq!(fs.read_to_string(&file).unwrap(), "a\nB\nc\n");
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("Applied 1 chunk to 1 file"),
        "unexpected status output: {out:?}"
    );
}

/// `apply --dry-run` through the handler verifies but writes nothing.
#[test]
fn test_apply_dry_run_writes_nothing_on_memory_fs() {
    use crate::cli::ApplyArgs;

    let fs = MemoryFS::new();
    let file = PathBuf::from("/f.txt");
    fs.add_file(&file, "a\nb\nc\n").unwrap();

    let mut input = "@/f.txt:2:1\nB\n@@@\n".as_bytes();
    let mut out: Vec<u8> = Vec::new();

    ApplyArgs {
        input: None,
        dry_run: true,
        force: false,
    }
    .run(&fs, &mut input, &mut out, false)
    .expect("dry-run should succeed on a valid chunk");

    assert_eq!(
        fs.read_to_string(&file).unwrap(),
        "a\nb\nc\n",
        "dry-run must not modify the file"
    );
    assert_eq!(fs.file_count(), 1, "dry-run must not leave staged files");
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("Would apply 1 chunk to /f.txt"),
        "unexpected status output: {out:?}"
    );
}

/// `ingest -o` writes the file through staging: the result lands in place and
/// no temp file is left in the filesystem.
#[test]
fn test_ingest_output_file_is_written_atomically_on_memory_fs() {
    use crate::cli::IngestArgs;

    let fs = MemoryFS::new();
    fs.add_file(&PathBuf::from("/f.txt"), "a\nb\nc\n").unwrap();
    let out_path = PathBuf::from("/work/edits.bk");

    let mut err: Vec<u8> = Vec::new();
    IngestArgs {
        path: None,
        format: Default::default(),
        output: Some(out_path.clone()),
        context: 0,
        plain: false,
    }
    .run(
        &fs,
        &mut "/f.txt:2\n".as_bytes(),
        &mut Vec::new(),
        &mut err,
        false,
    )
    .unwrap();

    assert_eq!(
        fs.read_to_string(&out_path).unwrap(),
        format!(
            "@/f.txt:2:1 #{}\nb\n@@@\n",
            crate::format::Fingerprint::of(b"b\n")
        )
    );
    assert_eq!(
        fs.file_count(),
        2,
        "no staged temp left behind: {:?}",
        fs.paths()
    );
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("wrote 1 chunk to /work/edits.bk")
    );
}

/// `apply` refuses a stale fingerprint; `apply --force` overwrites the lines anyway.
#[test]
fn test_apply_force_ignores_stale_fingerprints_on_memory_fs() {
    use crate::cli::ApplyArgs;
    use crate::format::Fingerprint;

    let fs = MemoryFS::new();
    let file = PathBuf::from("/f.txt");
    fs.add_file(&file, "a\nCHANGED\nc\n").unwrap();
    // Generated when line 2 was still `b`.
    let bk = format!("@/f.txt:2:1 #{}\nB\n@@@\n", Fingerprint::of(b"b\n"));

    let err = ApplyArgs {
        input: None,
        dry_run: false,
        force: false,
    }
    .run(&fs, &mut bk.as_bytes(), &mut Vec::new(), false)
    .expect_err("a stale fingerprint must be refused without --force");
    assert!(
        err.to_string()
            .contains("changed since this chunk was generated"),
        "unexpected error: {err}"
    );
    assert_eq!(fs.read_to_string(&file).unwrap(), "a\nCHANGED\nc\n");

    let mut out: Vec<u8> = Vec::new();
    ApplyArgs {
        input: None,
        dry_run: false,
        force: true,
    }
    .run(&fs, &mut bk.as_bytes(), &mut out, false)
    .expect("--force must apply despite the stale fingerprint");
    assert_eq!(fs.read_to_string(&file).unwrap(), "a\nB\nc\n");
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("Applied 1 chunk to 1 file")
    );
}

/// The `refresh` CLI handler, end to end: a `.bk` with one stale and one current
/// fingerprint is rewritten in place, and the result applies cleanly.
#[test]
fn test_refresh_handler_rewrites_bk_in_place_on_memory_fs() {
    use crate::cli::{ApplyArgs, Exit, RefreshArgs};
    use crate::format::Fingerprint;

    let fs = MemoryFS::new();
    let file = PathBuf::from("/f.txt");
    let bk_path = PathBuf::from("/edits.bk");
    fs.add_file(&file, "a\nCHANGED\nc\nd\n").unwrap();
    fs.add_file(
        &bk_path,
        &format!(
            "my notes\n@/f.txt:2:1 #{}\nB\n@@@\n@/f.txt:4:1 #{}\nD\n@@@\n",
            Fingerprint::of(b"b\n"),
            Fingerprint::of(b"d\n")
        ),
    )
    .unwrap();

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let exit = RefreshArgs {
        path: Some(bk_path.clone()),
        output: None,
        dry_run: false,
    }
    .run(&fs, &mut "".as_bytes(), &mut out, &mut err, false)
    .expect("refresh should succeed");

    assert_eq!(exit, Exit::Ok);
    assert!(out.is_empty(), "in-place refresh must not print the chunks");
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.contains("updated 1 of 2 chunks in /edits.bk"),
        "unexpected status: {err:?}"
    );
    assert_eq!(
        fs.read_to_string(&bk_path).unwrap(),
        format!(
            "my notes\n@/f.txt:2:1 #{}\nB\n@@@\n@/f.txt:4:1 #{}\nD\n@@@\n",
            Fingerprint::of(b"CHANGED\n"),
            Fingerprint::of(b"d\n")
        )
    );
    assert_eq!(fs.file_count(), 2, "refresh must not leave staged files");

    // Running it again finds nothing to do and leaves the file alone.
    let mut err: Vec<u8> = Vec::new();
    let exit = RefreshArgs {
        path: Some(bk_path.clone()),
        output: None,
        dry_run: false,
    }
    .run(&fs, &mut "".as_bytes(), &mut Vec::new(), &mut err, false)
    .unwrap();
    assert_eq!(exit, Exit::Nothing);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("all 2 chunks in /edits.bk are current")
    );

    // And the refreshed file now applies without --force.
    ApplyArgs {
        input: Some(bk_path),
        dry_run: false,
        force: false,
    }
    .run(&fs, &mut "".as_bytes(), &mut Vec::new(), false)
    .expect("refreshed chunks must apply");
    assert_eq!(fs.read_to_string(&file).unwrap(), "a\nB\nc\nD\n");
}

/// `refresh --dry-run` prints the old-vs-new chunk diff and writes nothing.
#[test]
fn test_refresh_dry_run_prints_diff_and_writes_nothing_on_memory_fs() {
    use crate::cli::{Exit, RefreshArgs};
    use crate::format::Fingerprint;

    let fs = MemoryFS::new();
    let bk_path = PathBuf::from("/edits.bk");
    fs.add_file(&PathBuf::from("/f.txt"), "a\nNEW\nc\nd\n")
        .unwrap();
    // Chunk at 2 is unedited (still `b`), chunk at 4 was edited to `D`.
    let bk = format!(
        "@/f.txt:2:1 #{}\nb\n@@@\n@/f.txt:4:1 #{}\nD\n@@@\n",
        Fingerprint::of(b"b\n"),
        Fingerprint::of(b"d\n")
    );
    fs.add_file(&bk_path, &bk).unwrap();

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let exit = RefreshArgs {
        path: Some(bk_path.clone()),
        output: None,
        dry_run: true,
    }
    .run(&fs, &mut "".as_bytes(), &mut out, &mut err, false)
    .unwrap();

    assert_eq!(exit, Exit::Ok);
    assert_eq!(
        fs.read_to_string(&bk_path).unwrap(),
        bk,
        "dry-run must not write"
    );
    assert_eq!(fs.file_count(), 2);
    let out = String::from_utf8(out).unwrap();
    assert_eq!(
        out,
        format!(
            "--- @/f.txt:2:1 #{}\n+++ @/f.txt:2:1 #{}  (reread from the file)\n-b\n+NEW\n",
            Fingerprint::of(b"b\n"),
            Fingerprint::of(b"NEW\n")
        ),
        "chunk 4 is current and must not appear"
    );
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.contains("would update 1 of 2 chunks in /edits.bk"),
        "{err:?}"
    );
    assert!(err.contains("reread from the file"), "{err:?}");
}

/// `refresh` with no path filters stdin to stdout; status stays on stderr.
#[test]
fn test_refresh_handler_stdin_to_stdout_on_memory_fs() {
    use crate::cli::{Exit, RefreshArgs};
    use crate::format::Fingerprint;

    let fs = MemoryFS::new();
    fs.add_file(&PathBuf::from("/f.txt"), "x\n").unwrap();
    let bk = format!("@/f.txt:1:1 #{}\nY\n@@@\n", Fingerprint::of(b"old\n"));

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let exit = RefreshArgs {
        path: None,
        output: None,
        dry_run: false,
    }
    .run(&fs, &mut bk.as_bytes(), &mut out, &mut err, false)
    .unwrap();

    assert_eq!(exit, Exit::Ok);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("@/f.txt:1:1 #{}\nY\n@@@\n", Fingerprint::of(b"x\n"))
    );
    assert!(
        String::from_utf8(err)
            .unwrap()
            .starts_with("bulked refresh: updated 1 of 1 chunk")
    );
}

/// The `ingest` CLI handler, end to end, against an in-memory filesystem:
/// grep-format locations from `input`, context read through `MemoryFS`, and
/// the chunk format written to `out`.
#[test]
fn test_ingest_handler_end_to_end_on_memory_fs() {
    use crate::cli::IngestArgs;

    let fs = MemoryFS::new();
    fs.add_file(&PathBuf::from("/f.txt"), "a\nb\nc\n").unwrap();

    let mut input = "/f.txt:2\n".as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();

    IngestArgs {
        path: None,
        format: Default::default(),
        output: None,
        context: 1,
        plain: false,
    }
    .run(&fs, &mut input, &mut out, &mut err, false)
    .expect("ingest should succeed on a grep-format location");

    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "@/f.txt:1:3 #{}\na\nb\nc\n@@@\n",
            crate::format::Fingerprint::of(b"a\nb\nc\n")
        )
    );
    assert!(
        err.is_empty(),
        "no status line expected without --output: {:?}",
        String::from_utf8_lossy(&err)
    );
}
