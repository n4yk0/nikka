use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::theme::{cyan, muted, text};
use super::{INDENT, Ui};

const FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
const TICK: Duration = Duration::from_millis(80);

pub struct Spinner {
    ui: Ui,
    start: Instant,
    worker: Option<(Sender<()>, JoinHandle<()>)>,
}

impl Ui {
    /// Animates on a TTY; elsewhere it prints nothing until the outcome is reported.
    pub fn spin(&self, label: &str) -> Spinner {
        let start = Instant::now();
        if !self.tty() {
            return Spinner { ui: self.clone(), start, worker: None };
        }
        let (stop, stopped) = mpsc::channel::<()>();
        let ui = self.clone();
        let label = label.to_string();
        let handle = thread::spawn(move || {
            for frame in FRAMES.iter().cycle() {
                ui.write_raw(&format!("\r\x1b[2K{INDENT}{} {} {}", cyan(frame), text(&label), muted(elapsed(start))));
                if !matches!(stopped.recv_timeout(TICK), Err(RecvTimeoutError::Timeout)) {
                    break;
                }
            }
            ui.write_raw("\r\x1b[2K");
        });
        Spinner { ui: self.clone(), start, worker: Some((stop, handle)) }
    }
}

impl Spinner {
    fn halt(&mut self) {
        if let Some((stop, handle)) = self.worker.take() {
            drop(stop);
            let _ = handle.join();
        }
    }

    pub fn ok(mut self, msg: impl AsRef<str>) {
        self.halt();
        self.ui.ok(format!("{} {}", msg.as_ref(), muted(elapsed(self.start))));
    }

    pub fn warn(mut self, msg: impl AsRef<str>) {
        self.halt();
        self.ui.warn(msg);
    }

    pub fn fail(mut self, msg: impl AsRef<str>) {
        self.halt();
        self.ui.fail(msg);
    }

    pub fn stop(mut self) {
        self.halt();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.halt();
    }
}

fn elapsed(since: Instant) -> String {
    let d = since.elapsed();
    if d < Duration::from_secs(1) { format!("{}ms", d.as_millis()) } else { format!("{:.1}s", d.as_secs_f64()) }
}

#[cfg(test)]
mod tests {
    use crate::ui::Options;
    use crate::ui::testing::ui;

    #[test]
    fn non_tty_spinner_only_prints_the_outcome() {
        let (u, out) = ui("", Options::default());
        u.spin("refreshing").ok("refreshed");
        let plain = out.plain();
        assert!(plain.starts_with("  ✔ refreshed ") && plain.ends_with("ms\n"), "{plain:?}");
        assert!(!plain.contains("refreshing"));
    }

    #[test]
    fn tty_spinner_clears_its_line_before_the_outcome() {
        let (u, out) = ui("", Options { tty: true, ..Options::default() });
        u.spin("working").fail("broken");
        let text = out.text();
        assert!(text.contains("working"), "{text:?}");
        let cleared = text.rfind("\r\x1b[2K").unwrap();
        assert!(text[cleared..].contains("✖ broken"), "{text:?}");
    }
}
