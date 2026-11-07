use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use jiff::civil::DateTime;
use jiff::tz::TimeZone;

use super::{BoxError, Estimate, Target};

pub(super) fn days(n: i64) -> Duration {
    Duration::from_secs(n.max(0).unsigned_abs() * 86_400)
}

pub fn human_days(d: Duration) -> String {
    match d.as_secs() / 86_400 {
        1 => "1 day".to_string(),
        n => format!("{n} days"),
    }
}

pub(super) fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[derive(Clone)]
pub(super) struct StaleFiles {
    pub name: String,
    pub dir: PathBuf,
    pub max_age: Duration,
    pub exclude: Vec<String>,
    /// `None` accepts any owner.
    pub owned_by: Option<u32>,
    /// Judge each regular file on its own instead of whole top-level entries.
    pub per_file: bool,
    pub now: fn() -> SystemTime,
}

impl Default for StaleFiles {
    fn default() -> Self {
        StaleFiles {
            name: String::new(),
            dir: PathBuf::new(),
            max_age: Duration::ZERO,
            exclude: Vec::new(),
            owned_by: None,
            per_file: false,
            now: SystemTime::now,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    path: PathBuf,
    size: u64,
}

pub(super) fn stale_files_target(s: StaleFiles) -> Target<'static> {
    let scanner = s.clone();
    Target {
        name: s.name.clone(),
        root: false,
        commands: vec![format!("remove entries of {} untouched for {}", s.dir.display(), human_days(s.max_age))],
        estimate: Box::new(move || Ok(summarize(&scanner.scan()?))),
        run: Box::new(move |w| remove_all(&s.scan()?, w)),
    }
}

impl StaleFiles {
    fn scan(&self) -> io::Result<Vec<Candidate>> {
        if self.max_age.is_zero() {
            return Ok(Vec::new());
        }
        let cutoff = (self.now)().checked_sub(self.max_age).unwrap_or(SystemTime::UNIX_EPOCH);
        if self.per_file {
            return Ok(self.scan_files(cutoff));
        }
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if self.excluded(&name.to_string_lossy()) {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else { continue };
            if !self.owned(&meta) || is_special(&meta) {
                continue;
            }
            if let Some((newest, size)) = newest_in_tree(&path, self.owned_by) {
                if newest < cutoff {
                    found.push(Candidate { path, size });
                }
            }
        }
        Ok(found)
    }

    fn scan_files(&self, cutoff: SystemTime) -> Vec<Candidate> {
        let mut found = Vec::new();
        let mut pending = vec![self.dir.clone()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    pending.push(entry.path());
                } else if meta.is_file() && self.owned(&meta) && meta.modified().is_ok_and(|m| m < cutoff) {
                    found.push(Candidate { path: entry.path(), size: meta.len() });
                }
            }
        }
        found
    }

    fn excluded(&self, name: &str) -> bool {
        self.exclude.iter().any(|prefix| name.starts_with(prefix.as_str()))
    }

    fn owned(&self, meta: &Metadata) -> bool {
        self.owned_by.is_none_or(|uid| meta.uid() == uid)
    }
}

fn is_special(meta: &Metadata) -> bool {
    let t = meta.file_type();
    t.is_socket() || t.is_fifo() || t.is_block_device() || t.is_char_device()
}

/// Refuses (`None`) any tree it cannot fully inspect or that holds sockets or
/// foreign-owned files: those usually belong to a live process. Symlinks are never followed.
fn newest_in_tree(root: &Path, owned_by: Option<u32>) -> Option<(SystemTime, u64)> {
    let mut newest = SystemTime::UNIX_EPOCH;
    let mut size = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).ok()?;
        if is_special(&meta) || owned_by.is_some_and(|uid| meta.uid() != uid) {
            return None;
        }
        newest = newest.max(meta.modified().ok()?);
        if meta.is_file() {
            size += meta.len();
        }
        if meta.is_dir() {
            for entry in fs::read_dir(&path).ok()? {
                pending.push(entry.ok()?.path());
            }
        }
    }
    Some((newest, size))
}

fn summarize(found: &[Candidate]) -> Estimate {
    Estimate { bytes: Some(found.iter().map(|c| c.size).sum()), detail: format!("{} entries", found.len()) }
}

