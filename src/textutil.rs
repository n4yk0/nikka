//! Normalises scraped text: HTML stripping, accent folding, truncation.

use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").expect("valid regex"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\p{Zs}]+").expect("valid regex"));

const MARKDOWN_ESCAPES: [(&str, &str); 4] = [(r"\[", "["), (r"\]", "]"), (r"\_", "_"), (r"\*", "*")];

pub fn clean_text(s: &str) -> String {
    let unescaped = htmlize::unescape(s);
    let untagged = HTML_TAG.replace_all(&unescaped, " ");
    let mut text = htmlize::unescape(untagged.as_ref()).into_owned();
    for (escaped, plain) in MARKDOWN_ESCAPES {
        text = text.replace(escaped, plain);
    }
    WHITESPACE.replace_all(&text, " ").trim().to_string()
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

/// Lowercases and strips diacritics so "Générative" matches "generative".
pub fn fold(s: &str) -> String {
    s.to_lowercase().nfd().filter(|c| !is_combining_mark(*c)).nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_strips_tags_entities_and_markdown_escapes() {
        let input = r#"<div><a href="x"><img src="y"/></a>Fuite &amp; faille&#8230; \[MàJ\]&nbsp;CVE</div>"#;
        assert_eq!(clean_text(input), "Fuite & faille… [MàJ] CVE");
        assert_eq!(clean_text("   "), "");
    }

    #[test]
    fn clean_text_decodes_legacy_entities_without_semicolon() {
        assert_eq!(clean_text("R&amp D &copy 2026"), "R& D © 2026");
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("héllo wörld again", 8), "héllo w…");
        assert_eq!(truncate("abc", 0), "…");
    }

    #[test]
    fn fold_lowercases_and_removes_accents() {
        assert_eq!(fold("IA Générative, Sécurité & Ça"), "ia generative, securite & ca");
    }
}
