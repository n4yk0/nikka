use std::thread;
use std::time::Duration;

use super::font::big_text;
use super::format::pad_right;
use super::theme::{amber, cyan, muted, neon, text};
use super::{INDENT, Ui, ansi};
use crate::logo;

#[derive(Clone, Debug, Default)]
pub struct BannerInfo {
    pub version: String,
    pub operator: String,
    pub kernel: String,
    pub uptime: String,
    pub clock: String,
}

const BANNER_GAP: usize = 3;
const TYPE_INTERVAL: Duration = Duration::from_millis(6);

impl Ui {
    fn panel(&self, info: &BannerInfo) -> Vec<String> {
        let mut lines = big_text("N4YK0");
        let spaced: Vec<String> = "ROUTINE".chars().map(String::from).collect();
        lines.push(cyan(spaced.join(" ")) + &muted(format!("  v{}  ·  daily ops & intel", info.version)));
        lines.push(String::new());
        let field = |k: &str, v: &str| neon("› ") + &muted(pad_right(k, 9)) + &text(v);
        lines.extend([
            field("operator", &info.operator),
            field("kernel", &info.kernel),
            field("uptime", &info.uptime),
            field("clock", &info.clock),
        ]);
        lines
    }

    pub fn banner(&self, info: &BannerInfo) {
        let panel = self.panel(info);
        let panel_width = panel.iter().map(|l| ansi::width(l)).max().unwrap_or(0);
        let opts = logo::Options {
            below: self.width() < 1 + logo::width() + BANNER_GAP + panel_width,
            side: panel,
            gap: BANNER_GAP,
            indent: 1,
            color: self.inner.color,
            ..logo::Options::default()
        };
        self.blank();
        let mut out = self.out();
        let _ = if self.animated() { logo::animate(&mut *out, &opts) } else { logo::print(&mut *out, &opts) };
    }

    /// Types each line out like a terminal boot sequence when animations are on.
    pub fn boot_log(&self, lines: &[String]) {
        for l in lines {
            if !self.animated() {
                self.line(l);
                continue;
            }
            let plain: Vec<char> = ansi::strip(l).chars().collect();
            for i in (1..=plain.len()).step_by(3) {
                self.write_raw(&format!("\r\x1b[2K{INDENT}{}", muted(plain[..i].iter().collect::<String>())));
                thread::sleep(TYPE_INTERVAL);
            }
            self.write_raw(&format!("\r\x1b[2K{INDENT}{l}\n"));
        }
    }
}

pub fn boot_ok(msg: &str) -> String {
    muted("[") + &neon(" OK ") + &muted("] ") + &text(msg)
}

pub fn boot_warn(msg: &str) -> String {
    muted("[") + &amber("WARN") + &muted("] ") + &text(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Options;
    use crate::ui::testing::ui;

    #[test]
    fn static_banner_shows_wordmark_and_fields_without_rewinding() {
        let (u, out) = ui("", Options { width: 120, ..Options::default() });
        u.banner(&BannerInfo { version: "1.2.3".into(), operator: "op@host".into(), ..BannerInfo::default() });
        let plain = out.plain();
        assert!(plain.contains("R O U T I N E") && plain.contains("v1.2.3") && plain.contains("op@host"), "{plain}");
        assert!(!out.text().contains("\x1b[1A"));
        let is_logo = |l: &str| l.contains(['█', '▀', '▄']);
        assert!(plain.lines().any(|l| l.contains("op@host") && is_logo(l)), "panel sits beside the logo:\n{plain}");
    }

    #[test]
    fn narrow_terminals_stack_the_panel_under_the_logo() {
        let (u, out) = ui("", Options { width: 50, ..Options::default() });
        u.banner(&BannerInfo { operator: "op@host".into(), ..BannerInfo::default() });
        let plain = out.plain();
        let lines: Vec<&str> = plain.lines().collect();
        let operator = lines.iter().position(|l| l.contains("op@host")).unwrap();
        assert!(operator > logo::height() && !lines[operator].contains('▀'), "{plain}");
    }

    #[test]
    fn boot_lines_print_plainly_without_animation() {
        let (u, out) = ui("", Options::default());
        u.boot_log(&[boot_ok("config loaded"), boot_warn("no credentials")]);
        assert_eq!(out.plain(), "  [ OK ] config loaded\n  [WARN] no credentials\n");
    }
}
