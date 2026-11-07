use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, OnceLock};

use regex::Regex;
use serde::Deserialize;

use super::{BoxError, Estimate, EstimateFn, Target};
use crate::execx::{Runner, cmd};

/// Runs `docker system df` once, whichever docker target is estimated first.
pub(super) struct DockerProbe<'r> {
    runner: &'r dyn Runner,
    rows: OnceLock<Result<HashMap<String, String>, String>>,
}

impl<'r> DockerProbe<'r> {
    pub(super) fn new(runner: &'r dyn Runner) -> Self {
        DockerProbe { runner, rows: OnceLock::new() }
    }

    fn rows(&self) -> &Result<HashMap<String, String>, String> {
        self.rows.get_or_init(|| {
            let out = self
                .runner
                .output(&cmd("docker", ["system", "df", "--format", "{{json .}}"]))
                .map_err(|e| e.to_string())?;
            parse_docker_system_df(&out.stdout).map_err(|e| e.to_string())
        })
    }
}

pub(super) fn docker_reclaimable<'r>(probe: Arc<DockerProbe<'r>>, kind: &'static str) -> EstimateFn<'r> {
    Box::new(move || {
        let rows = probe.rows().as_ref().map_err(|e| BoxError::from(e.clone()))?;
        let Some(raw) = rows.get(kind) else { return Ok(Estimate { bytes: Some(0), detail: String::new() }) };
        let size = parse_docker_size(raw.split_whitespace().next().unwrap_or_default())?;
        Ok(Estimate { bytes: Some(size), detail: raw.clone() })
    })
}

#[derive(Deserialize)]
struct DockerDfRow {
    #[serde(rename = "Type", default)]
    kind: String,
    #[serde(rename = "Reclaimable", default)]
    reclaimable: String,
}

pub fn parse_docker_system_df(out: &str) -> Result<HashMap<String, String>, BoxError> {
    let mut rows = HashMap::new();
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let row: DockerDfRow = serde_json::from_str(line).map_err(|e| format!("docker system df: {e}"))?;
        rows.insert(row.kind, row.reclaimable);
    }
    Ok(rows)
}

static DOCKER_SIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([0-9]+(?:\.[0-9]+)?)\s*([kKMGTP]?B)$").expect("valid regex"));

/// Reads docker's decimal (SI) human sizes such as "23.12GB" or "913.3MB".
pub fn parse_docker_size(s: &str) -> Result<u64, BoxError> {
    let m = DOCKER_SIZE.captures(s.trim()).ok_or_else(|| format!("unrecognised docker size {s:?}"))?;
    let value: f64 = m[1].parse()?;
    let multiplier = match &m[2] {
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "PB" => 1e15,
        _ => 1.0,
    };
    Ok((value * multiplier) as u64)
}

pub(super) fn apt_estimate(r: &dyn Runner, archives: PathBuf) -> EstimateFn<'_> {
    Box::new(move || {
        let debs: Vec<u64> = fs::read_dir(&archives)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "deb"))
                    .filter_map(|e| e.metadata().ok().map(|m| m.len()))
                    .collect()
            })
            .unwrap_or_default();
        let out = r.output(&cmd("apt-get", ["-s", "autoremove"]).with_env(["LC_ALL=C"]))?;
        let orphans = out.stdout.lines().filter(|l| l.starts_with("Remv ")).count();
        Ok(Estimate {
            bytes: Some(debs.iter().sum()),
            detail: format!("{} cached .deb, {orphans} orphan packages", debs.len()),
        })
    })
}

static JOURNAL_USAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"take up ([0-9.]+[KMGT]?B?)").expect("valid regex"));

pub(super) fn journal_estimate(r: &dyn Runner) -> EstimateFn<'_> {
    Box::new(move || {
        let out = r.output(&cmd("journalctl", ["--disk-usage"]).with_env(["LC_ALL=C"]))?;
        let detail = JOURNAL_USAGE
            .captures(&out.stdout)
            .map_or_else(|| "journal size unknown".to_string(), |m| format!("journal uses {} in total", &m[1]));
        Ok(Estimate { bytes: None, detail })
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapRevision {
    pub name: String,
    pub revision: String,
}

pub fn parse_disabled_snaps(out: &str) -> Vec<SnapRevision> {
    out.split('\n')
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let notes = fields.last()?;
            (fields.len() >= 4 && notes.contains("disabled"))
                .then(|| SnapRevision { name: fields[0].to_string(), revision: fields[2].to_string() })
        })
        .collect()
}

fn disabled_snaps(r: &dyn Runner) -> Result<Vec<SnapRevision>, BoxError> {
    Ok(parse_disabled_snaps(&r.output(&cmd("snap", ["list", "--all"]).with_env(["LC_ALL=C"]))?.stdout))
}

