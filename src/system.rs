//! Detects the desktop package managers, lists pending updates and applies them.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

use crate::execx::{self, Command, Runner, cmd};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub name: String,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub updates: Vec<Update>,
    pub removals: Vec<String>,
    pub deferred: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Apt,
    Snap,
    Flatpak,
    Fwupd,
}

#[derive(Clone, Debug)]
pub struct Manager {
    kind: Kind,
    pub name: &'static str,
    pub binary: &'static str,
    pub refresh: Option<Command>,
    pub upgrade: Vec<Command>,
    pub manual_hint: &'static str,
}

#[derive(Debug)]
pub enum CheckError {
    Exec(execx::Error),
    Parse(serde_json::Error),
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckError::Exec(e) => e.fmt(f),
            CheckError::Parse(e) => write!(f, "unreadable report: {e}"),
        }
    }
}

impl std::error::Error for CheckError {}

const C_LOCALE: &str = "LC_ALL=C";
const FWUPD_NOTHING_TO_DO: i32 = 2;

impl Manager {
    pub fn apt() -> Manager {
        Manager {
            kind: Kind::Apt,
            name: "apt",
            binary: "apt-get",
            refresh: Some(cmd("apt-get", ["update", "-q"]).with_env([C_LOCALE]).as_root()),
            upgrade: vec![
                cmd(
                    "apt-get",
                    [
                        "-y",
                        "-o",
                        "Dpkg::Options::=--force-confdef",
                        "-o",
                        "Dpkg::Options::=--force-confold",
                        "full-upgrade",
                    ],
                )
                .with_env([C_LOCALE, "DEBIAN_FRONTEND=noninteractive", "NEEDRESTART_MODE=l"])
                .as_root(),
            ],
            manual_hint: "",
        }
    }

    pub fn snap() -> Manager {
        Manager {
            kind: Kind::Snap,
            name: "snap",
            binary: "snap",
            refresh: None,
            upgrade: vec![cmd("snap", ["refresh"]).as_root()],
            manual_hint: "",
        }
    }

    pub fn flatpak() -> Manager {
        Manager {
            kind: Kind::Flatpak,
            name: "flatpak",
            binary: "flatpak",
            refresh: None,
            upgrade: vec![cmd("flatpak", ["update", "-y", "--noninteractive"])],
            manual_hint: "",
        }
    }

    pub fn fwupd() -> Manager {
        Manager {
            kind: Kind::Fwupd,
            name: "fwupd",
            binary: "fwupdmgr",
            refresh: None,
            upgrade: Vec::new(),
            manual_hint: "fwupdmgr update",
        }
    }

    pub fn check_only(&self) -> bool {
        self.upgrade.is_empty()
    }

    pub fn check(&self, r: &dyn Runner) -> Result<Plan, CheckError> {
        let stdout = |c: Command| r.output(&c.with_env([C_LOCALE])).map(|o| o.stdout).map_err(CheckError::Exec);
        match self.kind {
            Kind::Apt => Ok(parse_apt_simulation(&stdout(cmd("apt-get", ["-s", "full-upgrade"]))?)),
            Kind::Snap => Ok(Plan {
                updates: parse_snap_refresh_list(&stdout(cmd("snap", ["refresh", "--list"]))?),
                ..Plan::default()
            }),
            Kind::Flatpak => {
                let out = stdout(cmd("flatpak", ["remote-ls", "--updates", "--columns=application,version"]))?;
                Ok(Plan { updates: parse_flatpak_updates(&out), ..Plan::default() })
            }
            Kind::Fwupd => match stdout(cmd("fwupdmgr", ["get-updates", "--json"])) {
                Err(CheckError::Exec(e)) if e.exit_code() == Some(FWUPD_NOTHING_TO_DO) => Ok(Plan::default()),
                Err(e) => Err(e),
                Ok(out) => {
                    Ok(Plan { updates: parse_fwupd_updates(&out).map_err(CheckError::Parse)?, ..Plan::default() })
                }
            },
        }
    }
}

/// Keeps the configured order and skips managers whose binary is missing.
pub fn detect(r: &dyn Runner, enabled: &[String]) -> Vec<Manager> {
    enabled
        .iter()
        .filter_map(|name| match name.as_str() {
            "apt" => Some(Manager::apt()),
            "snap" => Some(Manager::snap()),
            "flatpak" => Some(Manager::flatpak()),
            "fwupd" => Some(Manager::fwupd()),
            _ => None,
        })
        .filter(|m| r.available(m.binary))
        .collect()
}

