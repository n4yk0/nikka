//! Opens a terminal on four panes: a 2×2 tmux grid, or the terminal's own panes when tmux is missing.

use std::fs;
use std::path::{Path, PathBuf};

use crate::execx::{Command, Runner, cmd};
use crate::ui::{Ui, muted};

pub const AUTO: &str = "auto";

const PANES: usize = 4;
const KITTY_SESSION_FILE: &str = "work.kitty-session";
const KITTY_GRID_SESSION: &str = "layout grid\nlaunch\nlaunch\nlaunch\nlaunch\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminal {
    Kitty,
    Ghostty,
    Ptyxis,
    GnomeTerminal,
    Konsole,
    Xfce4Terminal,
    XTerminalEmulator,
}

impl Terminal {
    /// Auto-detection falls back to the first installed one, in this order.
    pub const ALL: [Terminal; 7] = [
        Terminal::Kitty,
        Terminal::Ghostty,
        Terminal::Ptyxis,
        Terminal::GnomeTerminal,
        Terminal::Konsole,
        Terminal::Xfce4Terminal,
        Terminal::XTerminalEmulator,
    ];

    /// Also the binary name.
    pub fn name(self) -> &'static str {
        match self {
            Terminal::Kitty => "kitty",
            Terminal::Ghostty => "ghostty",
            Terminal::Ptyxis => "ptyxis",
            Terminal::GnomeTerminal => "gnome-terminal",
            Terminal::Konsole => "konsole",
            Terminal::Xfce4Terminal => "xfce4-terminal",
            Terminal::XTerminalEmulator => "x-terminal-emulator",
        }
    }

    pub fn from_name(name: &str) -> Option<Terminal> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The terminal nikka runs in, from the variables each one exports to its shells (tmux keeps them).
    fn current(env: &dyn Fn(&str) -> Option<String>) -> Option<Terminal> {
        let set = |key: &str| env(key).is_some_and(|v| !v.is_empty());
        let term = env("TERM").unwrap_or_default();
        if set("KITTY_WINDOW_ID") || term == "xterm-kitty" {
            Some(Terminal::Kitty)
        } else if set("GHOSTTY_RESOURCES_DIR") || term == "xterm-ghostty" {
            Some(Terminal::Ghostty)
        } else if set("KONSOLE_VERSION") {
            Some(Terminal::Konsole)
        } else if set("GNOME_TERMINAL_SCREEN") {
            Some(Terminal::GnomeTerminal)
        } else {
            None
        }
    }

    /// A new window in `dir` running `program`, or the user's shell when it is empty.
    fn window(self, dir: &str, program: &[&str]) -> Command {
        let (mut args, exec): (Vec<String>, Option<&str>) = match self {
            Terminal::Kitty => (vec!["--directory".into(), dir.into()], None),
            Terminal::Ghostty => (vec![format!("--working-directory={dir}")], Some("-e")),
            Terminal::Ptyxis => (vec!["--new-window".into(), format!("--working-directory={dir}")], Some("--")),
            Terminal::GnomeTerminal => (vec!["--window".into(), format!("--working-directory={dir}")], Some("--")),
            Terminal::Konsole => (vec!["--workdir".into(), dir.into()], Some("-e")),
            Terminal::Xfce4Terminal => (vec![format!("--working-directory={dir}")], Some("-x")),
            // The Debian alternative only guarantees -e, so the window inherits nikka's working directory.
            Terminal::XTerminalEmulator => (Vec::new(), Some("-e")),
        };
        if !program.is_empty() {
            args.extend(exec.map(String::from));
            args.extend(program.iter().map(|a| a.to_string()));
        }
        cmd(self.name(), args)
    }

    /// The terminal's own 2×2 layout, for the terminals whose command line can build one.
    fn grid(self, dir: &str, session_file: &Path) -> Option<Command> {
        match self {
            Terminal::Kitty => Some(cmd("kitty", ["--directory", dir, "--session", &session_file.to_string_lossy()])),
            _ => None,
        }
    }
}

/// Every accepted `terminal` value, `auto` first.
pub fn choices() -> Vec<&'static str> {
    std::iter::once(AUTO).chain(Terminal::ALL.map(Terminal::name)).collect()
}

