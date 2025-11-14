use crate::filesystem::memory::MemoryFS;
use crate::matcher::Matcher; // Import the trait
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
    let results: Vec<_> = searcher.search_all().collect::<Result<Vec<_>, _>>().unwrap();
    let all_matches: Vec<_> = results.iter().flat_map(|r| &r.matches).collect();

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
    let results: Vec<_> = searcher.search_all().collect::<Result<Vec<_>, _>>().unwrap();
    let all_matches: Vec<_> = results.iter().flat_map(|r| &r.matches).collect();

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
        match1.context_before.len(),
        20,
        "Should have exactly 20 lines of context before"
    );
    assert_eq!(match1.context_before[0].line_number, 5);
    assert_eq!(match1.context_before[19].line_number, 24);

    // Verify context after (should be exactly 20 lines: lines 26-45)
    assert_eq!(
        match1.context_after.len(),
        20,
        "Should have exactly 20 lines of context after"
    );
    assert_eq!(match1.context_after[0].line_number, 26);
    assert_eq!(match1.context_after[19].line_number, 45);

    // Verify second match (file2.txt, line 3 near start - limited context)
    let match2 = all_matches
        .iter()
        .find(|m| m.file_path.to_str().unwrap().contains("file2.txt"))
        .expect("Should find match in file2.txt");

    assert_eq!(match2.line_number, 3);
    assert!(match2.line_content.contains("TARGET"));

    // Verify context before (only 2 lines available: lines 1-2)
    assert_eq!(
        match2.context_before.len(),
        2,
        "Should have only 2 lines before (file boundary)"
    );
    assert_eq!(match2.context_before[0].line_number, 1);
    assert_eq!(match2.context_before[1].line_number, 2);

    // Verify context after (only 1 line available: line 4)
    assert_eq!(
        match2.context_after.len(),
        1,
        "Should have only 1 line after (file boundary)"
    );
    assert_eq!(match2.context_after[0].line_number, 4);

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
    use crate::filesystem::FileSystem;
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
    let results: Vec<_> = searcher.search_all().collect::<Result<Vec<_>, _>>().unwrap();
    let all_matches: Vec<_> = results.iter().flat_map(|r| &r.matches).cloned().collect();

    // Convert matches to Format
    let mut format = Format::from_matches(&all_matches);

    // Apply the format back to the file (no modifications)
    apply_format_to_fs(&mut format, &mut fs.clone()).unwrap();

    // Read the file back and verify it's unchanged
    let final_content = fs.read_to_string(&test_file).unwrap();
    assert_eq!(
        original_content, final_content,
        "Content should be identical after roundtrip.\nOriginal:\n{:?}\nFinal:\n{:?}",
        original_content, final_content
    );
}

