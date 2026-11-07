use jiff::{SignedDuration, Timestamp};

use super::ansi;
use super::theme::{muted, neon, red};

pub fn bytes(n: i64) -> String {
    if n < 0 {
        return "?".to_string();
    }
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

pub fn ago(t: Option<Timestamp>, now: Timestamp) -> String {
    let Some(t) = t else { return "undated".to_string() };
    let d = now.duration_since(t);
    if d < SignedDuration::from_mins(1) {
        "just now".to_string()
    } else if d < SignedDuration::from_hours(1) {
        format!("{}m ago", d.as_secs() / 60)
    } else if d < SignedDuration::from_hours(48) {
        format!("{}h ago", d.as_secs() / 3600)
    } else {
        format!("{}d ago", d.as_secs() / 86_400)
    }
}

/// Formats a launch time as T-2d 04:12:33 (T+ once passed).
pub fn countdown(target: Timestamp, now: Timestamp) -> String {
    let d = target.duration_since(now);
    let sign = if d.is_negative() { "T+" } else { "T-" };
    let secs = d.as_secs().unsigned_abs();
    let (days, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if days > 0 { format!("{sign}{days}d {h:02}:{m:02}:{s:02}") } else { format!("{sign}{h:02}:{m:02}:{s:02}") }
}

pub fn bar(fraction: f64, width: usize) -> String {
    let full = (fraction.clamp(0.0, 1.0) * width as f64).round() as usize;
    neon("█".repeat(full)) + &muted("░".repeat(width - full))
}

pub fn percent(f: f64) -> String {
    format!("{:3.0}%", f * 100.0)
}

pub fn delta(pp: f64) -> String {
    if pp >= 0.5 {
        neon(format!("▲{pp:+.1}pp"))
    } else if pp <= -0.5 {
        red(format!("▼{pp:+.1}pp"))
    } else {
        muted(format!("■{pp:+.1}pp"))
    }
}

pub fn pad_right(s: &str, width: usize) -> String {
    let w = ansi::width(s);
    if w >= width { ansi::truncate(s, width, "…") } else { format!("{s}{}", " ".repeat(width - w)) }
}

pub fn pad_left(s: &str, width: usize) -> String {
    let w = ansi::width(s);
    if w >= width { s.to_string() } else { format!("{}{s}", " ".repeat(width - w)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> Timestamp {
        rfc3339.parse().unwrap()
    }

    #[test]
    fn bytes_use_si_units() {
        for (n, want) in [(-1, "?"), (0, "0 B"), (999, "999 B"), (1500, "1.5 kB"), (23_120_000_000, "23.1 GB")] {
            assert_eq!(bytes(n), want, "{n}");
        }
    }

    #[test]
    fn ago_buckets() {
        let now = at("2026-10-05T12:00:00Z");
        for (offset, want) in [(30, "just now"), (5 * 60, "5m ago"), (3 * 3600, "3h ago"), (72 * 3600, "3d ago")] {
            assert_eq!(ago(Some(now - SignedDuration::from_secs(offset)), now), want);
        }
        assert_eq!(ago(None, now), "undated");
        assert_eq!(ago(Some(now + SignedDuration::from_hours(2)), now), "just now");
    }

    #[test]
    fn countdown_before_and_after() {
        let now = at("2026-10-05T12:00:00Z");
        assert_eq!(countdown(now + SignedDuration::from_secs(26 * 3600 + 3 * 60 + 4), now), "T-1d 02:03:04");
        assert_eq!(countdown(now - SignedDuration::from_secs(90), now), "T+00:01:30");
    }

    #[test]
    fn bar_clamps_and_pads() {
        for f in [-1.0, 0.0, 0.5, 2.0] {
            assert_eq!(ansi::width(&bar(f, 10)), 10, "{f}");
        }
        assert_eq!(ansi::strip(&bar(0.5, 10)), "█████░░░░░");
    }

    #[test]
    fn percent_and_delta() {
        assert_eq!(percent(0.342), " 34%");
        for (pp, want) in [(3.04, "▲+3.0pp"), (-2.0, "▼-2.0pp"), (0.1, "■+0.1pp")] {
            assert_eq!(ansi::strip(&delta(pp)), want);
        }
    }

    #[test]
    fn padding() {
        assert_eq!(pad_right("ab", 4), "ab  ");
        assert_eq!(pad_left("ab", 4), "  ab");
        assert_eq!(pad_left("abcdef", 3), "abcdef");
        assert_eq!(ansi::width(&pad_right("abcdefgh", 4)), 4);
    }
}
