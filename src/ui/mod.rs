//! Hacker-style terminal output: banner, sections, spinners, prompts and streamed command output.

pub mod ansi;
mod banner;
mod font;
mod format;
mod input;
mod spinner;
mod theme;

use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

pub use banner::{BannerInfo, boot_ok, boot_warn};
pub use font::big_text;
pub use format::{ago, bar, bytes, countdown, delta, pad_left, pad_right, percent};
pub use input::InterruptibleInput;
pub use spinner::Spinner;
pub use theme::{amber, badge, bold, cyan, magenta, muted, neon, red, set_color, tag, text, title};

const INDENT: &str = "  ";
const DEFAULT_WIDTH: usize = 100;
const MAX_WIDTH: usize = 120;

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub tty: bool,
    pub color: bool,
    pub animations: bool,
    pub hyperlinks: bool,
    pub assume_yes: bool,
    pub width: usize,
}

/// Cheap to clone: every clone writes to the same output under the same lock.
#[derive(Clone)]
pub struct Ui {
    inner: Arc<Inner>,
}

struct Inner {
    out: Mutex<Box<dyn Write + Send>>,
    input: Mutex<Box<dyn BufRead + Send>>,
    tty: bool,
    color: bool,
    anim: bool,
    hyperlinks: bool,
    assume_yes: bool,
    width: usize,
}

impl Ui {
    pub fn new(out: impl Write + Send + 'static, input: impl BufRead + Send + 'static, opts: Options) -> Ui {
        let width = if opts.width == 0 { DEFAULT_WIDTH } else { opts.width };
        Ui {
            inner: Arc::new(Inner {
                out: Mutex::new(Box::new(out)),
                input: Mutex::new(Box::new(input)),
                tty: opts.tty,
                color: opts.color && opts.tty,
                anim: opts.animations && opts.tty,
                hyperlinks: opts.hyperlinks && opts.tty,
                assume_yes: opts.assume_yes,
                width: width.min(MAX_WIDTH),
            }),
        }
    }

    fn out(&self) -> MutexGuard<'_, Box<dyn Write + Send>> {
        self.inner.out.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_raw(&self, s: &str) {
        let mut out = self.out();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }

    pub fn width(&self) -> usize {
        self.inner.width
    }

    pub fn animated(&self) -> bool {
        self.inner.anim
    }

    pub fn tty(&self) -> bool {
        self.inner.tty
    }

    pub fn print(&self, s: &str) {
        self.write_raw(&format!("{s}\n"));
    }

    pub fn blank(&self) {
        self.print("");
    }

    pub fn line(&self, s: &str) {
        self.print(&format!("{INDENT}{s}"));
    }

    pub fn ok(&self, msg: impl AsRef<str>) {
        self.line(&(neon("✔ ") + &text(msg)));
    }

    pub fn warn(&self, msg: impl AsRef<str>) {
        self.line(&(amber("▲ ") + &text(msg)));
    }

    pub fn fail(&self, msg: impl AsRef<str>) {
        self.line(&(red("✖ ") + &text(msg)));
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        self.line(&muted(format!("· {}", msg.as_ref())));
    }

    pub fn step(&self, msg: impl AsRef<str>) {
        self.line(&(cyan("➜ ") + &text(msg)));
    }

    /// Prints "[0x01] SYS.PATCH // subtitle ─────" padded to the terminal width.
    pub fn section(&self, index: usize, name: &str, subtitle: &str) {
        let head = format!("{} {}{}", badge(format!(" 0x{index:02X} ")), bold(name), muted(format!(" // {subtitle} ")));
        let fill = self.width().saturating_sub(ansi::width(&head) + 2).max(3);
        self.blank();
        self.print(&format!(" {head}{}", muted("─".repeat(fill))));
    }

    pub fn link(&self, url: &str, label: &str) -> String {
        if !self.inner.hyperlinks || url.is_empty() {
            return label.to_string();
        }
        ansi::hyperlink(url, label)
    }

    pub fn truncate(&self, s: &str, width: usize) -> String {
        ansi::truncate(s, width, "…")
    }

    /// Never blocks without a terminal: non-interactive runs answer "no" unless --yes was given.
    pub fn confirm(&self, question: &str, default_yes: bool) -> bool {
        if self.inner.assume_yes {
            self.line(&(magenta("? ") + &text(question) + &muted(" yes (--yes)")));
            return true;
        }
        if !self.inner.tty {
            self.line(&(magenta("? ") + &text(question) + &muted(" no (non-interactive, pass --yes)")));
            return false;
        }
        self.ask(question, if default_yes { "[Y/n]" } else { "[y/N]" }, default_yes)
    }

    /// Ignores --yes: reserved for choices that must stay with a human (package removals).
    pub fn confirm_strict(&self, question: &str) -> bool {
        if !self.inner.tty {
            self.line(&(magenta("? ") + &text(question) + &muted(" no (needs an interactive answer)")));
            return false;
        }
        self.ask(question, "[y/N]", false)
    }

    fn ask(&self, question: &str, hint: &str, default_yes: bool) -> bool {
        self.write_raw(&format!("{INDENT}{}{} {} ", magenta("? "), text(question), muted(hint)));
        let mut answer = String::new();
        let mut input = self.inner.input.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match input.read_line(&mut answer) {
            Ok(0) | Err(_) => false,
            Ok(_) => parse_answer(&answer, default_yes),
        }
    }

