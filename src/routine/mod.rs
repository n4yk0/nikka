//! Orchestrates the daily run: patch, purge, then the intel briefing.

mod clean;
mod intel;
mod market;
mod sys;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jiff::Zoned;

use crate::config::Config;
use crate::execx::{RootAccess, Runner};
use crate::httpx;
use crate::ui::{self, BannerInfo, Ui};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Sys,
    Clean,
    News,
    Skills,
    Jobs,
}

impl Section {
    pub fn key(self) -> &'static str {
        match self {
            Section::Sys => "sys",
            Section::Clean => "clean",
            Section::News => "news",
            Section::Skills => "skills",
            Section::Jobs => "jobs",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub sections: Vec<Section>,
    pub dry_run: bool,
    pub check_only: bool,
    pub config_path: PathBuf,
    pub config_seen: bool,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

pub struct Deps {
    pub ui: Ui,
    pub runner: Arc<dyn Runner>,
    pub root: Arc<dyn RootAccess>,
    pub http: httpx::Client,
    /// Set by SIGINT/SIGTERM; checked between steps so running package managers are never cut off.
    pub interrupted: Arc<AtomicBool>,
}

pub struct Routine {
    cfg: Config,
    opts: Options,
    ui: Ui,
    runner: Arc<dyn Runner>,
    root: Arc<dyn RootAccess>,
    http: httpx::Client,
    interrupted: Arc<AtomicBool>,
    now: fn() -> Zoned,
    endpoints: intel::Endpoints,
    run_dir: PathBuf,
    home: PathBuf,
    root_asked: bool,
    root_ok: bool,
    interruption_noted: bool,
    summary: Vec<(Level, String)>,
}

impl Routine {
    pub fn new(cfg: Config, opts: Options, deps: Deps) -> Routine {
        Routine {
            cfg,
            opts,
            ui: deps.ui,
            runner: deps.runner,
            root: deps.root,
            http: deps.http,
            interrupted: deps.interrupted,
            now: Zoned::now,
            endpoints: intel::Endpoints::default(),
            run_dir: PathBuf::from("/var/run"),
            home: std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default(),
            root_asked: false,
            root_ok: false,
            interruption_noted: false,
            summary: Vec::new(),
        }
    }

    fn enabled(&self, section: Section) -> bool {
        self.opts.sections.contains(&section)
    }

    fn note(&mut self, level: Level, text: String) {
        self.summary.push((level, text));
    }

    /// Every failure is printed as it happens; the outcome only drives the exit code.
    pub fn run(&mut self) -> Outcome {
        let start = (self.now)();
        self.banner();
        let mut intel = self.prefetch();
        let mut index = 1;
        let jobs_subtitle =
            format!("targets ≤ {:.0} min from {}", self.cfg.jobs.max_drive_minutes, self.cfg.jobs.origin_name);
        let plan = [
            (Section::Sys, "SYS.PATCH", "package integrity & updates".to_string()),
            (Section::Clean, "SYS.PURGE", "dead weight removal".to_string()),
            (Section::News, "INTEL.FEED", "world uplink · most important stories".to_string()),
            (Section::Skills, "MARKET.SKILLS", "what the software market is buying".to_string()),
            (Section::Jobs, "MARKET.JOBS", jobs_subtitle),
        ];
        for (section, name, subtitle) in plan {
            if !self.enabled(section) {
                continue;
            }
            if self.stop_requested() {
                break;
            }
            self.ui.section(index, name, &subtitle);
            match section {
                Section::Sys => self.run_system(),
                Section::Clean => self.run_cleanup(),
                Section::News => self.show_news(&mut intel),
                Section::Skills => self.show_skills(&mut intel),
                Section::Jobs => self.show_jobs(&mut intel),
            }
            index += 1;
        }
        self.mission_report(start);
        if self.summary.iter().any(|(level, _)| *level == Level::Fail) { Outcome::Failed } else { Outcome::Completed }
    }

    fn stop_requested(&mut self) -> bool {
        if !self.interrupted.load(Ordering::Relaxed) {
            return false;
        }
        if !self.interruption_noted {
            self.interruption_noted = true;
            self.ui.warn("interrupted by operator, remaining steps skipped");
            self.note(Level::Fail, "interrupted by operator, remaining steps skipped".to_string());
        }
        true
    }

    fn banner(&self) {
        let host = read_proc("/proc/sys/kernel/hostname");
        let user =
            std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| "unknown".to_string());
        let operator = if host.is_empty() { user } else { format!("{user}@{host}") };
        let kernel =
            Some(read_proc("/proc/sys/kernel/osrelease")).filter(|k| !k.is_empty()).unwrap_or_else(|| "?".to_string());
        self.ui.banner(&BannerInfo {
            version: self.opts.version.clone(),
            operator,
            kernel,
            uptime: uptime(&read_proc("/proc/uptime")),
            clock: (self.now)().strftime("%a %Y-%m-%d %H:%M %Z").to_string(),
        });
        self.ui.blank();

        let config_line = if self.opts.config_seen {
            ui::boot_ok(&format!("config {}", self.opts.config_path.display()))
        } else {
            ui::boot_warn("no config file, built-in defaults (run `nikka init`)")
        };
        let mode = match (self.opts.dry_run, self.opts.check_only) {
            (true, _) => "dry-run · nothing will be modified",
            (_, true) => "check-only · no upgrade, no purge",
            _ => "live",
        };
        let modules: Vec<&str> = self.opts.sections.iter().map(|s| s.key()).collect();
        let mut lines = vec![
            config_line,
            ui::boot_ok(&format!("mode {mode}")),
            ui::boot_ok(&format!("modules {}", modules.join(" · "))),
        ];
        if self.enabled(Section::Jobs) || self.enabled(Section::Skills) {
            lines.push(match self.job_boards_label() {
                Some(boards) => ui::boot_ok(&format!("job boards: {boards}")),
                None => ui::boot_warn("no job board credentials · offers disabled (see README)"),
            });
        }
        self.ui.boot_log(&lines);
    }

    fn job_boards_label(&self) -> Option<String> {
        let mut boards = Vec::new();
        if self.cfg.has_france_travail() {
            boards.push("France Travail");
        }
        if self.cfg.has_adzuna() {
            boards.push("Adzuna");
        }
        (!boards.is_empty()).then(|| boards.join(" + "))
    }

    fn mission_report(&self, start: Zoned) {
        self.ui.section(0xFF, "MISSION.REPORT", "debrief");
        for (level, text) in &self.summary {
            match level {
                Level::Ok => self.ui.ok(text),
                Level::Warn => self.ui.warn(text),
                Level::Fail => self.ui.fail(text),
            }
        }
        let took = (self.now)().timestamp().duration_since(start.timestamp());
        self.ui.info(format!("completed in {}", round_seconds(took.try_into().unwrap_or_default())));
        self.ui.blank();
        self.ui.line(&(ui::neon("> ") + &ui::muted("stay curious. stay patched. ") + &ui::neon("n4yk0 out_")));
        self.ui.blank();
    }
}

fn read_proc(path: &str) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn uptime(proc_uptime: &str) -> String {
    let Some(seconds) = proc_uptime.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()) else {
        return "?".to_string();
    };
    let total = seconds as u64;
    format!("{}d {:02}h {:02}m", total / 86_400, total / 3600 % 24, total / 60 % 60)
}

