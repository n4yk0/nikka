use std::path::{Path, PathBuf};
use std::thread;

use super::{Level, Routine};
use crate::cleanup::{self, BoxError, Estimate, Target};
use crate::ui::{amber, bytes, magenta, muted, neon, pad_left, pad_right, red, text};

const SNAP_DIR: &str = "/var/lib/snapd/snaps";
const APT_ARCHIVES: &str = "/var/cache/apt/archives";

impl Routine {
    fn cleanup_options(&self) -> cleanup::Options {
        let c = &self.cfg.clean;
        cleanup::Options {
            home: self.home.clone(),
            tmp_dir: PathBuf::from(&c.tmp_dir),
            tmp_max_age_days: c.tmp_max_age_days,
            tmp_exclude: c.tmp_exclude.clone(),
            thumbnails_max_age_days: c.thumbnails_max_age_days,
            trash_max_age_days: c.trash_max_age_days,
            journal_max_age: c.journal_max_age.clone(),
            docker_images: c.docker_images,
            docker_build_cache: c.docker_build_cache,
            docker_stopped_containers: c.docker_stopped_containers,
            snap_dir: PathBuf::from(SNAP_DIR),
            apt_archives: PathBuf::from(APT_ARCHIVES),
        }
    }

    pub(super) fn run_cleanup(&mut self) {
        let runner = self.runner.clone();
        let (targets, notices) = cleanup::targets(runner.as_ref(), &self.cleanup_options());

        let spin = self.ui.spin(&format!("estimating {} purge targets", targets.len()));
        let estimates = estimate_all(&targets);
        spin.stop();

        let mut total = 0;
        for (target, estimate) in targets.iter().zip(&estimates) {
            let (size, detail) = match estimate {
                Ok(Estimate { bytes: Some(n), detail }) => {
                    total += n;
                    (bytes(*n as i64), detail.clone())
                }
                Ok(Estimate { bytes: None, detail }) => ("—".to_string(), detail.clone()),
                Err(e) => ("—".to_string(), red(format!("estimate failed: {e}"))),
            };
            let root = if target.root { magenta("⚿") } else { " ".to_string() };
            let detail = self.ui.truncate(&detail, self.ui.width().saturating_sub(50));
            self.ui.line(&format!(
                "{root} {}{}  {}",
                text(pad_right(&target.name, 30)),
                neon(pad_left(&size, 10)),
                muted(detail)
            ));
        }
        for n in &notices {
            self.ui.line(&format!("{} {}{}", amber("!"), text(pad_right(&n.name, 30)), muted(&n.detail)));
        }
        let reclaimable = bytes(total as i64);
        self.ui.line(&muted(format!(
            "  {}{}  (+ items marked —)",
            pad_right("estimated reclaimable", 30),
            pad_left(&reclaimable, 10)
        )));

        if self.opts.dry_run {
            self.ui.blank();
            self.ui.step("dry-run, would execute:");
            for c in targets.iter().flat_map(|t| &t.commands) {
                self.ui.line(&muted(format!("  $ {c}")));
            }
            self.note(Level::Ok, format!("purge: ~{reclaimable} reclaimable (dry-run)"));
            return;
        }
        if self.opts.check_only {
            self.note(Level::Ok, format!("purge: ~{reclaimable} reclaimable (check-only)"));
            return;
        }
        self.ui.blank();
        if !self.ui.confirm("purge all targets?", true) || self.stop_requested() {
            self.note(Level::Ok, "purge: postponed by operator".to_string());
            return;
        }
        let root_ok = !targets.iter().any(|t| t.root) || self.ensure_root();

        let before = free_bytes(Path::new("/"));
        let mut failures = 0;
        for target in &targets {
            if self.stop_requested() {
                break;
            }
            if target.root && !root_ok {
                self.ui.info(format!("{} skipped (no root)", target.name));
                continue;
            }
            self.ui.step(&target.name);
            let mut w = self.ui.stream_writer();
            let result = target.run(&mut w);
            w.finish();
            if let Err(e) = result {
                failures += 1;
                self.ui.fail(format!("{}: {e}", target.name));
            }
        }
        let freed = bytes(free_bytes(Path::new("/")).saturating_sub(before) as i64);
        self.ui.ok(format!("reclaimed {freed} on /"));
        if failures > 0 {
            self.note(Level::Fail, format!("purge: {freed} reclaimed on /, {failures} targets failed"));
        } else {
            self.note(Level::Ok, format!("purge: {freed} reclaimed on /"));
        }
    }
}