fn remove_path(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn remove_all(found: &[Candidate], w: &mut (dyn Write + Send)) -> Result<(), BoxError> {
    let mut failures = 0;
    for c in found {
        if let Err(e) = remove_path(&c.path) {
            failures += 1;
            let _ = writeln!(w, "skip {}: {e}", c.path.display());
        }
    }
    let _ = writeln!(w, "removed {}/{} entries", found.len() - failures, found.len());
    if failures > 0 {
        return Err(format!("{failures} entries could not be removed").into());
    }
    Ok(())
}

/// Follows the freedesktop.org Trash spec: info/<name>.trashinfo holds DeletionDate for files/<name>.
pub(super) fn trash_target(trash_dir: PathBuf, max_age: Duration) -> Target<'static> {
    trash_target_at(trash_dir, max_age, SystemTime::now)
}

fn trash_target_at(trash_dir: PathBuf, max_age: Duration, now: fn() -> SystemTime) -> Target<'static> {
    let commands = vec![format!("empty {} items trashed more than {} ago", trash_dir.display(), human_days(max_age))];
    let scan = move || -> Vec<Candidate> {
        if max_age.is_zero() {
            return Vec::new();
        }
        let Ok(entries) = fs::read_dir(trash_dir.join("info")) else { return Vec::new() };
        let cutoff = now().checked_sub(max_age).unwrap_or(SystemTime::UNIX_EPOCH);
        let mut infos: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "trashinfo")).collect();
        infos.sort();
        let mut found = Vec::new();
        for info in infos {
            let Some(deleted) = trash_deletion_date(&info) else { continue };
            if deleted >= cutoff {
                continue;
            }
            let Some(name) = info.file_stem() else { continue };
            let payload = trash_dir.join("files").join(name);
            let size = newest_in_tree(&payload, None).map_or(0, |(_, size)| size);
            found.push(Candidate { path: payload, size });
            found.push(Candidate { path: info, size: 0 });
        }
        found
    };
    let estimate_scan = scan.clone();
    Target {
        name: "trash".to_string(),
        root: false,
        commands,
        estimate: Box::new(move || {
            let found = estimate_scan();
            Ok(Estimate { detail: format!("{} items", found.len() / 2), ..summarize(&found) })
        }),
        run: Box::new(move |w| remove_all(&scan(), w)),
    }
}

