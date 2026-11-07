//! Estimates and reclaims disk space: docker leftovers, stale temp files, package caches, journals.

mod files;
mod sources;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use crate::execx::{Command, Runner, cmd};

pub use files::human_days;
pub use sources::{SnapRevision, parse_disabled_snaps, parse_docker_size, parse_docker_system_df};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Estimate {
    /// `None` when the size cannot be known before running the target.
    pub bytes: Option<u64>,
    pub detail: String,
}

type EstimateFn<'r> = Box<dyn Fn() -> Result<Estimate, BoxError> + Send + Sync + 'r>;
type RunFn<'r> = Box<dyn Fn(&mut (dyn Write + Send)) -> Result<(), BoxError> + Send + Sync + 'r>;

pub struct Target<'r> {
    pub name: String,
    pub root: bool,
    pub commands: Vec<String>,
    estimate: EstimateFn<'r>,
    run: RunFn<'r>,
}

impl Target<'_> {
    pub fn estimate(&self) -> Result<Estimate, BoxError> {
        (self.estimate)()
    }

    pub fn run(&self, w: &mut (dyn Write + Send)) -> Result<(), BoxError> {
        (self.run)(w)
    }
}

fn command_target<'r>(name: &str, r: &'r dyn Runner, estimate: EstimateFn<'r>, cmds: Vec<Command>) -> Target<'r> {
    Target {
        name: name.to_string(),
        root: cmds.iter().any(|c| c.root),
        commands: cmds.iter().map(Command::to_string).collect(),
        estimate,
        run: Box::new(move |w| {
            for c in &cmds {
                r.stream(c, w)?;
            }
            Ok(())
        }),
    }
}

fn unknown_estimate<'r>(detail: &str) -> EstimateFn<'r> {
    let detail = detail.to_string();
    Box::new(move || Ok(Estimate { bytes: None, detail: detail.clone() }))
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub home: PathBuf,
    pub tmp_dir: PathBuf,
    pub tmp_max_age_days: i64,
    pub tmp_exclude: Vec<String>,
    pub thumbnails_max_age_days: i64,
    pub trash_max_age_days: i64,
    pub journal_max_age: String,
    pub docker_images: bool,
    pub docker_build_cache: bool,
    pub docker_stopped_containers: bool,
    pub snap_dir: PathBuf,
    pub apt_archives: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub name: String,
    pub detail: String,
}

