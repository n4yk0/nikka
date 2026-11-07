use std::io;

use super::{Level, Routine};
use crate::system::{self, Manager, Plan};
use crate::ui::{amber, bold, magenta, muted, pad_right, text};

const MAX_LISTED_UPDATES: usize = 8;

impl Routine {
    /// Only reminds: the routine never waits for, nor triggers, a reboot.
    fn reboot_check(&mut self) {
        let Some(packages) = system::reboot_required(&self.run_dir) else { return };
        let detail = if packages.is_empty() { String::new() } else { format!(" ({})", packages.join(", ")) };
        self.ui.warn(format!("reboot pending{detail}, continuing without it"));
        self.ui.info("reboot when convenient: systemctl reboot");
        self.note(Level::Warn, format!("reboot pending{detail}, run `systemctl reboot` when convenient"));
    }

    /// Asks for sudo once per run, the first time a step really needs it.
    pub(super) fn ensure_root(&mut self) -> bool {
        if self.root_asked {
            return self.root_ok;
        }
        self.root_asked = true;
        self.ui.line(&(magenta("⚿ ") + &text("root clearance required")));
        if let Err(e) = self.root.ensure() {
            self.ui.fail(format!("root access denied: {e}"));
            self.note(Level::Fail, "root access denied, privileged steps skipped".to_string());
            return false;
        }
        self.root_ok = true;
        true
    }

    pub(super) fn run_system(&mut self) {
        let runner = self.runner.clone();
        let managers = system::detect(runner.as_ref(), &self.cfg.system.managers);
        if managers.is_empty() {
            self.ui.warn("no supported package manager found");
            return;
        }
        let names: Vec<&str> = managers.iter().map(|m| m.name).collect();
        self.ui.info(format!("detected: {}", names.join(", ")));

        let mut checked: Vec<(Manager, Plan)> = Vec::new();
        let mut pending = 0;
        for m in managers {
            if self.stop_requested() {
                return;
            }
            if let Some(refresh) = &m.refresh {
                if self.opts.dry_run {
                    self.ui.info(format!("{}: index refresh skipped (dry-run)", m.name));
                } else if !self.ensure_root() {
                    self.ui.info(format!("{}: index refresh skipped (no root)", m.name));
                } else {
                    let spin = self.ui.spin(&format!("{}: refreshing package index", m.name));
                    match runner.stream(refresh, &mut io::sink()) {
                        Ok(()) => spin.ok(format!("{}: index refreshed", m.name)),
                        Err(e) => spin.fail(format!("{}: index refresh failed: {e}", m.name)),
                    }
                }
            }
            let spin = self.ui.spin(&format!("{}: scanning for updates", m.name));
            let plan = match m.check(runner.as_ref()) {
                Ok(plan) => plan,
                Err(e) => {
                    spin.fail(format!("{}: check failed: {e}", m.name));
                    self.note(Level::Fail, format!("{} check failed", m.name));
                    continue;
                }
            };
            spin.stop();
            self.print_plan(&m, &plan);
            if !plan.updates.is_empty() && !m.check_only() {
                pending += plan.updates.len();
                checked.push((m, plan));
            }
        }

        if pending == 0 {
            self.note(Level::Ok, "packages: system already up to date".to_string());
        } else if self.opts.dry_run {
            self.ui.blank();
            self.ui.step("dry-run, would execute:");
            for (m, _) in &checked {
                for c in &m.upgrade {
                    self.ui.line(&muted(format!("  $ {c}")));
                }
            }
            self.note(Level::Ok, format!("packages: {pending} upgrades pending (dry-run)"));
        } else if self.opts.check_only {
            self.note(Level::Ok, format!("packages: {pending} upgrades pending (check-only)"));
        } else {
            self.ui.blank();
            if self.stop_requested() {
                return;
            }
            if !self.ui.confirm(&format!("deploy {pending} upgrades now?"), true) || self.stop_requested() {
                self.note(Level::Ok, format!("packages: {pending} upgrades postponed by operator"));
            } else if !self.ensure_root() {
                self.note(Level::Fail, format!("packages: {pending} upgrades pending, root unavailable"));
            } else {
                self.upgrade(&checked);
            }
        }
        self.reboot_check();
    }

