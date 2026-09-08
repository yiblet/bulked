//! Line diffs for the `--dry-run` previews, on top of the `similar` crate.

use std::io::Write;

use similar::{ChangeTag, TextDiff};

/// Write a line-by-line diff of `old` against `new` in unified-diff body style:
/// unchanged lines prefixed with a space, removed lines with `-`, added lines with
/// `+`. A side whose last line has no trailing newline is flagged the way `diff`
/// does, with `\ No newline at end of file`.
///
/// With `color`, removed lines are red and added lines green (ANSI), reset before
/// the line ending so a pager or terminal never bleeds color into the next line.
/// Context lines and the no-newline marker are never colored.
///
/// This is only the body; callers write their own `---`/`+++`/`@@` headers, since
/// what those describe (a file, a chunk) depends on the caller.
pub fn write_line_diff(
    out: &mut dyn Write,
    old: &str,
    new: &str,
    color: bool,
) -> std::io::Result<()> {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const RESET: &str = "\x1b[0m";

    let diff = TextDiff::from_lines(old, new);
    for change in diff.iter_all_changes() {
        let (marker, paint) = match change.tag() {
            ChangeTag::Equal => (' ', None),
            ChangeTag::Delete => ('-', Some(RED)),
            ChangeTag::Insert => ('+', Some(GREEN)),
        };
        let value = change.value();
        let text = value.strip_suffix('\n').unwrap_or(value);
        match paint.filter(|_| color) {
            Some(paint) => write!(out, "{paint}{marker}{text}{RESET}")?,
            None => write!(out, "{marker}{text}")?,
        }
        if change.missing_newline() {
            writeln!(out, "\n\\ No newline at end of file")?;
        } else {
            writeln!(out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(old: &str, new: &str) -> String {
        let mut out = Vec::new();
        write_line_diff(&mut out, old, new, false).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn test_color_paints_only_changed_lines_and_resets_before_the_newline() {
        let mut out = Vec::new();
        write_line_diff(&mut out, "a\nb\nc", "a\nB\nc", true).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            " a\n\x1b[31m-b\x1b[0m\n\x1b[32m+B\x1b[0m\n c\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn test_unchanged_lines_are_context_not_churn() {
        assert_eq!(diff("a\nb\nc\n", "a\nB\nc\n"), " a\n-b\n+B\n c\n");
    }

    #[test]
    fn test_pure_replacement_insertion_and_deletion() {
        assert_eq!(diff("b\n", "B\n"), "-b\n+B\n");
        assert_eq!(diff("d\n", "D1\nD2\n"), "-d\n+D1\n+D2\n");
        assert_eq!(diff("c\n", ""), "-c\n");
        assert_eq!(diff("", "x\n"), "+x\n");
    }

    #[test]
    fn test_missing_final_newline_is_flagged_per_side() {
        assert_eq!(
            diff("b", "B"),
            "-b\n\\ No newline at end of file\n+B\n\\ No newline at end of file\n"
        );
        // Same text, only the newline differs: still a change.
        assert_eq!(diff("b\n", "b"), "-b\n+b\n\\ No newline at end of file\n");
    }
}
