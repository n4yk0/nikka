//! Loads ~/.config/nikka/config.toml over built-in defaults; secrets may come from the environment.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::news::{self, Category};
use crate::work;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub system: System,
    pub clean: Clean,
    pub news: News,
    pub skills: Skills,
    pub jobs: Jobs,
    pub work: Work,
    pub france_travail: FranceTravail,
    pub adzuna: Adzuna,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub animations: bool,
    pub hyperlinks: bool,
}

impl Default for General {
    fn default() -> Self {
        General { animations: true, hyperlinks: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct System {
    pub managers: Vec<String>,
}

impl Default for System {
    fn default() -> Self {
        System { managers: strings(&["apt", "snap", "flatpak", "fwupd"]) }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clean {
    pub tmp_dir: String,
    pub tmp_max_age_days: i64,
    pub tmp_exclude: Vec<String>,
    pub thumbnails_max_age_days: i64,
    pub trash_max_age_days: i64,
    pub journal_max_age: String,
    pub docker_images: bool,
    pub docker_build_cache: bool,
    pub docker_stopped_containers: bool,
}

impl Default for Clean {
    fn default() -> Self {
        Clean {
            tmp_dir: "/tmp".to_string(),
            tmp_max_age_days: 7,
            tmp_exclude: strings(&[
                ".X",
                ".ICE-unix",
                ".XIM-unix",
                ".font-unix",
                ".Test-unix",
                "systemd-private-",
                "snap-private-tmp",
                "tmux-",
                "ssh-",
                "pulse-",
                "dbus-",
                "vscode-",
                "jetbrains",
                "go-build",
            ]),
            thumbnails_max_age_days: 30,
            trash_max_age_days: 30,
            journal_max_age: "2weeks".to_string(),
            docker_images: true,
            docker_build_cache: true,
            docker_stopped_containers: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct News {
    pub per_category: usize,
    pub kev_count: usize,
    pub categories: Vec<Category>,
}

impl Default for News {
    fn default() -> Self {
        News { per_category: 5, kev_count: 3, categories: news::default_categories() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Skills {
    pub top: usize,
}

impl Default for Skills {
    fn default() -> Self {
        Skills { top: 5 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Jobs {
    pub top: usize,
    pub origin_name: String,
    pub origin_postal_code: String,
    pub origin_region: String,
    pub origin_insee: String,
    pub origin_lat: f64,
    pub origin_lon: f64,
    pub max_drive_minutes: f64,
    pub search_radius_km: u32,
    pub keywords: Vec<String>,
    #[serde(rename = "france_travail_domain")]
    pub ft_domain: String,
    pub profile_skills: Vec<String>,
    pub routing_url: String,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs {
            top: 5,
            origin_name: "Bordeaux".to_string(),
            origin_postal_code: "33000".to_string(),
            origin_region: "Nouvelle-Aquitaine".to_string(),
            origin_insee: "33063".to_string(),
            origin_lat: 44.8624,
            origin_lon: -0.5848,
            max_drive_minutes: 45.0,
            search_radius_km: 60,
            keywords: strings(&[
                "développeur",
                "developpeur",
                "ingénieur logiciel",
                "software engineer",
                "fullstack",
                "devops",
                "product engineer",
            ]),
            ft_domain: "M18".to_string(),
            profile_skills: Vec::new(),
            routing_url: "https://router.project-osrm.org".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Work {
    /// `auto` or one of `work::Terminal` names.
    pub terminal: String,
    pub session: String,
}

impl Default for Work {
    fn default() -> Self {
        Work { terminal: work::AUTO.to_string(), session: "work".to_string() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FranceTravail {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Adzuna {
    pub app_id: String,
    pub app_key: String,
}

#[derive(Debug)]
pub enum Error {
    Io(PathBuf, io::Error),
    Parse(PathBuf, Box<toml::de::Error>),
    Invalid(Vec<String>),
    AlreadyExists(PathBuf),
    Encode(toml::ser::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Error::Parse(path, e) => write!(f, "{}: {e}", path.display()),
            Error::Invalid(problems) => f.write_str(&problems.join("\n")),
            Error::AlreadyExists(path) => write!(f, "{} already exists", path.display()),
            Error::Encode(e) => write!(f, "encode default config: {e}"),
        }
    }
}

impl std::error::Error for Error {}

const SECRET_VARIABLES: [&str; 4] =
    ["FRANCE_TRAVAIL_CLIENT_ID", "FRANCE_TRAVAIL_CLIENT_SECRET", "ADZUNA_APP_ID", "ADZUNA_APP_KEY"];

impl Config {
    /// Returns defaults when the file is missing; a present but invalid file is an error.
    pub fn load(path: &Path) -> Result<(Config, bool), Error> {
        Self::load_with_env(path, |key| std::env::var(key).ok())
    }

    pub fn load_with_env(path: &Path, env: impl Fn(&str) -> Option<String>) -> Result<(Config, bool), Error> {
        let data = match fs::read_to_string(path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let mut cfg = Config::default();
                cfg.apply_env(env);
                return Ok((cfg, false));
            }
            Err(e) => return Err(Error::Io(path.to_path_buf(), e)),
        };
        let mut cfg: Config = toml::from_str(&data).map_err(|e| Error::Parse(path.to_path_buf(), Box::new(e)))?;
        if cfg.news.categories.is_empty() {
            cfg.news.categories = news::default_categories();
        }
        cfg.apply_env(env);
        cfg.validate()?;
        Ok((cfg, true))
    }

    fn apply_env(&mut self, env: impl Fn(&str) -> Option<String>) {
        let [ft_id, ft_secret, adzuna_id, adzuna_key] = SECRET_VARIABLES;
        let targets = [
            (ft_id, &mut self.france_travail.client_id),
            (ft_secret, &mut self.france_travail.client_secret),
            (adzuna_id, &mut self.adzuna.app_id),
            (adzuna_key, &mut self.adzuna.app_key),
        ];
        for (key, dst) in targets {
            if let Some(value) = env(key).filter(|v| !v.is_empty()) {
                *dst = value;
            }
        }
    }

    fn validate(&self) -> Result<(), Error> {
        let mut problems = Vec::new();
        if self.jobs.max_drive_minutes <= 0.0 {
            problems.push("jobs.max_drive_minutes must be > 0".to_string());
        }
        if !(-90.0..=90.0).contains(&self.jobs.origin_lat) || !(-180.0..=180.0).contains(&self.jobs.origin_lon) {
            problems.push("jobs.origin_lat/origin_lon out of range".to_string());
        }
        if !work::choices().contains(&self.work.terminal.as_str()) {
            problems.push(format!("work.terminal must be one of: {}", work::choices().join(", ")));
        }
        if self.work.session.is_empty() || self.work.session.contains([':', '.']) {
            problems.push("work.session must be a non-empty tmux session name without ':' or '.'".to_string());
        }
        for cat in &self.news.categories {
            if cat.key.is_empty() || cat.sources.is_empty() {
                problems.push(format!("news category {:?} needs a key and at least one source", cat.title));
            }
        }
        if problems.is_empty() { Ok(()) } else { Err(Error::Invalid(problems)) }
    }

    pub fn has_france_travail(&self) -> bool {
        !self.france_travail.client_id.is_empty() && !self.france_travail.client_secret.is_empty()
    }

    pub fn has_adzuna(&self) -> bool {
        !self.adzuna.app_id.is_empty() && !self.adzuna.app_key.is_empty()
    }
}

/// $XDG_CONFIG_HOME when absolute, else $HOME/.config.
pub fn default_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").unwrap_or_else(|| PathBuf::from(".config")).join("nikka").join("config.toml")
}

/// $XDG_CACHE_HOME when absolute, else $HOME/.cache; `None` (no home) disables the HTTP cache.
pub fn cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache").map(|dir| dir.join("nikka"))
}

fn xdg_dir(variable: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var_os(variable).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Some(dir),
        _ => std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|home| PathBuf::from(home).join(fallback)),
    }
}

const HEADER: &str = "# nikka configuration.
# Secrets can stay out of this file: FRANCE_TRAVAIL_CLIENT_ID, FRANCE_TRAVAIL_CLIENT_SECRET,
# ADZUNA_APP_ID and ADZUNA_APP_KEY environment variables override the values below.

";

/// Refuses to overwrite an existing file and creates it 0600 because it may receive API secrets.
pub fn write_default(path: &Path) -> Result<(), Error> {
    let io_err = |e| Error::Io(path.to_path_buf(), e);
    if path.exists() {
        return Err(Error::AlreadyExists(path.to_path_buf()));
    }
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(io_err)?;
    }
    let body = toml::to_string(&Config::default()).map_err(Error::Encode)?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists { Error::AlreadyExists(path.to_path_buf()) } else { io_err(e) }
    })?;
    file.write_all(format!("{HEADER}{body}").as_bytes()).map_err(io_err)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |key| map.get(key).cloned()
    }

    fn write(dir: &tempfile::TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn missing_file_uses_defaults_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let env =
            env_of(&[("FRANCE_TRAVAIL_CLIENT_ID", "id-from-env"), ("FRANCE_TRAVAIL_CLIENT_SECRET", "secret-from-env")]);
        let (cfg, found) = Config::load_with_env(&dir.path().join("missing.toml"), env).unwrap();
        assert!(!found);
        assert!(cfg.has_france_travail() && !cfg.has_adzuna());
        assert_eq!(cfg.jobs.max_drive_minutes, 45.0);
        assert_eq!(cfg.jobs.origin_postal_code, "33000");
        assert_eq!(cfg.news.categories.len(), 5);
    }

    #[test]
    fn file_overrides_only_selected_fields_and_env_wins_for_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "config.toml",
            "
[general]
animations = false

[jobs]
max_drive_minutes = 30
keywords = [\"golang\"]

[adzuna]
app_id = \"a\"
app_key = \"k\"
",
        );
        let (cfg, found) = Config::load_with_env(&path, env_of(&[("ADZUNA_APP_KEY", "k-env")])).unwrap();
        assert!(found);
        assert!(!cfg.general.animations && cfg.general.hyperlinks);
        assert_eq!(cfg.jobs.max_drive_minutes, 30.0);
        assert_eq!(cfg.jobs.keywords, ["golang"]);
        assert_eq!(cfg.jobs.origin_insee, Jobs::default().origin_insee);
        assert_eq!(cfg.adzuna.app_key, "k-env");
        assert_eq!(cfg.news.categories, news::default_categories());
    }

    #[test]
    fn custom_categories_replace_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "config.toml",
            "
[[news.categories]]
key = \"rust\"
title = \"RUST\"
[[news.categories.sources]]
name = \"This Week in Rust\"
url = \"https://this-week-in-rust.org/rss.xml\"
",
        );
        let (cfg, _) = Config::load_with_env(&path, env_of(&[])).unwrap();
        assert_eq!(cfg.news.categories.len(), 1);
        assert_eq!(cfg.news.categories[0].sources[0].name, "This Week in Rust");
    }

    #[test]
    fn syntax_and_validation_errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let broken = write(&dir, "broken.toml", "[jobs\nmax=");
        assert!(matches!(Config::load_with_env(&broken, env_of(&[])), Err(Error::Parse(..))));

        let invalid = write(&dir, "invalid.toml", "[jobs]\nmax_drive_minutes = -5\norigin_lat = 120.0\n");
        let err = Config::load_with_env(&invalid, env_of(&[])).unwrap_err().to_string();
        assert!(err.contains("max_drive_minutes") && err.contains("origin_lat"), "{err}");

        let terminal = write(&dir, "terminal.toml", "[work]\nterminal = \"xterm\"\nsession = \"a:b\"\n");
        let err = Config::load_with_env(&terminal, env_of(&[])).unwrap_err().to_string();
        assert!(err.contains("work.terminal must be one of: auto, kitty,") && err.contains("work.session"), "{err}");

        let headless = write(&dir, "headless.toml", "[[news.categories]]\ntitle = \"NO KEY\"\n");
        let err = Config::load_with_env(&headless, env_of(&[])).unwrap_err().to_string();
        assert!(err.contains("\"NO KEY\" needs a key"), "{err}");
    }

    #[test]
    fn write_default_round_trips_with_private_mode_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        write_default(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(fs::read_to_string(&path).unwrap().starts_with("# nikka configuration."));
        let (cfg, found) = Config::load_with_env(&path, env_of(&[])).unwrap();
        assert!(found);
        assert_eq!(cfg, Config::default());
        assert!(matches!(write_default(&path), Err(Error::AlreadyExists(_))));
    }

    #[test]
    fn configs_written_by_earlier_releases_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "config.toml",
            "
[general]
  animations = true
  hyperlinks = true

[jobs]
  top = 5
  max_drive_minutes = 45.0
  france_travail_domain = \"M18\"

  [[news.categories]]
    key = \"ai\"
    title = \"ARTIFICIAL INTELLIGENCE\"
    half_life_hours = 24.0
    max_age_hours = 96.0

    [[news.categories.sources]]
      name = \"Hacker News\"
      url = \"https://hn.algolia.com/api/v1/search?tags=front_page&hitsPerPage=50\"
      kind = \"hn\"
      weight = 1.1
",
        );
        let (cfg, _) = Config::load_with_env(&path, env_of(&[])).unwrap();
        assert_eq!(cfg.jobs.ft_domain, "M18");
        assert_eq!(cfg.news.categories[0].sources[0].weight, 1.1);
    }
}