/// Rounded to the second: 0s, 45s, 1m12s, 2h0m5s.
fn round_seconds(d: Duration) -> String {
    let total = (d.as_millis() + 500) / 1000;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m{s}s"),
        _ => format!("{h}h{m}m{s}s"),
    }
}

#[cfg(test)]
mod testkit;

#[cfg(test)]
mod tests {
    use super::testkit::{Setup, harness};
    use super::*;

    fn setup(sections: &[Section], opts: Options, cfg: Config) -> Setup {
        Setup { sections: sections.to_vec(), opts, cfg, ..Setup::default() }
    }

    #[test]
    fn sections_are_numbered_in_order_and_reported() {
        let mut h = harness(setup(&[Section::Sys, Section::Jobs], Options::default(), Config::default()));
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        for want in ["0x01  SYS.PATCH", "0x02  MARKET.JOBS", "MISSION.REPORT", "completed in 0s"] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert!(!plain.contains("SYS.PURGE"));
    }

    #[test]
    fn boot_log_reflects_mode_config_and_credentials() {
        let opts =
            Options { dry_run: true, config_seen: true, config_path: "/etc/nikka.toml".into(), ..Options::default() };
        let mut h = harness(setup(&[Section::Jobs], opts, Config::default()));
        h.routine.run();
        let plain = h.out.plain();
        for want in [
            "[ OK ] config /etc/nikka.toml",
            "mode dry-run · nothing will be modified",
            "modules jobs",
            "[WARN] no job board credentials",
            "clock    Mon 2026-10-05 12:00 UTC",
            "targets ≤ 45 min from Bordeaux",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert!(!plain.contains("33000"), "the banner no longer shows the job search origin\n{plain}");
    }

    #[test]
    fn job_boards_label_lists_configured_providers() {
        let mut cfg = Config::default();
        assert_eq!(harness(setup(&[], Options::default(), cfg.clone())).routine.job_boards_label(), None);
        cfg.adzuna.app_id = "a".into();
        cfg.adzuna.app_key = "k".into();
        cfg.france_travail.client_id = "i".into();
        cfg.france_travail.client_secret = "s".into();
        let h = harness(setup(&[], Options::default(), cfg));
        assert_eq!(h.routine.job_boards_label().as_deref(), Some("France Travail + Adzuna"));
    }

    #[test]
    fn an_interrupt_skips_every_remaining_section_and_fails_the_run() {
        let mut h = harness(setup(&[Section::Sys, Section::Clean], Options::default(), Config::default()));
        h.interrupted.store(true, Ordering::Relaxed);
        assert_eq!(h.routine.run(), Outcome::Failed);
        let plain = h.out.plain();
        assert!(plain.contains("interrupted by operator, remaining steps skipped"), "{plain}");
        assert!(!plain.contains("SYS.PATCH") && !plain.contains("SYS.PURGE"), "{plain}");
        assert!(h.runner.ran().is_empty());
    }

    #[test]
    fn uptime_parsing() {
        assert_eq!(uptime("93784.52 12345.00"), "1d 02h 03m");
        assert_eq!(uptime(""), "?");
    }

    #[test]
    fn durations_round_to_the_second() {
        assert_eq!(round_seconds(Duration::from_millis(400)), "0s");
        assert_eq!(round_seconds(Duration::from_millis(44_600)), "45s");
        assert_eq!(round_seconds(Duration::from_secs(72)), "1m12s");
        assert_eq!(round_seconds(Duration::from_secs(7205)), "2h0m5s");
    }
}