    fn print_plan(&self, m: &Manager, plan: &Plan) {
        let label = pad_right(&bold(m.name), 9);
        let (updates, deferred) = (plan.updates.len(), plan.deferred.len());
        if updates == 0 && deferred == 0 {
            self.ui.ok(format!("{label} {}", muted("up to date")));
        } else if updates == 0 {
            self.ui
                .ok(format!("{label} {}", muted(format!("up to date · {deferred} phased updates deferred by Ubuntu"))));
        } else {
            let kind = if m.check_only() {
                format!("available (manual: {})", m.manual_hint)
            } else {
                "upgradable".to_string()
            };
            self.ui.line(&format!("{}{label} {}", amber("◆ "), amber(format!("{updates} {kind}"))));
            for u in plan.updates.iter().take(MAX_LISTED_UPDATES) {
                let version = if u.from.is_empty() { u.to.clone() } else { format!("{} → {}", u.from, u.to) };
                self.ui.line(&format!("    {}{}", text(pad_right(&u.name, 34)), muted(version)));
            }
            if updates > MAX_LISTED_UPDATES {
                self.ui.line(&muted(format!("    … +{} more", updates - MAX_LISTED_UPDATES)));
            }
            if deferred > 0 {
                self.ui.info(format!("  {deferred} phased updates deferred by Ubuntu"));
            }
        }
        if !plan.removals.is_empty() {
            self.ui.warn(format!("{} full-upgrade would REMOVE: {}", m.name, plan.removals.join(", ")));
        }
    }