static APT_INST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Inst (\S+)(?: \[([^\]]+)\])? \((\S+)").expect("valid regex"));
static APT_REMV: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Remv (\S+)").expect("valid regex"));

pub fn parse_apt_simulation(out: &str) -> Plan {
    let mut plan = Plan::default();
    let mut in_deferred = false;
    for line in out.lines() {
        if in_deferred {
            if line.starts_with("  ") {
                plan.deferred.extend(line.split_whitespace().map(String::from));
                continue;
            }
            in_deferred = false;
        }
        if line.contains("deferred due to phasing") {
            in_deferred = true;
        } else if let Some(m) = APT_INST.captures(line) {
            let group = |i| m.get(i).map_or(String::new(), |g| g.as_str().to_string());
            plan.updates.push(Update { name: group(1), from: group(2), to: group(3) });
        } else if let Some(m) = APT_REMV.captures(line) {
            plan.removals.push(m[1].to_string());
        }
    }
    plan
}

pub fn parse_snap_refresh_list(out: &str) -> Vec<Update> {
    out.split('\n')
        .enumerate()
        .filter_map(|(i, line)| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 2 || (i == 0 && fields[0] == "Name") {
                return None;
            }
            Some(Update { name: fields[0].to_string(), to: fields[1].to_string(), ..Update::default() })
        })
        .collect()
}