/// Trash dates are local wall-clock times without offset.
fn trash_deletion_date(info: &Path) -> Option<SystemTime> {
    let content = fs::read_to_string(info).ok()?;
    let value = content.lines().find_map(|l| l.strip_prefix("DeletionDate="))?;
    let civil: DateTime = value.trim().parse().ok()?;
    let zoned = civil.to_zoned(TimeZone::system()).ok()?;
    Some(SystemTime::from(zoned.timestamp()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs::{File, FileTimes};
    use std::os::unix::net::UnixListener;

    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    fn set_mtime(path: &Path, mtime: SystemTime) {
        File::open(path).unwrap().set_times(FileTimes::new().set_modified(mtime)).unwrap();
    }

    fn touch(path: &Path, size: usize, mtime: SystemTime) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; size]).unwrap();
        set_mtime(path, mtime);
    }

    fn stale(dir: &Path, max_age: Duration) -> StaleFiles {
        StaleFiles { dir: dir.into(), max_age, owned_by: Some(current_uid()), ..StaleFiles::default() }
    }

    #[test]
    fn top_level_entries_go_only_when_the_whole_tree_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();
        let old = now - 10 * DAY;
        touch(&dir.join("old.log"), 100, old);
        touch(&dir.join("fresh.log"), 100, now);
        touch(&dir.join("olddir/a"), 50, old);
        touch(&dir.join("olddir/b"), 50, old);
        touch(&dir.join("mixed/old"), 10, old);
        touch(&dir.join("mixed/new"), 10, now);
        touch(&dir.join("keep-me/x"), 10, old);
        for d in ["olddir", "mixed", "keep-me"] {
            set_mtime(&dir.join(d), old);
        }

        let s = StaleFiles { exclude: vec!["keep-".to_string()], ..stale(dir, 7 * DAY) };
        let found: HashMap<String, u64> = s
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| (c.path.file_name().unwrap().to_string_lossy().into_owned(), c.size))
            .collect();
        assert_eq!(found, HashMap::from([("old.log".to_string(), 100), ("olddir".to_string(), 100)]));

        let mut out = Vec::new();
        stale_files_target(s).run(&mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("removed 2/2 entries"));
        for gone in ["old.log", "olddir"] {
            assert!(!dir.join(gone).exists(), "{gone} should be removed");
        }
        for kept in ["fresh.log", "mixed/new", "mixed/old", "keep-me/x"] {
            assert!(dir.join(kept).exists(), "{kept} should be kept");
        }
    }

    #[test]
    fn trees_with_sockets_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app");
        fs::create_dir(&app).unwrap();
        let _listener = UnixListener::bind(app.join("s.sock")).unwrap();
        set_mtime(&app, SystemTime::now() - 30 * DAY);
        assert!(stale(tmp.path(), Duration::from_secs(3600)).scan().unwrap().is_empty());
    }

    #[test]
    fn symlinks_are_removed_without_touching_their_target() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        touch(&outside.path().join("precious"), 10, SystemTime::now() - 30 * DAY);
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("link")).unwrap();
        let s = stale(tmp.path(), DAY);
        let found = s.scan().unwrap();
        assert_eq!(found.len(), 0, "a fresh symlink is not stale");
        remove_all(&[Candidate { path: tmp.path().join("link"), size: 0 }], &mut Vec::new()).unwrap();
        assert!(outside.path().join("precious").exists());
    }

    #[test]
    fn missing_dir_and_disabled_age() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = StaleFiles { owned_by: None, ..stale(&tmp.path().join("nope"), Duration::from_secs(3600)) };
        assert!(missing.scan().unwrap().is_empty());
        assert!(missing.scan_files(SystemTime::now()).is_empty());
        touch(&tmp.path().join("old"), 1, SystemTime::now() - 30 * DAY);
        assert!(stale(tmp.path(), Duration::ZERO).scan().unwrap().is_empty(), "max age 0 disables the target");
    }

    #[test]
    fn per_file_mode_judges_each_file() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join("normal/old.png"), 20, SystemTime::now() - 60 * DAY);
        touch(&tmp.path().join("normal/new.png"), 20, SystemTime::now());
        let found = StaleFiles { per_file: true, ..stale(tmp.path(), 30 * DAY) }.scan().unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("normal/old.png"));
    }

    fn write_trash(trash: &Path, name: &str, deletion: &str, size: usize) {
        touch(&trash.join("files").join(name), size, SystemTime::now());
        let info = format!("[Trash Info]\nPath=/home/u/{name}\nDeletionDate={deletion}\n");
        fs::write(trash.join("info").join(format!("{name}.trashinfo")), info).unwrap();
    }

    fn fixed_now() -> SystemTime {
        "2026-10-05T12:00:00Z".parse::<jiff::Timestamp>().unwrap().into()
    }

    #[test]
    fn trash_purges_old_items_and_their_info_files() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path();
        fs::create_dir_all(trash.join("info")).unwrap();
        write_trash(trash, "ancient.txt", "2026-08-01T10:00:00", 300);
        write_trash(trash, "recent.txt", "2026-10-04T10:00:00", 10);
        fs::write(trash.join("info/broken.trashinfo"), "garbage").unwrap();

        let target = trash_target_at(trash.into(), 30 * DAY, fixed_now);
        assert_eq!(target.estimate().unwrap(), Estimate { bytes: Some(300), detail: "1 items".into() });
        target.run(&mut Vec::new()).unwrap();
        assert!(!trash.join("files/ancient.txt").exists());
        assert!(!trash.join("info/ancient.txt.trashinfo").exists());
        assert!(trash.join("files/recent.txt").exists());
    }

    #[test]
    fn human_days_pluralises() {
        assert_eq!(human_days(days(1)), "1 day");
        assert_eq!(human_days(days(30)), "30 days");
        assert_eq!(days(-3), Duration::ZERO);
    }
}