pub(super) fn snap_revisions_target(r: &dyn Runner, snap_dir: PathBuf) -> Target<'_> {
    Target {
        name: "snap disabled revisions".to_string(),
        root: true,
        commands: vec!["sudo snap remove <name> --revision=<rev>  (for each disabled revision)".to_string()],
        estimate: Box::new(move || {
            let revisions = disabled_snaps(r)?;
            let total = revisions
                .iter()
                .filter_map(|rev| fs::metadata(snap_dir.join(format!("{}_{}.snap", rev.name, rev.revision))).ok())
                .map(|m| m.len())
                .sum();
            Ok(Estimate { bytes: Some(total), detail: format!("{} disabled revisions", revisions.len()) })
        }),
        run: Box::new(move |w| {
            for rev in disabled_snaps(r)? {
                r.stream(
                    &cmd("snap", ["remove", rev.name.as_str(), &format!("--revision={}", rev.revision)]).as_root(),
                    w,
                )?;
            }
            Ok(())
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execx::Fake;

    #[test]
    fn docker_sizes() {
        for (input, want) in [
            ("23.12GB", 23_120_000_000),
            ("913.3MB", 913_300_000),
            ("0B", 0),
            ("12kB", 12_000),
            ("1TB", 1_000_000_000_000),
        ] {
            assert_eq!(parse_docker_size(input).unwrap(), want, "{input}");
        }
        for bad in ["", "GB", "12XB", "-1GB"] {
            assert!(parse_docker_size(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn docker_system_df_rows() {
        let out = "{\"Active\":\"36\",\"Reclaimable\":\"23.12GB (69%)\",\"Size\":\"33.08GB\",\"TotalCount\":\"86\",\"Type\":\"Images\"}\n\
                   {\"Active\":\"0\",\"Reclaimable\":\"1.786GB\",\"Size\":\"1.786GB\",\"TotalCount\":\"385\",\"Type\":\"Build Cache\"}\n";
        let rows = parse_docker_system_df(out).unwrap();
        assert_eq!(rows["Images"], "23.12GB (69%)");
        assert_eq!(rows["Build Cache"], "1.786GB");
        assert!(parse_docker_system_df("{broken").is_err());
    }

    #[test]
    fn docker_estimates_reuse_a_single_probe() {
        let f = Fake::new().on(
            cmd("docker", ["system", "df", "--format", "{{json .}}"]),
            "{\"Reclaimable\":\"23.12GB (69%)\",\"Type\":\"Images\"}\n",
        );
        let probe = Arc::new(DockerProbe::new(&f));
        assert_eq!(docker_reclaimable(probe.clone(), "Images")().unwrap().bytes, Some(23_120_000_000));
        assert_eq!(docker_reclaimable(probe, "Build Cache")().unwrap().bytes, Some(0));
        assert_eq!(f.ran().len(), 1);
    }

    #[test]
    fn docker_probe_errors_reach_every_target() {
        let f = Fake::new();
        let probe = Arc::new(DockerProbe::new(&f));
        assert!(docker_reclaimable(probe.clone(), "Images")().is_err());
        assert!(docker_reclaimable(probe, "Build Cache")().is_err());
        assert_eq!(f.ran().len(), 1);
    }

    #[test]
    fn disabled_snaps() {
        let out = "Name      Version   Rev   Tracking       Publisher   Notes\n\
                   core22    20240111  1122  latest/stable  canonical** base,disabled\n\
                   core22    20240408  1380  latest/stable  canonical** base\n\
                   firefox   124.0     4033  latest/stable  mozilla**   disabled\n";
        let want = [
            SnapRevision { name: "core22".into(), revision: "1122".into() },
            SnapRevision { name: "firefox".into(), revision: "4033".into() },
        ];
        assert_eq!(parse_disabled_snaps(out), want);
    }

    #[test]
    fn snap_target_sizes_and_removes_each_disabled_revision() {
        let snaps = tempfile::tempdir().unwrap();
        fs::write(snaps.path().join("core22_1122.snap"), vec![0u8; 700]).unwrap();
        let list = "Name Version Rev Tracking Publisher Notes\ncore22 1 1122 latest/stable canonical** base,disabled\n";
        let f = Fake::new()
            .on(cmd("snap", ["list", "--all"]).with_env(["LC_ALL=C"]), list)
            .on(cmd("snap", ["remove", "core22", "--revision=1122"]).as_root(), "core22 (revision 1122) removed\n");
        let target = snap_revisions_target(&f, snaps.path().into());
        assert_eq!(target.estimate().unwrap(), Estimate { bytes: Some(700), detail: "1 disabled revisions".into() });
        let mut out = Vec::new();
        target.run(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "core22 (revision 1122) removed\n");
    }

    #[test]
    fn apt_estimate_counts_cached_debs_and_orphans() {
        let archives = tempfile::tempdir().unwrap();
        fs::write(archives.path().join("a.deb"), vec![0u8; 100]).unwrap();
        fs::write(archives.path().join("b.deb"), vec![0u8; 50]).unwrap();
        fs::write(archives.path().join("lock"), "").unwrap();
        let f = Fake::new()
            .on(cmd("apt-get", ["-s", "autoremove"]).with_env(["LC_ALL=C"]), "Remv old1 [1]\nRemv old2 [2]\n");
        let est = apt_estimate(&f, archives.path().into())().unwrap();
        assert_eq!(est, Estimate { bytes: Some(150), detail: "2 cached .deb, 2 orphan packages".into() });
    }

    #[test]
    fn journal_estimate_reports_usage() {
        let f = Fake::new().on(
            cmd("journalctl", ["--disk-usage"]).with_env(["LC_ALL=C"]),
            "Archived and active journals take up 1.2G in the file system.\n",
        );
        assert_eq!(
            journal_estimate(&f)().unwrap(),
            Estimate { bytes: None, detail: "journal uses 1.2G in total".into() }
        );
    }
}
