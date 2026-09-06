//! Positional escaping for chunk content.
//!
//! Inside a chunk only the *first character of a line* can be ambiguous: a line
//! that starts with `@` could be the `@@@` terminator or the next chunk's header.
//! So escaping is confined to the start of a line and nothing mid-line is ever
//! touched:
//!
//! - **serialize**: a content line starting with `@`, `\@` or `\\` gets one `\`
//!   prepended ([`needs_escape`] decides; `format::types` writes it);
//! - **parse**: a content line starting with `\@` or `\\` drops that first `\`
//!   ([`unescape_line`]).
//!
//! Every other line — `\begin{document}`, `"\\d+"`, `user@example.com` — is
//! written and read back verbatim, and the two rules are exact inverses of each
//! other (the `\\` case is what keeps a line that already starts with `\@` or
//! `\\` distinguishable after escaping, the same reason mboxrd quotes `>From `
//! as well as `From `).

/// Does this content line need a leading `\` to survive serialization?
///
/// `line` is one line of content, with or without its trailing newline.
#[must_use]
pub fn needs_escape(line: &str) -> bool {
    line.starts_with('@') || line.starts_with("\\@") || line.starts_with("\\\\")
}

/// Undo the serializer's escaping for one line: strip the leading `\` that protects a
/// line starting with `@` or `\`. Any other line is returned unchanged.
#[must_use]
pub fn unescape_line(line: &str) -> &str {
    if line.starts_with("\\@") || line.starts_with("\\\\") {
        &line[1..]
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The serializer's rule, spelled out for the tests: one `\` before any
    /// line that [`needs_escape`].
    fn escape_content(content: &str) -> String {
        content
            .split_inclusive('\n')
            .map(|line| {
                if needs_escape(line) {
                    format!("\\{line}")
                } else {
                    line.to_string()
                }
            })
            .collect()
    }

    fn roundtrip(content: &str) -> String {
        escape_content(content)
            .split_inclusive('\n')
            .map(unescape_line)
            .collect()
    }

    #[test]
    fn test_escape_content_empty_and_plain() {
        assert_eq!(escape_content(""), "");
        assert_eq!(escape_content("hello world\n"), "hello world\n");
    }

    #[test]
    fn test_escape_only_touches_line_starts() {
        // Mid-line `@` and `\` are content, not syntax.
        assert_eq!(
            escape_content("user@domain.com\\path\n"),
            "user@domain.com\\path\n"
        );
        assert_eq!(
            escape_content("    let re = \"\\\\d+\";\n"),
            "    let re = \"\\\\d+\";\n"
        );
        // A line-initial backslash that is not `\@` or `\\` is also verbatim.
        assert_eq!(escape_content("\\begin{document}\n"), "\\begin{document}\n");
    }

    #[test]
    fn test_escape_line_initial_at_and_backslash_forms() {
        assert_eq!(escape_content("@dataclass\n"), "\\@dataclass\n");
        assert_eq!(escape_content("@@@\n"), "\\@@@\n");
        assert_eq!(escape_content("\\@x\n"), "\\\\@x\n");
        assert_eq!(
            escape_content("\\\\server\\share\n"),
            "\\\\\\server\\share\n"
        );
        // Last line without a trailing newline is escaped the same way.
        assert_eq!(escape_content("a\n@b"), "a\n\\@b");
    }

    #[test]
    fn test_unescape_line() {
        assert_eq!(unescape_line("\\@dataclass\n"), "@dataclass\n");
        assert_eq!(unescape_line("\\\\@x\n"), "\\@x\n");
        assert_eq!(unescape_line("\\\\\\server\n"), "\\\\server\n");
        assert_eq!(unescape_line("\\begin\n"), "\\begin\n");
        assert_eq!(unescape_line("plain\n"), "plain\n");
        assert_eq!(unescape_line(""), "");
    }

    #[test]
    fn test_escape_unescape_roundtrip() {
        for content in [
            "",
            "plain\n",
            "@dataclass\nclass B:\n    e = \"a@b.com\"\n",
            "\\@x\n\\\\y\n\\begin\n\\\n",
            "    let re = \"\\\\d+\";\n",
            "@@@\n@@@-\n@path:1:2\n",
            "no trailing newline @ here",
            "@no trailing newline",
        ] {
            assert_eq!(roundtrip(content), content, "roundtrip of {content:?}");
        }
    }
}
