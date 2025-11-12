use aho_corasick::AhoCorasick;

pub fn escape_content(content: &str) -> String {
    let patterns = &["\\", "@"];
    let replacements = &["\\\\", "\\@"];
    let ac = AhoCorasick::new(patterns).expect("Failed to create Aho-Corasick matcher");
    ac.replace_all(content, replacements)
}

pub fn unescape_content(content: &str) -> String {
    let patterns = &["\\@", "\\\\"];
    let replacements = &["@", "\\"];
    let ac = AhoCorasick::new(patterns).expect("Failed to create Aho-Corasick matcher");
    ac.replace_all(content, replacements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_content_empty_string() {
        assert_eq!(escape_content(""), "");
    }

    #[test]
    fn test_escape_content_no_special_chars() {
        assert_eq!(escape_content("hello world"), "hello world");
    }

    #[test]
    fn test_escape_content_backslash() {
        assert_eq!(escape_content("path\\to\\file"), "path\\\\to\\\\file");
    }

    #[test]
    fn test_escape_content_at_symbol() {
        assert_eq!(escape_content("user@domain.com"), "user\\@domain.com");
    }

    #[test]
    fn test_escape_content_both_special_chars() {
        assert_eq!(escape_content("C:\\path@file"), "C:\\\\path\\@file");
    }

    #[test]
    fn test_escape_content_multiple_backslashes() {
        assert_eq!(escape_content("\\\\\\"), "\\\\\\\\\\\\");
    }

    #[test]
    fn test_escape_content_multiple_at_symbols() {
        assert_eq!(escape_content("@@test@@"), "\\@\\@test\\@\\@");
    }

    #[test]
    fn test_unescape_content_empty_string() {
        assert_eq!(unescape_content(""), "");
    }

    #[test]
    fn test_unescape_content_no_special_chars() {
        assert_eq!(unescape_content("hello world"), "hello world");
    }

    #[test]
    fn test_unescape_content_escaped_backslash() {
        assert_eq!(unescape_content("path\\\\to\\\\file"), "path\\to\\file");
    }

    #[test]
    fn test_unescape_content_escaped_at_symbol() {
        assert_eq!(unescape_content("user\\@domain.com"), "user@domain.com");
    }

    #[test]
    fn test_unescape_content_both_escaped_chars() {
        assert_eq!(unescape_content("C:\\\\path\\@file"), "C:\\path@file");
    }

    #[test]
    fn test_roundtrip_escape_unescape() {
        let original = "C:\\path\\to@file\\with@symbols";
        let escaped = escape_content(original);
        let unescaped = unescape_content(&escaped);
        assert_eq!(original, unescaped);
    }

    #[test]
    fn test_roundtrip_complex_string() {
        let original = "\\\\@@@\\\\test\\@value";
        let escaped = escape_content(original);
        let unescaped = unescape_content(&escaped);
        assert_eq!(original, unescaped);
    }

    #[test]
    fn test_escape_already_escaped() {
        // Escaping an already escaped string should double-escape
        assert_eq!(escape_content("\\\\"), "\\\\\\\\");
        assert_eq!(escape_content("\\@"), "\\\\\\@");
    }

    #[test]
    fn test_unescape_non_escaped() {
        // Unescaping a non-escaped string should leave it unchanged
        assert_eq!(unescape_content("regular text"), "regular text");
    }
}