/// `auto` prefers the terminal nikka runs in, then $TERMINAL, then the first installed one.
pub fn resolve(choice: &str, runner: &dyn Runner, env: &dyn Fn(&str) -> Option<String>) -> Result<Terminal, String> {
    if choice != AUTO {
        let terminal = Terminal::from_name(choice).ok_or_else(|| format!("unknown terminal {choice:?}"))?;
        if !runner.available(terminal.name()) {
            return Err(format!("{choice} is not installed"));
        }
        return Ok(terminal);
    }
    let from_variable = env("TERMINAL").and_then(|t| Path::new(&t).file_name()?.to_str().and_then(Terminal::from_name));
    Terminal::current(env)
        .into_iter()
        .chain(from_variable)
        .chain(Terminal::ALL)
        .find(|t| runner.available(t.name()))
        .ok_or_else(|| format!("no supported terminal installed, install one of: {}", choices()[1..].join(", ")))
}

#[derive(Clone, Debug)]
pub struct Options {
    /// `auto` or a terminal name.
    pub terminal: String,
    pub session: String,
    pub dir: PathBuf,
    /// Holds the kitty session file: kitty only reads startup layouts from a file.
    pub state_dir: Option<PathBuf>,
    pub dry_run: bool,
}

#[derive(Debug)]
struct Plan {
    tmux: bool,
    /// Builds the tmux grid; `None` when the session already runs or tmux is missing.
    setup: Option<Command>,
    windows: Vec<Command>,
    kitty_session: Option<PathBuf>,
    summary: String,
}

/// Returns false when nothing could be opened.
pub fn open(ui: &Ui, runner: &dyn Runner, env: impl Fn(&str) -> Option<String>, opts: &Options) -> bool {
    let plan = match plan(runner, &env, opts) {
        Ok(plan) => plan,
        Err(e) => {
            ui.fail(e);
            return false;
        }
    };
    if !plan.tmux {
        ui.warn("tmux not found: it gives every terminal the same 2×2 grid");
        ui.info(format!("install it with: {}", install_hint(runner)));
    }
    if opts.dry_run {
        ui.step("dry-run, would execute:");
        for c in plan.setup.iter().chain(&plan.windows) {
            ui.line(&muted(format!("  $ {c}")));
        }
        return true;
    }
    if !["WAYLAND_DISPLAY", "DISPLAY"].iter().any(|key| env(key).is_some_and(|v| !v.is_empty())) {
        ui.fail("no graphical session: DISPLAY and WAYLAND_DISPLAY are unset");
        return false;
    }
    match execute(runner, &plan) {
        Ok(()) => {
            ui.ok(&plan.summary);
            true
        }
        Err(e) => {
            ui.fail(e);
            false
        }
    }
}

fn plan(runner: &dyn Runner, env: &dyn Fn(&str) -> Option<String>, opts: &Options) -> Result<Plan, String> {
    let terminal = resolve(&opts.terminal, runner, env)?;
    let (name, dir) = (terminal.name(), opts.dir.to_string_lossy());
    if runner.available("tmux") {
        let target = format!("={}", opts.session);
        let running = runner.output(&cmd("tmux", ["has-session", "-t", target.as_str()])).is_ok();
        let mut attach = terminal.window(&dir, &["tmux", "attach-session", "-t", &target]);
        if env("TMUX").is_some_and(|v| !v.is_empty()) {
            // tmux refuses to attach from inside another session while $TMUX is set.
            attach = attach.with_env(["TMUX="]);
        }
        let summary = if running {
            format!("{name} · attached to the running tmux session {}", opts.session)
        } else {
            format!("{name} · tmux session {} · {PANES} panes in {dir}", opts.session)
        };
        return Ok(Plan {
            tmux: true,
            setup: (!running).then(|| tmux_grid(&opts.session, &dir)),
            windows: vec![attach],
            kitty_session: None,
            summary,
        });
    }
    let session_file = opts.state_dir.as_ref().map(|d| d.join(KITTY_SESSION_FILE));
    if let Some(grid) = session_file.as_deref().and_then(|f| terminal.grid(&dir, f)) {
        return Ok(Plan {
            tmux: false,
            setup: None,
            windows: vec![grid],
            kitty_session: session_file,
            summary: format!("{name} · {PANES} panes in {dir}"),
        });
    }
    Ok(Plan {
        tmux: false,
        setup: None,
        windows: vec![terminal.window(&dir, &[]); PANES],
        kitty_session: None,
        summary: format!("{name} cannot split from the command line · {PANES} windows in {dir}"),
    })
}

/// One tmux call: the session, three splits, then an even 2×2 layout focused on the top-left pane.
/// Explicit targets keep a surrounding tmux session or a custom base-index out of the way.
fn tmux_grid(session: &str, dir: &str) -> Command {
    let window = format!("={session}:");
    let mut args = vec!["new-session", "-d", "-s", session, "-c", dir];
    for _ in 1..PANES {
        args.extend([";", "split-window", "-t", window.as_str(), "-c", dir]);
    }
    let top_left = format!("{window}.{{top-left}}");
    args.extend([";", "select-layout", "-t", window.as_str(), "tiled", ";", "select-pane", "-t", top_left.as_str()]);
    cmd("tmux", args)
}

