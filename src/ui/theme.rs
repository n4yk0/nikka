//! Palette and text styles; styling is a no-op until `set_color(true)`, so piped output stays plain.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const NEON: Rgb = Rgb(0x00, 0xFF, 0x9C);
pub const CYAN: Rgb = Rgb(0x00, 0xE5, 0xFF);
pub const MAGENTA: Rgb = Rgb(0xFF, 0x2E, 0x97);
pub const AMBER: Rgb = Rgb(0xFF, 0xB0, 0x00);
pub const RED: Rgb = Rgb(0xFF, 0x4D, 0x4D);
pub const MUTED: Rgb = Rgb(0x5C, 0x6F, 0x7F);
pub const TEXT: Rgb = Rgb(0xD7, 0xE3, 0xEC);
const BADGE_INK: Rgb = Rgb(0x04, 0x14, 0x0D);
const TAG_BACKGROUND: Rgb = Rgb(0x0E, 0x2A, 0x33);

static COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_color(enabled: bool) {
    COLOR.store(enabled, Ordering::Relaxed);
}

pub fn color_enabled() -> bool {
    COLOR.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Default)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
}

impl Style {
    pub const fn fg(c: Rgb) -> Style {
        Style { fg: Some(c), bg: None, bold: false }
    }

    pub fn paint(self, s: &str) -> String {
        if s.is_empty() || !color_enabled() {
            return s.to_string();
        }
        self.sgr() + s + "\x1b[0m"
    }

    fn sgr(self) -> String {
        let mut codes = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if let Some(Rgb(r, g, b)) = self.fg {
            codes.push(format!("38;2;{r};{g};{b}"));
        }
        if let Some(Rgb(r, g, b)) = self.bg {
            codes.push(format!("48;2;{r};{g};{b}"));
        }
        format!("\x1b[{}m", codes.join(";"))
    }
}

pub fn neon(s: impl AsRef<str>) -> String {
    Style::fg(NEON).paint(s.as_ref())
}

pub fn bold(s: impl AsRef<str>) -> String {
    Style { bold: true, ..Style::fg(NEON) }.paint(s.as_ref())
}

pub fn cyan(s: impl AsRef<str>) -> String {
    Style::fg(CYAN).paint(s.as_ref())
}

pub fn magenta(s: impl AsRef<str>) -> String {
    Style::fg(MAGENTA).paint(s.as_ref())
}

pub fn amber(s: impl AsRef<str>) -> String {
    Style::fg(AMBER).paint(s.as_ref())
}

pub fn red(s: impl AsRef<str>) -> String {
    Style::fg(RED).paint(s.as_ref())
}

pub fn muted(s: impl AsRef<str>) -> String {
    Style::fg(MUTED).paint(s.as_ref())
}

pub fn text(s: impl AsRef<str>) -> String {
    Style::fg(TEXT).paint(s.as_ref())
}

pub fn title(s: impl AsRef<str>) -> String {
    Style { bold: true, ..Style::fg(TEXT) }.paint(s.as_ref())
}

pub fn tag(s: impl AsRef<str>) -> String {
    Style { bg: Some(TAG_BACKGROUND), ..Style::fg(CYAN) }.paint(&format!(" {} ", s.as_ref()))
}

pub fn badge(s: impl AsRef<str>) -> String {
    Style { fg: Some(BADGE_INK), bg: Some(NEON), bold: true }.paint(s.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_combines_bold_foreground_and_background() {
        let style = Style { fg: Some(NEON), bg: Some(TAG_BACKGROUND), bold: true };
        assert_eq!(style.sgr(), "\x1b[1;38;2;0;255;156;48;2;14;42;51m");
    }

    #[test]
    fn styles_are_plain_while_color_is_off() {
        assert_eq!(neon("x"), "x");
        assert_eq!(tag("rust"), " rust ");
        assert_eq!(Style::fg(RED).paint(""), "");
    }
}
