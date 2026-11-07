//! Escape-aware width, stripping and truncation for styled terminal strings.

use unicode_width::UnicodeWidthChar;

enum Piece<'a> {
    Escape(&'a str),
    Text(char),
}

/// Splits CSI (`ESC [`), OSC (`ESC ]`, ended by BEL or `ESC \`) and two-byte escapes from printable characters.
fn pieces(s: &str) -> impl Iterator<Item = Piece<'_>> {
    let mut rest = s;
    std::iter::from_fn(move || {
        let c = rest.chars().next()?;
        if c != '\x1b' {
            rest = &rest[c.len_utf8()..];
            return Some(Piece::Text(c));
        }
        let end = escape_len(rest);
        let (escape, tail) = rest.split_at(end);
        rest = tail;
        Some(Piece::Escape(escape))
    })
}

fn escape_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    match bytes.get(1) {
        Some(b'[') => bytes[2..].iter().position(|b| (0x40..=0x7e).contains(b)).map_or(bytes.len(), |i| i + 3),
        Some(b']') => {
            let mut i = 2;
            while i < bytes.len() {
                match bytes[i] {
                    0x07 => return i + 1,
                    0x1b if bytes.get(i + 1) == Some(&b'\\') => return i + 2,
                    _ => i += 1,
                }
            }
            bytes.len()
        }
        Some(_) => 1 + s[1..].chars().next().map_or(0, char::len_utf8),
        None => 1,
    }
}

pub fn strip(s: &str) -> String {
    pieces(s)
        .filter_map(|p| match p {
            Piece::Text(c) => Some(c),
            Piece::Escape(_) => None,
        })
        .collect()
}

pub fn width(s: &str) -> usize {
    pieces(s)
        .map(|p| match p {
            Piece::Text(c) => c.width().unwrap_or(0),
            Piece::Escape(_) => 0,
        })
        .sum()
}

/// Keeps every escape sequence so styles still reset after the cut; only printable text is dropped.
pub fn truncate(s: &str, max_width: usize, tail: &str) -> String {
    if width(s) <= max_width {
        return s.to_string();
    }
    let budget = max_width.saturating_sub(width(tail));
    let mut out = String::with_capacity(s.len());
    let mut used = 0;
    let mut cut = false;
    for piece in pieces(s) {
        match piece {
            Piece::Escape(e) => out.push_str(e),
            Piece::Text(_) if cut => {}
            Piece::Text(c) => {
                let w = c.width().unwrap_or(0);
                if used + w > budget {
                    cut = true;
                    out.push_str(tail);
                    continue;
                }
                used += w;
                out.push(c);
            }
        }
    }
    out
}

/// OSC 8 hyperlink, see https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda
pub fn hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x07{text}\x1b]8;;\x07")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLED: &str = "\x1b[1;38;2;0;255;156mneon\x1b[0m";

    #[test]
    fn strip_and_width_ignore_csi_and_osc() {
        assert_eq!(strip(STYLED), "neon");
        assert_eq!(width(STYLED), 4);
        let link = hyperlink("https://x", "t");
        assert_eq!((strip(&link).as_str(), width(&link)), ("t", 1));
        assert_eq!(width("\x1b]8;;https://x\x1b\\t\x1b]8;;\x1b\\"), 1);
    }

    #[test]
    fn width_counts_wide_characters() {
        assert_eq!(width("日本"), 4);
        assert_eq!(width("é▀█"), 3);
    }

    #[test]
    fn truncate_keeps_escapes_and_appends_tail() {
        assert_eq!(truncate(STYLED, 10, "…"), STYLED);
        let cut = truncate(STYLED, 3, "…");
        assert_eq!(strip(&cut), "ne…");
        assert!(cut.ends_with("\x1b[0m"), "{cut:?}");
        assert_eq!(width(&truncate("日本語", 3, "…")), 3);
        assert_eq!(truncate("abcdef", 0, "…"), "…");
    }

    #[test]
    fn unterminated_escapes_do_not_panic() {
        assert_eq!(strip("a\x1b["), "a");
        assert_eq!(strip("a\x1b]8;;x"), "a");
        assert_eq!(strip("a\x1b"), "a");
    }
}