pub fn parse_flatpak_updates(out: &str) -> Vec<Update> {
    out.split('\n')
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next().filter(|f| f.contains('.'))?;
            Some(Update {
                name: name.to_string(),
                to: fields.next().unwrap_or_default().to_string(),
                ..Update::default()
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct FwupdReport {
    #[serde(rename = "Devices", default)]
    devices: Vec<FwupdDevice>,
}

#[derive(Deserialize)]
struct FwupdDevice {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Version", default)]
    version: String,
    #[serde(rename = "Releases", default)]
    releases: Vec<FwupdRelease>,
}

#[derive(Deserialize)]
struct FwupdRelease {
    #[serde(rename = "Version", default)]
    version: String,
}

pub fn parse_fwupd_updates(data: &str) -> Result<Vec<Update>, serde_json::Error> {
    let report: FwupdReport = serde_json::from_str(data)?;
    Ok(report
        .devices
        .into_iter()
        .filter_map(|d| {
            let latest = d.releases.into_iter().next()?;
            Some(Update { name: d.name, from: d.version, to: latest.version })
        })
        .collect())
}

/// `None` when no reboot is pending; otherwise the packages asking for it, deduplicated in file order.
pub fn reboot_required(run_dir: &Path) -> Option<Vec<String>> {
    if !run_dir.join("reboot-required").exists() {
        return None;
    }
    let listed = fs::read_to_string(run_dir.join("reboot-required.pkgs")).unwrap_or_default();
    let mut seen = HashSet::new();
    Some(listed.split_whitespace().filter(|p| seen.insert(*p)).map(String::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execx::{Error, Fake, Output};

    const APT_SIMULATION: &str = "NOTE: This is only a simulation!
Reading package lists...
The following upgrades have been deferred due to phasing:
  dnsmasq-base libegl-mesa0
  thermald
The following packages will be upgraded:
  firefox libssl3t64
Inst libssl3t64 [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])
Inst linux-image-6.8.0-60-generic (6.8.0-60.63 Ubuntu:24.04/noble-updates [amd64])
Remv oldpkg [1.0]
Conf libssl3t64 (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])
2 upgraded, 1 newly installed, 1 to remove and 3 not upgraded.
";

    fn update(name: &str, from: &str, to: &str) -> Update {
        Update { name: name.into(), from: from.into(), to: to.into() }
    }

    #[test]
    fn apt_simulation_lists_updates_removals_and_phased_packages() {
        let plan = parse_apt_simulation(APT_SIMULATION);
        assert_eq!(
            plan.updates,
            [
                update("libssl3t64", "3.0.13-0ubuntu3.4", "3.0.13-0ubuntu3.5"),
                update("linux-image-6.8.0-60-generic", "", "6.8.0-60.63")
            ]
        );
        assert_eq!(plan.removals, ["oldpkg"]);
        assert_eq!(plan.deferred, ["dnsmasq-base", "libegl-mesa0", "thermald"]);
    }

    #[test]
    fn apt_simulation_up_to_date() {
        assert_eq!(
            parse_apt_simulation("0 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.\n"),
            Plan::default()
        );
    }

    #[test]
    fn snap_refresh_list() {
        let out = "Name      Version        Rev   Size   Publisher    Notes\n\
                   chromium  154.0.8037.57  3548  191MB  canonical**  -\n\
                   firefox   157.0-1        8995  275MB  mozilla**    -\n";
        assert_eq!(
            parse_snap_refresh_list(out),
            [update("chromium", "", "154.0.8037.57"), update("firefox", "", "157.0-1")]
        );
        assert!(parse_snap_refresh_list("").is_empty());
    }

    #[test]
    fn flatpak_updates() {
        let out = "Application ID\tVersion\norg.mozilla.firefox\t131.0\norg.gnome.Platform\t\n\n";
        assert_eq!(
            parse_flatpak_updates(out),
            [update("org.mozilla.firefox", "", "131.0"), update("org.gnome.Platform", "", "")]
        );
    }

    #[test]
    fn fwupd_updates_take_the_newest_release() {
        let data = r#"{"Devices":[
            {"Name":"System Firmware","Version":"0.1.11","Releases":[{"Version":"0.1.43"},{"Version":"0.1.39"}]},
            {"Name":"No releases","Version":"1"}
        ]}"#;
        assert_eq!(parse_fwupd_updates(data).unwrap(), [update("System Firmware", "0.1.11", "0.1.43")]);
        assert!(parse_fwupd_updates("not json").is_err());
    }

    #[test]
    fn fwupd_nothing_to_do_is_not_an_error() {
        let c = cmd("fwupdmgr", ["get-updates", "--json"]).with_env([C_LOCALE]);
        let err = Error::Exit { command: c.to_string(), exit_code: 2, stderr: String::new() };
        let f = Fake::new().on_error(c, Output { exit_code: 2, ..Output::default() }, err);
        assert_eq!(Manager::fwupd().check(&f).unwrap(), Plan::default());
    }

    #[test]
    fn fwupd_report_errors_surface() {
        let f = Fake::new().on(cmd("fwupdmgr", ["get-updates", "--json"]).with_env([C_LOCALE]), "{oops");
        assert!(matches!(Manager::fwupd().check(&f), Err(CheckError::Parse(_))));
    }

    #[test]
    fn apt_check_propagates_errors() {
        assert!(matches!(Manager::apt().check(&Fake::new()), Err(CheckError::Exec(_))));
    }

    #[test]
    fn detect_keeps_order_and_skips_missing() {
        let f = Fake::new().binary("snap").binary("apt-get");
        let enabled: Vec<String> = ["snap", "flatpak", "apt", "unknown"].map(String::from).to_vec();
        let names: Vec<&str> = detect(&f, &enabled).iter().map(|m| m.name).collect();
        assert_eq!(names, ["snap", "apt"]);
    }

    #[test]
    fn only_fwupd_is_check_only() {
        assert!(Manager::fwupd().check_only());
        assert!(!Manager::apt().check_only() && !Manager::snap().check_only() && !Manager::flatpak().check_only());
    }

    #[test]
    fn apt_upgrade_never_prompts() {
        let upgrade = Manager::apt().upgrade[0].to_string();
        for needle in ["DEBIAN_FRONTEND=noninteractive", "NEEDRESTART_MODE=l", "--force-confold", "--force-confdef"] {
            assert!(upgrade.contains(needle), "{upgrade}");
        }
    }

    #[test]
    fn reboot_marker_and_package_list() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(reboot_required(dir.path()), None);
        fs::write(dir.path().join("reboot-required"), "").unwrap();
        assert_eq!(reboot_required(dir.path()), Some(vec![]));
        fs::write(dir.path().join("reboot-required.pkgs"), "linux-base\nlinux-base\nlibc6\n").unwrap();
        assert_eq!(reboot_required(dir.path()), Some(vec!["linux-base".to_string(), "libc6".to_string()]));
    }
}