fn estimate_all(targets: &[Target<'_>]) -> Vec<Result<Estimate, BoxError>> {
    thread::scope(|s| {
        let handles: Vec<_> = targets.iter().map(|t| s.spawn(move || t.estimate())).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("estimate panicked".into()))).collect()
    })
}

fn free_bytes(path: &Path) -> u64 {
    rustix::fs::statvfs(path).map_or(0, |st| st.f_bavail.saturating_mul(st.f_frsize))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File, FileTimes};
    use std::time::{Duration, SystemTime};

    use super::super::testkit::{FakeRoot, Setup, harness};
    use super::super::{Options, Outcome, Section};
    use super::*;
    use crate::config::Config;
    use crate::execx::{Fake, cmd};
    use crate::ui;

    fn docker_fake() -> Fake {
        Fake::new().binary("docker").on(
            cmd("docker", ["system", "df", "--format", "{{json .}}"]),
            "{\"Reclaimable\":\"2.5GB (40%)\",\"Type\":\"Images\"}\n{\"Reclaimable\":\"500MB\",\"Type\":\"Build Cache\"}\n",
        )
    }

    fn clean_config(tmp: &Path) -> Config {
        let mut cfg = Config::default();
        cfg.clean.tmp_dir = tmp.display().to_string();
        cfg
    }

    #[test]
    fn dry_run_lists_targets_and_commands_without_pruning() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = harness(Setup {
            sections: vec![Section::Clean],
            opts: Options { dry_run: true, ..Options::default() },
            cfg: clean_config(tmp.path()),
            fake: docker_fake(),
            ..Setup::default()
        });
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        for want in [
            "docker unused images",
            "2.5 GB",
            "docker build cache",
            "$ docker image prune --all --force",
            "docker volumes",
            "never pruned",
            "~3.0 GB reclaimable (dry-run)",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert!(!h.runner.ran().iter().any(|c| c.contains("prune")), "{:?}", h.runner.ran());
    }

    #[test]
    fn confirmed_purge_removes_stale_files_and_runs_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("old.log");
        fs::write(&stale, "x").unwrap();
        let month_ago = SystemTime::now() - Duration::from_secs(30 * 86_400);
        File::options().write(true).open(&stale).unwrap().set_times(FileTimes::new().set_modified(month_ago)).unwrap();
        let fake = docker_fake()
            .on(cmd("docker", ["image", "prune", "--all", "--force"]), "Total reclaimed space: 2.5GB\n")
            .on(cmd("docker", ["builder", "prune", "--all", "--force"]), "Total: 500MB\n");
        let mut h = harness(Setup {
            sections: vec![Section::Clean],
            cfg: clean_config(tmp.path()),
            fake,
            ui: ui::Options { assume_yes: true, width: 110, ..ui::Options::default() },
            ..Setup::default()
        });
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        assert!(!stale.exists(), "{plain}");
        for want in ["│ Total reclaimed space: 2.5GB", "│ removed 1/1 entries", "reclaimed", "purge: "] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert_eq!(h.root.calls(), 0, "no target needed root");
    }

    #[test]
    fn root_targets_are_skipped_when_sudo_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new().binary("systemd-tmpfiles");
        let mut h = harness(Setup {
            sections: vec![Section::Clean],
            cfg: clean_config(tmp.path()),
            fake,
            root: FakeRoot::denying(),
            ui: ui::Options { assume_yes: true, width: 110, ..ui::Options::default() },
            ..Setup::default()
        });
        assert_eq!(h.routine.run(), Outcome::Failed);
        let plain = h.out.plain();
        assert!(plain.contains("system tmpfiles policy skipped (no root)"), "{plain}");
        assert!(h.runner.ran().is_empty(), "{:?}", h.runner.ran());
    }

    #[test]
    fn failed_estimates_are_shown_but_do_not_stop_the_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = harness(Setup {
            sections: vec![Section::Clean],
            opts: Options { check_only: true, ..Options::default() },
            cfg: clean_config(tmp.path()),
            fake: Fake::new().binary("docker"),
            ..Setup::default()
        });
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        assert!(plain.contains("estimate failed: docker system df"), "{plain}");
        assert!(plain.contains("reclaimable (check-only)"), "{plain}");
    }

    #[test]
    fn free_bytes_reads_the_root_filesystem() {
        assert!(free_bytes(Path::new("/")) > 0);
        assert_eq!(free_bytes(Path::new("/definitely/not/here")), 0);
    }
}