    /// Ctrl-C is caught for the whole run, so a package manager reached by the terminal's
    /// SIGINT still gets its output drained instead of dying on a closed pipe.
    fn upgrade(&mut self, checked: &[(Manager, Plan)]) {
        let runner = self.runner.clone();
        for (m, plan) in checked {
            if self.stop_requested() {
                return;
            }
            let removals = plan.removals.len();
            if removals > 0
                && !self.ui.confirm_strict(&format!("{} will remove {removals} packages, continue?", m.name))
            {
                self.note(Level::Warn, format!("{}: upgrade skipped (removals refused)", m.name));
                continue;
            }
            let count = plan.updates.len();
            self.ui.step(format!("{}: deploying {count} upgrades", m.name));
            let mut w = self.ui.stream_writer();
            let result = m.upgrade.iter().try_for_each(|c| runner.stream(c, &mut w));
            w.finish();
            match result {
                Ok(()) => {
                    self.ui.ok(format!("{}: {count} packages upgraded", m.name));
                    self.note(Level::Ok, format!("{}: {count} packages upgraded", m.name));
                }
                Err(e) => {
                    self.ui.fail(format!("{}: upgrade failed: {e}", m.name));
                    self.note(Level::Fail, format!("{}: upgrade failed", m.name));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::testkit::{FakeRoot, Harness, Setup, harness};
    use super::super::{Options, Outcome, Section};
    use crate::config::Config;
    use crate::execx::{Error, Fake, Output, cmd};
    use crate::system::Manager;
    use crate::ui;

    const APT_OUT: &str = "Inst libssl3t64 [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])\n\
                           Inst firefox [1.0] (2.0 Ubuntu:24.04/noble-updates [amd64])\n";

    fn apt_config() -> Config {
        let mut cfg = Config::default();
        cfg.system.managers = vec!["apt".to_string()];
        cfg
    }

    fn apt_fake(simulation: &str) -> Fake {
        Fake::new().binary("apt-get").on(cmd("apt-get", ["-s", "full-upgrade"]).with_env(["LC_ALL=C"]), simulation)
    }

    fn sys(opts: Options, fake: Fake) -> Setup {
        Setup { sections: vec![Section::Sys], opts, cfg: apt_config(), fake, ..Setup::default() }
    }

    fn live(fake: Fake, root: FakeRoot, assume_yes: bool) -> Harness {
        harness(Setup {
            root,
            ui: ui::Options { assume_yes, width: 110, ..ui::Options::default() },
            ..sys(Options::default(), fake)
        })
    }

    #[test]
    fn dry_run_lists_updates_and_never_touches_root() {
        let mut h = harness(sys(Options { dry_run: true, ..Options::default() }, apt_fake(APT_OUT)));
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        for want in [
            "2 upgradable",
            "firefox",
            "1.0 → 2.0",
            "index refresh skipped (dry-run)",
            "$ sudo LC_ALL=C DEBIAN_FRONTEND=noninteractive",
            "2 upgrades pending (dry-run)",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert!(h.runner.ran().iter().all(|c| !c.starts_with("sudo")), "{:?}", h.runner.ran());
        assert_eq!(h.root.calls(), 0);
    }

    fn snap_setup(opts: Options) -> Setup {
        let mut cfg = Config::default();
        cfg.system.managers = vec!["snap".to_string()];
        let snap_list = "Name Version Rev Size Publisher Notes\nfirefox 157.0-1 8995 275MB mozilla** -\n";
        let fake = Fake::new().binary("snap").on(cmd("snap", ["refresh", "--list"]).with_env(["LC_ALL=C"]), snap_list);
        Setup { cfg, ..sys(opts, fake) }
    }

    #[test]
    fn check_only_reports_without_upgrading() {
        let mut h = harness(snap_setup(Options { check_only: true, ..Options::default() }));
        h.routine.run();
        let plain = h.out.plain();
        assert!(plain.contains("1 upgrades pending (check-only)") && !plain.contains("root clearance"), "{plain}");
        assert_eq!(h.runner.ran().len(), 1);
    }

    #[test]
    fn non_interactive_run_postpones_without_yes() {
        let mut h = harness(snap_setup(Options::default()));
        h.routine.run();
        let plain = h.out.plain();
        assert!(
            plain.contains("non-interactive, pass --yes") && plain.contains("1 upgrades postponed by operator"),
            "{plain}"
        );
        assert_eq!(h.root.calls(), 0, "root must not be requested when the operator declines");
    }

    #[test]
    fn yes_refreshes_upgrades_and_streams_output() {
        let apt = Manager::apt();
        let fake = apt_fake(APT_OUT)
            .on(apt.refresh.clone().unwrap(), "Hit:1 http://archive.ubuntu.com noble InRelease\n")
            .on(apt.upgrade[0].clone(), "Setting up firefox (2.0) ...\n");
        let mut h = live(fake, FakeRoot::default(), true);
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        for want in [
            "apt: index refreshed",
            "apt: deploying 2 upgrades",
            "│ Setting up firefox (2.0) ...",
            "apt: 2 packages upgraded",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert_eq!(h.root.calls(), 1, "sudo is asked once per run");
    }

    #[test]
    fn denied_root_fails_without_upgrading() {
        let mut h = live(apt_fake(APT_OUT), FakeRoot::denying(), true);
        assert_eq!(h.routine.run(), Outcome::Failed);
        let plain = h.out.plain();
        assert!(plain.contains("root access denied: sudo: 3 incorrect password attempts"), "{plain}");
        assert!(plain.contains("2 upgrades pending, root unavailable"), "{plain}");
        assert!(!h.runner.ran().iter().any(|c| c.contains("full-upgrade") && c.starts_with("sudo")));
    }

    #[test]
    fn removals_need_a_human_even_with_yes() {
        let fake = apt_fake(&format!("{APT_OUT}Remv oldpkg [1.0]\n")).on(Manager::apt().refresh.unwrap(), "");
        let mut h = live(fake, FakeRoot::default(), true);
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        assert!(plain.contains("full-upgrade would REMOVE: oldpkg"), "{plain}");
        assert!(plain.contains("apt: upgrade skipped (removals refused)"), "{plain}");
    }

    #[test]
    fn failed_upgrade_is_reported() {
        let apt = Manager::apt();
        let upgrade = apt.upgrade[0].clone();
        let err = Error::Exit { command: upgrade.to_string(), exit_code: 100, stderr: "E: Could not get lock".into() };
        let fake = apt_fake(APT_OUT).on(apt.refresh.unwrap(), "").on_error(upgrade, Output::default(), err);
        let mut h = live(fake, FakeRoot::default(), true);
        assert_eq!(h.routine.run(), Outcome::Failed);
        assert!(h.out.plain().contains("apt: upgrade failed: sudo LC_ALL=C"), "{}", h.out.plain());
    }

    #[test]
    fn check_failures_fail_the_run_but_continue() {
        let mut cfg = Config::default();
        cfg.system.managers = vec!["snap".to_string(), "apt".to_string()];
        let fake = apt_fake("").binary("snap");
        let mut h = harness(Setup { cfg, ..sys(Options { dry_run: true, ..Options::default() }, fake) });
        assert_eq!(h.routine.run(), Outcome::Failed);
        let plain = h.out.plain();
        assert!(
            plain.contains("snap: check failed") && plain.contains("apt") && plain.contains("up to date"),
            "{plain}"
        );
    }

    #[test]
    fn pending_reboot_never_blocks_the_routine() {
        let mut cfg = apt_config();
        let tmp = tempfile::tempdir().unwrap();
        cfg.clean.tmp_dir = tmp.path().display().to_string();
        let mut h = harness(Setup {
            sections: vec![Section::Sys, Section::Clean],
            cfg,
            ..sys(Options { dry_run: true, ..Options::default() }, apt_fake(APT_OUT))
        });
        fs::write(h.run_dir.path().join("reboot-required"), "").unwrap();
        fs::write(h.run_dir.path().join("reboot-required.pkgs"), "linux-image-7.0.0-38-generic\n").unwrap();
        assert_eq!(h.routine.run(), Outcome::Completed, "a pending reboot must not fail the run");
        let plain = h.out.plain();
        for want in
            ["reboot pending (linux-image-7.0.0-38-generic), continuing without it", "SYS.PURGE", "MISSION.REPORT"]
        {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert!(plain.find("reboot pending").unwrap() < plain.find("SYS.PURGE").unwrap());
    }
}
