use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::builder::PossibleValuesParser;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use signal_hook::consts::{SIGINT, SIGTERM};

use nikka::config::{self, Config};
use nikka::execx::{OsRunner, Sudo};
use nikka::httpx;
use nikka::routine::{self, Deps, Outcome, Routine, Section};
use nikka::{logo, ui, work};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// nikka — daily ops & intel for your Linux desktop
#[derive(Parser, Debug)]
#[command(name = "nikka", version, disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Same as the work command
    #[arg(long)]
    work: bool,

    /// Terminal for work [default: from config, auto]
    #[arg(long, global = true, value_name = "NAME", value_parser = PossibleValuesParser::new(work::choices()))]
    terminal: Option<String>,

    /// Answer yes to every confirmation
    #[arg(short, long, global = true)]
    yes: bool,

    /// Show what would run, change nothing, never ask for root
    #[arg(short = 'n', long, global = true)]
    dry_run: bool,

    /// Refresh indexes and report, but do not upgrade or purge
    #[arg(long, global = true)]
    check: bool,

    /// Disable boot animation
    #[arg(long, global = true)]
    no_anim: bool,

    /// Disable clickable terminal hyperlinks
    #[arg(long, global = true)]
    no_links: bool,

    /// Config file path [default: ~/.config/nikka/config.toml]
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Subcommand, Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    /// Patch + purge + intel briefing (default)
    All,
    /// Check and upgrade apt / snap / flatpak (+ fwupd check)
    Sys,
    /// Purge unused docker images, stale /tmp, caches, journals
    Clean,
    /// News + skills + jobs, no root needed
    Intel,
    /// AI · SpaceX · cybersecurity · France · Suisse headlines
    News,
    /// Top skills demanded on the software market
    Skills,
    /// Top offers within the drive-time budget
    Jobs,
    /// Open a terminal on a 4-pane tmux session
    Work,
    /// Write a default config file
    Init,
    /// Print version
    Version,
}

impl Command {
    fn sections(self) -> &'static [Section] {
        match self {
            Command::All => &[Section::Sys, Section::Clean, Section::News, Section::Skills, Section::Jobs],
            Command::Sys => &[Section::Sys],
            Command::Clean => &[Section::Clean],
            Command::Intel => &[Section::News, Section::Skills, Section::Jobs],
            Command::News => &[Section::News],
            Command::Skills => &[Section::Skills],
            Command::Jobs => &[Section::Jobs],
            Command::Work | Command::Init | Command::Version => &[],
        }
    }
}

impl Cli {
    /// `--work` stands for the work command, so it cannot name another one.
    fn selected(&self) -> Result<Command, clap::Error> {
        match (self.work, self.command) {
            (false, command) => Ok(command.unwrap_or(Command::All)),
            (true, None | Some(Command::Work)) => Ok(Command::Work),
            (true, Some(_)) => {
                Err(Cli::command().error(ErrorKind::ArgumentConflict, "--work cannot be combined with another command"))
            }
        }
    }
}

fn main() -> ExitCode {
    run(Cli::parse())
}

fn run(cli: Cli) -> ExitCode {
    let config_path = cli.config.clone().unwrap_or_else(config::default_path);
    let command = match cli.selected() {
        Ok(command) => command,
        Err(e) => e.exit(),
    };
    match command {
        Command::Version => {
            println!("nikka {VERSION}");
            return ExitCode::SUCCESS;
        }
        Command::Init => {
            return match config::write_default(&config_path) {
                Ok(()) => {
                    println!("config written to {}", config_path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        _ => {}
    }

    let (cfg, found) = match Config::load(&config_path) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::from(2);
        }
    };

    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM] {
        if let Err(e) = signal_hook::flag::register(signal, interrupted.clone()) {
            eprintln!("error: cannot handle signal {signal}: {e}");
            return ExitCode::FAILURE;
        }
    }

    let (tty, width) = ui::detect_terminal();
    let (color, _) = logo::detect(&io::stdout());
    ui::set_color(color);
    let out = ui::Ui::new(
        io::stdout(),
        BufReader::new(ui::InterruptibleInput::new(io::stdin(), interrupted.clone())),
        ui::Options {
            tty,
            color,
            animations: cfg.general.animations && !cli.no_anim,
            hyperlinks: cfg.general.hyperlinks && !cli.no_links,
            assume_yes: cli.yes,
            width,
        },
    );
    if command == Command::Work {
        let opts = work::Options {
            terminal: cli.terminal.unwrap_or(cfg.work.terminal),
            session: cfg.work.session,
            dir: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from(std::env::var_os("HOME").unwrap_or_default())),
            state_dir: config::cache_dir(),
            dry_run: cli.dry_run,
        };
        let opened = work::open(&out, &OsRunner::new(), |key| std::env::var(key).ok(), &opts);
        return if opened { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }

    let deps = Deps {
        ui: out,
        runner: Arc::new(OsRunner::new()),
        root: Arc::new(Sudo::new(&format!("  {} password for %u: ", ui::magenta("⚿")))),
        http: httpx::Client::new(&format!("nikka/{VERSION} (personal desktop routine)"), config::cache_dir()),
        interrupted,
    };

    let opts = routine::Options {
        sections: command.sections().to_vec(),
        dry_run: cli.dry_run,
        check_only: cli.check,
        config_path,
        config_seen: found,
        version: VERSION.to_string(),
    };
    match Routine::new(cfg, opts, deps).run() {
        Outcome::Completed => ExitCode::SUCCESS,
        Outcome::Failed => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("nikka").chain(args.iter().copied()))
    }

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn flags_work_before_and_after_the_command() {
        for (args, command, dry_run, yes) in [
            (&[][..], None, false, false),
            (&["clean", "--dry-run"][..], Some(Command::Clean), true, false),
            (&["-n", "-y", "sys"][..], Some(Command::Sys), true, true),
            (&["--yes", "intel", "--no-anim"][..], Some(Command::Intel), false, true),
        ] {
            let cli = parse(args).unwrap();
            assert_eq!((cli.command, cli.dry_run, cli.yes), (command, dry_run, yes), "{args:?}");
        }
    }

    #[test]
    fn work_is_a_command_and_a_flag() {
        for args in [&["work"][..], &["--work"][..], &["--work", "work"][..], &["work", "--terminal", "ghostty"][..]] {
            assert_eq!(parse(args).unwrap().selected().unwrap(), Command::Work, "{args:?}");
        }
        assert_eq!(parse(&["--terminal", "ptyxis", "--work"]).unwrap().terminal.as_deref(), Some("ptyxis"));
        assert_eq!(parse(&["--work", "news"]).unwrap().selected().unwrap_err().kind(), ErrorKind::ArgumentConflict);
        assert_eq!(parse(&["work", "--terminal", "xterm"]).unwrap_err().kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn config_path_flag() {
        let cli = parse(&["news", "--config", "/tmp/x.toml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/x.toml")));
    }

    #[test]
    fn usage_errors_and_help() {
        for args in [&["bogus"][..], &["news", "extra"][..], &["--unknown-flag"][..]] {
            assert!(parse(args).is_err(), "{args:?} must fail");
        }
        assert_eq!(parse(&["-h"]).unwrap_err().kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn every_routine_command_has_sections() {
        for c in
            [Command::All, Command::Sys, Command::Clean, Command::Intel, Command::News, Command::Skills, Command::Jobs]
        {
            assert!(!c.sections().is_empty(), "{c:?}");
        }
        assert_eq!(Command::All.sections().len(), 5);
    }
}