pub fn targets<'r>(r: &'r dyn Runner, opts: &Options) -> (Vec<Target<'r>>, Vec<Notice>) {
    let mut targets = Vec::new();
    let mut notices = Vec::new();
    let uid = files::current_uid();

    if r.available("docker") {
        let docker = Arc::new(sources::DockerProbe::new(r));
        let mut docker_target = |name: &str, kind: &'static str, c: Command| {
            targets.push(command_target(name, r, sources::docker_reclaimable(docker.clone(), kind), vec![c]));
        };
        if opts.docker_images {
            docker_target("docker unused images", "Images", cmd("docker", ["image", "prune", "--all", "--force"]));
        }
        if opts.docker_build_cache {
            docker_target("docker build cache", "Build Cache", cmd("docker", ["builder", "prune", "--all", "--force"]));
        }
        if opts.docker_stopped_containers {
            docker_target("docker stopped containers", "Containers", cmd("docker", ["container", "prune", "--force"]));
        }
        notices.push(Notice {
            name: "docker volumes".to_string(),
            detail: "never pruned automatically (may hold databases) — review with `docker volume ls -f dangling=true`"
                .to_string(),
        });
    }

    targets.push(files::stale_files_target(files::StaleFiles {
        name: format!("temp files ({})", opts.tmp_dir.display()),
        dir: opts.tmp_dir.clone(),
        max_age: files::days(opts.tmp_max_age_days),
        exclude: opts.tmp_exclude.clone(),
        owned_by: Some(uid),
        ..files::StaleFiles::default()
    }));

    if r.available("systemd-tmpfiles") {
        targets.push(command_target(
            "system tmpfiles policy",
            r,
            unknown_estimate("ages defined by tmpfiles.d"),
            vec![cmd("systemd-tmpfiles", ["--clean"]).as_root()],
        ));
    }

    if r.available("apt-get") {
        targets.push(command_target(
            "apt orphans + cache",
            r,
            sources::apt_estimate(r, opts.apt_archives.clone()),
            vec![
                cmd("apt-get", ["autoremove", "--purge", "-y"])
                    .with_env(["LC_ALL=C", "DEBIAN_FRONTEND=noninteractive"])
                    .as_root(),
                cmd("apt-get", ["clean"]).as_root(),
            ],
        ));
    }

    if r.available("journalctl") && !opts.journal_max_age.is_empty() {
        targets.push(command_target(
            &format!("journal (> {})", opts.journal_max_age),
            r,
            sources::journal_estimate(r),
            vec![cmd("journalctl", [format!("--vacuum-time={}", opts.journal_max_age)]).as_root()],
        ));
    }

    if r.available("snap") {
        targets.push(sources::snap_revisions_target(r, opts.snap_dir.clone()));
    }

    if r.available("flatpak") {
        targets.push(command_target(
            "flatpak unused runtimes",
            r,
            unknown_estimate(""),
            vec![cmd("flatpak", ["uninstall", "--unused", "-y", "--noninteractive"])],
        ));
    }

    // A relative home (unset $HOME) would resolve against the working directory.
    if opts.home.is_absolute() {
        targets.push(files::stale_files_target(files::StaleFiles {
            name: "thumbnail cache".to_string(),
            dir: opts.home.join(".cache/thumbnails"),
            max_age: files::days(opts.thumbnails_max_age_days),
            owned_by: Some(uid),
            per_file: true,
            ..files::StaleFiles::default()
        }));
        targets.push(files::trash_target(opts.home.join(".local/share/Trash"), files::days(opts.trash_max_age_days)));
    }
    (targets, notices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execx::{Error, Fake, Output};

    #[test]
    fn command_target_runs_in_order_and_stops_on_error() {
        let first = cmd("apt-get", ["autoremove"]).as_root();
        let second = cmd("apt-get", ["clean"]).as_root();
        let boom = Error::Spawn { command: first.to_string(), message: "boom".to_string() };
        let f = Fake::new().on_error(first.clone(), Output::default(), boom).on(second.clone(), "");
        let target = command_target("apt", &f, unknown_estimate(""), vec![first, second]);
        assert!(target.root);
        assert_eq!(target.commands.len(), 2);
        assert!(target.commands[0].starts_with("sudo "));
        assert!(target.run(&mut Vec::new()).is_err());
        assert_eq!(f.ran().len(), 1, "the second command must not run after a failure");
    }

    #[test]
    fn targets_respect_options_and_availability() {
        let home = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let f = Fake::new().binary("docker");
        let opts =
            Options { home: home.path().into(), tmp_dir: tmp.path().into(), docker_images: true, ..Options::default() };
        let (targets, notices) = targets(&f, &opts);
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"docker unused images"), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("build cache") || n.contains("apt")), "{names:?}");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].name, "docker volumes");
    }

    #[test]
    fn home_targets_need_an_absolute_home() {
        let names = |home: &str| -> Vec<String> {
            let opts = Options { home: home.into(), tmp_dir: "/nonexistent".into(), ..Options::default() };
            targets(&Fake::new(), &opts).0.into_iter().map(|t| t.name).collect()
        };
        assert_eq!(names(""), ["temp files (/nonexistent)"]);
        assert_eq!(names("relative/home"), ["temp files (/nonexistent)"]);
        assert_eq!(names("/home/op"), ["temp files (/nonexistent)", "thumbnail cache", "trash"]);
    }

    #[test]
    fn docker_targets_share_one_probe() {
        let df = cmd("docker", ["system", "df", "--format", "{{json .}}"]);
        let f = Fake::new()
            .binary("docker")
            .on(df, "{\"Reclaimable\":\"2.5GB (40%)\",\"Type\":\"Images\"}\n{\"Reclaimable\":\"500MB\",\"Type\":\"Build Cache\"}\n");
        let home = tempfile::tempdir().unwrap();
        let opts =
            Options { home: home.path().into(), docker_images: true, docker_build_cache: true, ..Options::default() };
        let (targets, _) = targets(&f, &opts);
        let sizes: Vec<Option<u64>> = targets[..2].iter().map(|t| t.estimate().unwrap().bytes).collect();
        assert_eq!(sizes, [Some(2_500_000_000), Some(500_000_000)]);
        assert_eq!(f.ran().len(), 1);
    }
}