    pub fn pause(&self, d: Duration) {
        if self.inner.anim {
            thread::sleep(d);
        }
    }

    /// Prefixes every line of a child process output with a dim gutter.
    pub fn stream_writer(&self) -> StreamWriter {
        StreamWriter { ui: self.clone(), width: self.width().saturating_sub(6), pending: Vec::new() }
    }
}

pub fn parse_answer(answer: &str, default_yes: bool) -> bool {
    match answer.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" | "o" | "oui" => true,
        _ => false,
    }
}

pub fn detect_terminal() -> (bool, usize) {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        return (false, 0);
    }
    match rustix::termios::tcgetwinsize(&stdout) {
        Ok(size) if size.ws_col > 0 => (true, usize::from(size.ws_col)),
        _ => (true, DEFAULT_WIDTH),
    }
}

pub struct StreamWriter {
    ui: Ui,
    width: usize,
    pending: Vec<u8>,
}

impl StreamWriter {
    fn emit(&self, raw: &[u8]) {
        let line = ansi::strip(&String::from_utf8_lossy(raw));
        let line = line.trim_end_matches([' ', '\t']);
        if line.is_empty() {
            return;
        }
        self.ui.print(&format!("{INDENT}{}", muted(format!("│ {}", ansi::truncate(line, self.width.max(20), "…")))));
    }

    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.emit(&rest);
        }
    }
}

impl Write for StreamWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(i) = self.pending.iter().position(|&b| b == b'\n' || b == b'\r') {
            let line: Vec<u8> = self.pending.drain(..=i).collect();
            self.emit(&line[..i]);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
pub mod testing {
    use std::io::{self, Cursor, Write};
    use std::sync::{Arc, Mutex};

    use super::{Options, Ui};

    #[derive(Clone, Default)]
    pub struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }

        pub fn plain(&self) -> String {
            super::ansi::strip(&self.text())
        }
    }

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub fn ui(input: &str, opts: Options) -> (Ui, Capture) {
        let capture = Capture::default();
        (Ui::new(capture.clone(), Cursor::new(input.to_string().into_bytes()), opts), capture)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::ui;
    use super::*;

    #[test]
    fn parse_answer_accepts_english_and_french() {
        for (input, default_yes, want) in [
            ("\n", true, true),
            ("\n", false, false),
            ("y\n", false, true),
            ("Oui\n", false, true),
            ("n\n", true, false),
            ("maybe\n", true, false),
        ] {
            assert_eq!(parse_answer(input, default_yes), want, "{input:?} {default_yes}");
        }
    }

    #[test]
    fn confirm_modes() {
        assert!(ui("", Options { assume_yes: true, ..Options::default() }).0.confirm("go?", false));
        assert!(!ui("y\n", Options::default()).0.confirm("go?", true), "non-interactive must refuse without --yes");
        assert!(ui("y\n", Options { tty: true, ..Options::default() }).0.confirm("go?", false));
        assert!(!ui("", Options { tty: true, ..Options::default() }).0.confirm("go?", true), "EOF must refuse");
    }

    #[test]
    fn confirm_strict_ignores_yes() {
        assert!(!ui("", Options { assume_yes: true, ..Options::default() }).0.confirm_strict("remove?"));
        assert!(!ui("\n", Options { assume_yes: true, tty: true, ..Options::default() }).0.confirm_strict("remove?"));
        assert!(ui("y\n", Options { assume_yes: true, tty: true, ..Options::default() }).0.confirm_strict("remove?"));
    }

    #[test]
    fn stream_writer_splits_lines_and_carriage_returns() {
        let (u, out) = ui("", Options { width: 80, ..Options::default() });
        {
            let mut w = u.stream_writer();
            w.write_all(b"Get:1 http://x\nprogress 10%\rprogress 100%\n\n  ").unwrap();
            w.write_all(b"tail without newline").unwrap();
        }
        let plain = out.plain();
        let lines: Vec<&str> = plain.trim_end_matches('\n').split('\n').collect();
        let want = ["Get:1 http://x", "progress 10%", "progress 100%", "tail without newline"];
        assert_eq!(lines.len(), want.len(), "{lines:?}");
        for (line, want) in lines.iter().zip(want) {
            assert!(line.ends_with(want) && line.contains('│'), "{line:?}");
        }
    }

    #[test]
    fn links_only_on_tty() {
        let plain = ui("", Options { hyperlinks: true, ..Options::default() }).0;
        assert_eq!(plain.link("https://x", "t"), "t");
        let tty = ui("", Options { hyperlinks: true, tty: true, ..Options::default() }).0;
        let link = tty.link("https://x", "t");
        assert!(link.contains("https://x") && ansi::width(&link) == 1, "{link:?}");
        assert_eq!(tty.link("", "t"), "t");
    }

    #[test]
    fn section_fills_the_line() {
        let (u, out) = ui("", Options { width: 60, ..Options::default() });
        u.section(1, "SYS.PATCH", "updates");
        let plain = out.plain();
        let line = plain.lines().last().unwrap();
        assert!(line.starts_with("  0x01  SYS.PATCH // updates ─"), "{line:?}");
        assert_eq!(ansi::width(line), 59);
    }

    #[test]
    fn width_is_capped() {
        assert_eq!(ui("", Options { width: 300, ..Options::default() }).0.width(), MAX_WIDTH);
        assert_eq!(ui("", Options::default()).0.width(), DEFAULT_WIDTH);
    }
}
