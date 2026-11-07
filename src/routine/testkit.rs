use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use jiff::Zoned;
use tempfile::TempDir;

use super::{Deps, Options, Routine, Section};
use crate::config::Config;
use crate::execx::{Fake, RootAccess};
use crate::ui::testing::{Capture, ui};
use crate::ui::{self as tui};

#[derive(Default)]
pub struct FakeRoot {
    deny: bool,
    calls: AtomicUsize,
}

impl FakeRoot {
    pub fn denying() -> Self {
        FakeRoot { deny: true, ..FakeRoot::default() }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl RootAccess for FakeRoot {
    fn ensure(&self) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.deny { Err(io::Error::other("sudo: 3 incorrect password attempts")) } else { Ok(()) }
    }
}

pub struct Harness {
    pub routine: Routine,
    pub out: Capture,
    pub runner: Arc<Fake>,
    pub root: Arc<FakeRoot>,
    pub interrupted: Arc<AtomicBool>,
    pub run_dir: TempDir,
    _home: TempDir,
}

pub struct Setup {
    pub sections: Vec<Section>,
    pub opts: Options,
    pub cfg: Config,
    pub fake: Fake,
    pub root: FakeRoot,
    pub ui: tui::Options,
    pub input: String,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            sections: Vec::new(),
            opts: Options::default(),
            cfg: Config::default(),
            fake: Fake::new(),
            root: FakeRoot::default(),
            ui: tui::Options { width: 110, ..tui::Options::default() },
            input: String::new(),
        }
    }
}

fn fixed_now() -> Zoned {
    "2026-10-05T12:00:00+00:00[UTC]".parse().expect("valid timestamp")
}

pub fn harness(setup: Setup) -> Harness {
    let (u, out) = ui(&setup.input, setup.ui);
    let runner = Arc::new(setup.fake);
    let root = Arc::new(setup.root);
    let interrupted = Arc::new(AtomicBool::new(false));
    let opts = Options { sections: setup.sections, version: "test".to_string(), ..setup.opts };
    let deps = Deps {
        ui: u,
        runner: runner.clone(),
        root: root.clone(),
        http: crate::httpx::Client::new("test", None),
        interrupted: interrupted.clone(),
    };
    let mut routine = Routine::new(setup.cfg, opts, deps);
    let run_dir = tempfile::tempdir().expect("temp dir");
    let home = tempfile::tempdir().expect("temp dir");
    routine.now = fixed_now;
    routine.run_dir = run_dir.path().to_path_buf();
    routine.home = home.path().to_path_buf();
    Harness { routine, out, runner, root, interrupted, run_dir, _home: home }
}