fn execute(runner: &dyn Runner, plan: &Plan) -> Result<(), String> {
    if let Some(path) = &plan.kitty_session {
        path.parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(path, KITTY_GRID_SESSION))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(setup) = &plan.setup {
        runner.output(setup).map_err(|e| e.to_string())?;
    }
    plan.windows.iter().try_for_each(|w| runner.spawn(w).map_err(|e| e.to_string()))
}

fn install_hint(runner: &dyn Runner) -> &'static str {
    [
        ("apt-get", "sudo apt install tmux"),
        ("dnf", "sudo dnf install tmux"),
        ("pacman", "sudo pacman -S tmux"),
        ("zypper", "sudo zypper install tmux"),
    ]
    .into_iter()
    .find(|(manager, _)| runner.available(manager))
    .map_or("your package manager", |(_, hint)| hint)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::execx::Fake;
    use crate::ui::Options as UiOptions;
    use crate::ui::testing::ui;

    const GRID: &str = "tmux new-session -d -s work -c /src ; split-window -t =work: -c /src ; \
                        split-window -t =work: -c /src ; split-window -t =work: -c /src ; \
                        select-layout -t =work: tiled ; select-pane -t =work:.{top-left}";

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |key| map.get(key).cloned()
    }

    fn options(terminal: &str) -> Options {
        Options {
            terminal: terminal.into(),
            session: "work".into(),
            dir: "/src".into(),
            state_dir: None,
            dry_run: false,
        }
    }

    fn fake(binaries: &[&str]) -> Fake {
        binaries.iter().fold(Fake::new(), |f, b| f.binary(b))
    }

    fn run(f: &Fake, env: &[(&str, &str)], opts: &Options) -> (bool, String) {
        let (u, out) = ui("", UiOptions::default());
        let opened = open(&u, f, env_of(env), opts);
        (opened, out.plain())
    }

    #[test]
    fn auto_prefers_the_current_terminal_then_terminal_variable_then_install_order() {
        let f = fake(&["kitty", "ghostty", "ptyxis"]);
        let pick = |env: &[(&str, &str)]| resolve(AUTO, &f, &env_of(env));
        assert_eq!(pick(&[("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty")]), Ok(Terminal::Ghostty));
        assert_eq!(pick(&[("TERMINAL", "/usr/bin/ptyxis")]), Ok(Terminal::Ptyxis));
        assert_eq!(pick(&[("KONSOLE_VERSION", "250401")]), Ok(Terminal::Kitty), "konsole is not installed");
        assert_eq!(pick(&[]), Ok(Terminal::Kitty));

        let err = resolve(AUTO, &Fake::new(), &env_of(&[])).unwrap_err();
        assert!(err.starts_with("no supported terminal installed, install one of: kitty, ghostty,"), "{err}");
    }

    #[test]
    fn an_explicit_terminal_must_be_installed() {
        let f = fake(&["kitty"]);
        assert_eq!(resolve("kitty", &f, &env_of(&[])), Ok(Terminal::Kitty));
        assert_eq!(resolve("ghostty", &f, &env_of(&[])), Err("ghostty is not installed".to_string()));
        assert_eq!(resolve("xterm", &f, &env_of(&[])), Err("unknown terminal \"xterm\"".to_string()));
        assert_eq!(choices()[..3], ["auto", "kitty", "ghostty"]);
    }

    #[test]
    fn each_terminal_opens_a_window_in_the_directory() {
        let attach = ["tmux", "attach-session", "-t", "=work"];
        for (terminal, want) in [
            (Terminal::Kitty, "kitty --directory /src tmux attach-session -t =work"),
            (Terminal::Ghostty, "ghostty --working-directory=/src -e tmux attach-session -t =work"),
            (Terminal::Ptyxis, "ptyxis --new-window --working-directory=/src -- tmux attach-session -t =work"),
            (
                Terminal::GnomeTerminal,
                "gnome-terminal --window --working-directory=/src -- tmux attach-session -t =work",
            ),
            (Terminal::Konsole, "konsole --workdir /src -e tmux attach-session -t =work"),
            (Terminal::Xfce4Terminal, "xfce4-terminal --working-directory=/src -x tmux attach-session -t =work"),
            (Terminal::XTerminalEmulator, "x-terminal-emulator -e tmux attach-session -t =work"),
        ] {
            assert_eq!(terminal.window("/src", &attach).to_string(), want);
        }
        assert_eq!(Terminal::Ghostty.window("/src", &[]).to_string(), "ghostty --working-directory=/src");
    }

    #[test]
    fn tmux_builds_a_two_by_two_grid_then_the_terminal_attaches() {
        let f = fake(&["tmux", "ghostty", "apt-get"])
            .on(tmux_grid("work", "/src"), "")
            .on(Terminal::Ghostty.window("/src", &["tmux", "attach-session", "-t", "=work"]), "");
        let (opened, out) = run(&f, &[("DISPLAY", ":0")], &options(AUTO));
        assert!(opened, "{out}");
        assert_eq!(
            f.ran(),
            ["tmux has-session -t =work", GRID, "ghostty --working-directory=/src -e tmux attach-session -t =work"]
        );
        assert!(out.contains("✔ ghostty · tmux session work · 4 panes in /src"), "{out}");
        assert!(!out.contains("tmux not found"), "{out}");
    }

    #[test]
    fn a_running_session_is_reattached_and_tmux_nesting_is_lifted() {
        let attach = Terminal::Kitty.window("/src", &["tmux", "attach-session", "-t", "=work"]).with_env(["TMUX="]);
        let f = fake(&["tmux", "kitty"]).on(cmd("tmux", ["has-session", "-t", "=work"]), "").on(attach, "");
        let env = [("WAYLAND_DISPLAY", "wayland-0"), ("TMUX", "/tmp/tmux-1000/default,42,0")];
        let (opened, out) = run(&f, &env, &options("kitty"));
        assert!(opened, "{out}");
        assert_eq!(f.ran(), ["tmux has-session -t =work", "TMUX= kitty --directory /src tmux attach-session -t =work"]);
        assert!(out.contains("kitty · attached to the running tmux session work"), "{out}");
    }

    #[test]
    fn without_tmux_kitty_lays_out_its_own_grid_and_tmux_is_suggested() {
        let state = tempfile::tempdir().unwrap();
        let session_file = state.path().join("nikka").join(KITTY_SESSION_FILE);
        let grid = Terminal::Kitty.grid("/src", &session_file).unwrap();
        let f = fake(&["kitty", "apt-get"]).on(grid.clone(), "");
        let opts = Options { state_dir: Some(state.path().join("nikka")), ..options(AUTO) };
        let (opened, out) = run(&f, &[("DISPLAY", ":0")], &opts);
        assert!(opened, "{out}");
        assert_eq!(f.ran(), [grid.to_string()]);
        assert_eq!(fs::read_to_string(&session_file).unwrap(), KITTY_GRID_SESSION);
        for want in ["▲ tmux not found", "install it with: sudo apt install tmux", "✔ kitty · 4 panes in /src"] {
            assert!(out.contains(want), "missing {want:?}\n{out}");
        }
    }

    #[test]
    fn without_tmux_other_terminals_open_one_window_per_pane() {
        let f = fake(&["gnome-terminal"]).on(Terminal::GnomeTerminal.window("/src", &[]), "");
        let (opened, out) = run(&f, &[("DISPLAY", ":0")], &options(AUTO));
        assert!(opened, "{out}");
        assert_eq!(f.ran(), vec!["gnome-terminal --window --working-directory=/src"; 4]);
        assert!(out.contains("install it with: your package manager"), "{out}");
        assert!(out.contains("gnome-terminal cannot split from the command line · 4 windows in /src"), "{out}");
    }

    #[test]
    fn dry_run_lists_the_commands_without_a_display() {
        let f = fake(&["tmux", "kitty"]);
        let (opened, out) = run(&f, &[], &Options { dry_run: true, ..options(AUTO) });
        assert!(opened, "{out}");
        assert_eq!(f.ran(), ["tmux has-session -t =work"], "only the read-only probe runs");
        assert!(out.contains(&format!("$ {GRID}")), "{out}");
        assert!(out.contains("$ kitty --directory /src tmux attach-session -t =work"), "{out}");
    }

    #[test]
    fn failures_are_reported() {
        let (opened, out) = run(&fake(&["tmux", "kitty"]), &[], &options(AUTO));
        assert!(!opened && out.contains("✖ no graphical session"), "{out}");

        let (opened, out) = run(&fake(&["tmux"]), &[("DISPLAY", ":0")], &options(AUTO));
        assert!(!opened && out.contains("✖ no supported terminal installed"), "{out}");

        let (opened, out) = run(&fake(&["tmux", "kitty"]), &[("DISPLAY", ":0")], &options(AUTO));
        assert!(!opened && out.contains("✖ tmux new-session -d -s work"), "an unscripted setup fails\n{out}");
    }
}
