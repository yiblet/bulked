use aho_corasick::AhoCorasick;

pub fn escape_content(content: &str) -> Display<'_, 2> {
    let patterns = ["\\", "@"];
    let replacements = ["\\\\", "\\@"];
    Display::<2> {
        source: content,
        patterns,
        replacements,
    }
}

pub struct Display<'a, const N: usize> {
    source: &'a str,
    patterns: [&'static str; N],
    replacements: [&'static str; N],
}

impl<'a, const N: usize> std::fmt::Display for Display<'a, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ac = AhoCorasick::new(self.patterns.as_slice())
            .expect("Failed to create Aho-Corasick matcher");
        let mut prev = 0;
        for mat in ac.find_iter(self.source) {
            f.write_str(&self.source[prev..mat.start()])?;
            let pat = &self.source[mat.start()..mat.end()];
            let rep = self.replacements[self.patterns.iter().position(|p| *p == pat).unwrap_or(0)];
            f.write_str(rep)?;
            prev = mat.end();
        }

        if prev < self.source.len() {
            f.write_str(&self.source[prev..])?;
        }
        Ok(())
    }
}

pub fn unescape_content(content: &str) -> Display<'_, 2> {
    let patterns = ["\\@", "\\\\"];
    let replacements = ["@", "\\"];
    Display::<2> {
        source: content,
        patterns,
        replacements,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_content_empty_string() {
        assert_eq!(escape_content("").to_string(), "");
    }

    #[test]
    fn test_escape_content_no_special_chars() {
        assert_eq!(escape_content("hello world").to_string(), "hello world");
    }

    #[test]
    fn test_escape_content_backslash() {
        assert_eq!(
            escape_content("path\\to\\file").to_string(),
            "path\\\\to\\\\file"
        );
    }

    #[test]
    fn test_escape_content_at_symbol() {
        assert_eq!(
            escape_content("user@domain.com").to_string(),
            "user\\@domain.com"
        );
    }

    #[test]
    fn test_escape_content_both_special_chars() {
        assert_eq!(
            escape_content("C:\\path@file").to_string(),
            "C:\\\\path\\@file"
        );
    }

    #[test]
    fn test_escape_content_multiple_backslashes() {
        assert_eq!(escape_content("\\\\\\").to_string(), "\\\\\\\\\\\\");
    }

    #[test]
    fn test_escape_content_multiple_at_symbols() {
        assert_eq!(escape_content("@@test@@").to_string(), "\\@\\@test\\@\\@");
    }

    #[test]
    fn test_unescape_content_empty_string() {
        assert_eq!(unescape_content("").to_string(), "");
    }

    #[test]
    fn test_unescape_content_no_special_chars() {
        assert_eq!(unescape_content("hello world").to_string(), "hello world");
    }

    #[test]
    fn test_unescape_content_escaped_backslash() {
        assert_eq!(
            unescape_content("path\\\\to\\\\file").to_string(),
            "path\\to\\file"
        );
    }

    #[test]
    fn test_unescape_content_escaped_at_symbol() {
        assert_eq!(
            unescape_content("user\\@domain.com").to_string(),
            "user@domain.com"
        );
    }

    #[test]
    fn test_unescape_content_both_escaped_chars() {
        assert_eq!(
            unescape_content("C:\\\\path\\@file").to_string(),
            "C:\\path@file"
        );
    }

    #[test]
    fn test_roundtrip_escape_unescape() {
        let original = "C:\\path\\to@file\\with@symbols";
        let escaped = escape_content(original).to_string();
        let unescaped = unescape_content(&escaped).to_string();
        assert_eq!(original, unescaped);
    }

    #[test]
    fn test_roundtrip_complex_string() {
        let original = "\\\\@@@\\\\test\\@value";
        let escaped = escape_content(original).to_string();
        let unescaped = unescape_content(&escaped).to_string();
        assert_eq!(original, unescaped);
    }

    #[test]
    fn test_escape_already_escaped() {
        // Escaping an already escaped string should double-escape
        assert_eq!(escape_content("\\\\").to_string(), "\\\\\\\\");
        assert_eq!(escape_content("\\@").to_string(), "\\\\\\@");
    }

    #[test]
    fn test_unescape_non_escaped() {
        // Unescaping a non-escaped string should leave it unchanged
        assert_eq!(unescape_content("regular text").to_string(), "regular text");
    }
}
