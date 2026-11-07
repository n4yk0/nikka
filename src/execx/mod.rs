//! Runs external commands, optionally through sudo, behind a trait that tests can fake.

#[cfg(test)]
mod fake;
mod sudo;

use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{self, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;

#[cfg(test)]
pub use fake::Fake;
pub use sudo::{RootAccess, Sudo};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub root: bool,
}

pub fn cmd<I, S>(name: &str, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Command { name: name.to_string(), args: args.into_iter().map(Into::into).collect(), ..Command::default() }
}

impl Command {
    pub fn as_root(mut self) -> Self {
        self.root = true;
        self
    }

    pub fn with_env<I, S>(mut self, env: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env.extend(env.into_iter().map(Into::into));
        self
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<&str> = Vec::with_capacity(self.args.len() + self.env.len() + 2);
        if self.root {
            parts.push("sudo");
        }
        parts.extend(self.env.iter().map(String::as_str));
        parts.push(&self.name);
        parts.extend(self.args.iter().map(String::as_str));
        f.write_str(&parts.join(" "))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Exit { command: String, exit_code: i32, stderr: String },
    Signaled { command: String, signal: i32, stderr: String },
    Spawn { command: String, message: String },
}

impl Error {
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Error::Exit { exit_code, .. } => Some(*exit_code),
            Error::Signaled { .. } | Error::Spawn { .. } => None,
        }
    }

    fn from_status(c: &Command, status: ExitStatus, stderr: &[u8]) -> Error {
        let command = c.to_string();
        let stderr = String::from_utf8_lossy(stderr).into_owned();
        match (status.code(), status.signal()) {
            (None, Some(signal)) => Error::Signaled { command, signal, stderr },
            (code, _) => Error::Exit { command, exit_code: code.unwrap_or(-1), stderr },
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stderr = match self {
            Error::Exit { command, exit_code, stderr } => {
                write!(f, "{command}: exit status {exit_code}")?;
                stderr
            }
            Error::Signaled { command, signal, stderr } => {
                write!(f, "{command}: killed by {}", signal_name(*signal))?;
                stderr
            }
            Error::Spawn { command, message } => return write!(f, "{command}: {message}"),
        };
        let tail = last_lines(stderr, 3);
        if !tail.is_empty() {
            write!(f, ": {tail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

pub trait Runner: Send + Sync {
    fn output(&self, c: &Command) -> Result<Output, Error>;
    fn stream(&self, c: &Command, w: &mut (dyn Write + Send)) -> Result<(), Error>;
    /// Starts a program that outlives nikka, such as a terminal window, without waiting for it.
    fn spawn(&self, c: &Command) -> Result<(), Error>;
    fn available(&self, binary: &str) -> bool;
}

pub struct OsRunner {
    pub is_root: bool,
}

impl OsRunner {
    pub fn new() -> Self {
        OsRunner { is_root: rustix::process::geteuid().is_root() }
    }

    fn invocation(&self, c: &Command) -> (String, Vec<String>, Vec<(String, String)>) {
        if c.root && !self.is_root {
            let mut args = vec!["-n".to_string()];
            if !c.env.is_empty() {
                args.push("env".to_string());
                args.extend(c.env.iter().cloned());
            }
            args.push(c.name.clone());
            args.extend(c.args.iter().cloned());
            return ("sudo".to_string(), args, Vec::new());
        }
        let env =
            c.env.iter().filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
        (c.name.clone(), c.args.clone(), env)
    }

    /// Stdin is always /dev/null so no child (apt hooks, needrestart...) can wait for an answer.
    fn build(&self, c: &Command) -> process::Command {
        let (program, args, env) = self.invocation(c);
        let mut command = process::Command::new(program);
        command.args(args).envs(env).stdin(Stdio::null());
        command
    }
}

impl Default for OsRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner for OsRunner {
    fn available(&self, binary: &str) -> bool {
        find_in_path(binary).is_some()
    }

    fn output(&self, c: &Command) -> Result<Output, Error> {
        let out = self.build(c).output().map_err(|e| spawn_error(c, &e))?;
        if !out.status.success() {
            return Err(Error::from_status(c, out.status, &out.stderr));
        }
        Ok(Output {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit_code: 0,
        })
    }

    fn stream(&self, c: &Command, w: &mut (dyn Write + Send)) -> Result<(), Error> {
        let mut child =
            self.build(c).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| spawn_error(c, &e))?;
        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(Error::Spawn { command: c.to_string(), message: "missing child pipes".to_string() });
        };
        let shared = Mutex::new(w);
        let stderr_text = thread::scope(|s| {
            let stderr_copy = s.spawn(|| {
                let mut captured = Vec::new();
                copy_chunks(&mut stderr, &shared, Some(&mut captured));
                captured
            });
            copy_chunks(&mut stdout, &shared, None);
            stderr_copy.join().unwrap_or_default()
        });
        let status = child.wait().map_err(|e| spawn_error(c, &e))?;
        if status.success() {
            return Ok(());
        }
        Err(Error::from_status(c, status, &stderr_text))
    }

    /// Its own process group keeps the program clear of a Ctrl-C or hangup aimed at nikka's terminal.
    fn spawn(&self, c: &Command) -> Result<(), Error> {
        self.build(c)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map(drop)
            .map_err(|e| spawn_error(c, &e))
    }
}

fn copy_chunks(src: &mut impl Read, dst: &Mutex<&mut (dyn Write + Send)>, mut capture: Option<&mut Vec<u8>>) {
    let mut buf = [0u8; 8192];
    loop {
        match src.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if let Some(captured) = capture.as_deref_mut() {
                    captured.extend_from_slice(&buf[..n]);
                }
                if let Ok(mut w) = dst.lock() {
                    let _ = w.write_all(&buf[..n]);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        1 => "SIGHUP".to_string(),
        2 => "SIGINT".to_string(),
        9 => "SIGKILL".to_string(),
        13 => "SIGPIPE".to_string(),
        15 => "SIGTERM".to_string(),
        n => format!("signal {n}"),
    }
}

fn spawn_error(c: &Command, e: &io::Error) -> Error {
    Error::Spawn { command: c.to_string(), message: e.to_string() }
}

fn find_in_path(binary: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let is_executable =
        |p: &std::path::Path| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if binary.contains('/') {
        let path = std::path::PathBuf::from(binary);
        return is_executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(binary)).find(|p| is_executable(p))
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.trim().split('\n').collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_shows_sudo_env_and_args() {
        let c = cmd("apt-get", ["-y", "upgrade"]).with_env(["DEBIAN_FRONTEND=noninteractive"]).as_root();
        assert_eq!(c.to_string(), "sudo DEBIAN_FRONTEND=noninteractive apt-get -y upgrade");
    }

    #[test]
    fn with_env_does_not_alias_original() {
        let base = cmd("x", Vec::<String>::new()).with_env(["A=1"]);
        let _ = base.clone().with_env(["B=2"]);
        let derived = base.clone().with_env(["C=3"]);
        assert_eq!(base.env, ["A=1"]);
        assert_eq!(derived.env, ["A=1", "C=3"]);
    }

    #[test]
    fn root_commands_go_through_non_interactive_sudo() {
        let r = OsRunner { is_root: false };
        let (program, args, env) = r.invocation(&cmd("apt-get", ["update"]).with_env(["LC_ALL=C"]).as_root());
        assert_eq!(format!("{program} {}", args.join(" ")), "sudo -n env LC_ALL=C apt-get update");
        assert!(env.is_empty());
    }

    #[test]
    fn root_skips_sudo_and_keeps_env() {
        let r = OsRunner { is_root: true };
        let (program, args, env) = r.invocation(&cmd("apt-get", ["update"]).with_env(["LC_ALL=C"]).as_root());
        assert_eq!(format!("{program} {}", args.join(" ")), "apt-get update");
        assert_eq!(env, [("LC_ALL".to_string(), "C".to_string())]);
    }

    #[test]
    fn output_captures_both_streams() {
        let res = OsRunner::new().output(&cmd("sh", ["-c", "echo out; echo err >&2"])).unwrap();
        assert_eq!((res.stdout.as_str(), res.stderr.as_str()), ("out\n", "err\n"));
    }

    #[test]
    fn exit_error_carries_code_and_stderr_tail() {
        let err = OsRunner::new().output(&cmd("sh", ["-c", "echo boom >&2; exit 3"])).unwrap_err();
        assert_eq!(err.exit_code(), Some(3));
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn signal_deaths_name_the_signal() {
        let err = OsRunner::new().output(&cmd("sh", ["-c", "echo dying >&2; kill -INT $$"])).unwrap_err();
        assert_eq!(err.exit_code(), None);
        assert!(err.to_string().ends_with("killed by SIGINT: dying"), "{err}");
        let err = OsRunner::new().stream(&cmd("sh", ["-c", "kill -TERM $$"]), &mut Vec::new()).unwrap_err();
        assert!(err.to_string().ends_with("killed by SIGTERM"), "{err}");
    }

    #[test]
    fn missing_binary_is_not_an_exit_code() {
        let err = OsRunner::new().output(&cmd("definitely-not-a-binary-n4yk0", Vec::<String>::new())).unwrap_err();
        assert_eq!(err.exit_code(), None);
        assert!(!OsRunner::new().available("definitely-not-a-binary-n4yk0"));
        assert!(OsRunner::new().available("sh"));
    }

    #[test]
    fn stream_forwards_stdout_and_stderr() {
        let mut buf = Vec::new();
        OsRunner::new().stream(&cmd("sh", ["-c", "echo a; echo b >&2"]), &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("a\n") && text.contains("b\n"), "{text:?}");
    }

    #[test]
    fn stream_reports_exit_code_with_stderr() {
        let mut buf = Vec::new();
        let err = OsRunner::new().stream(&cmd("sh", ["-c", "echo nope >&2; exit 4"]), &mut buf).unwrap_err();
        assert_eq!(err.exit_code(), Some(4));
        assert!(err.to_string().ends_with(": nope"), "{err}");
    }

    #[test]
    fn children_never_inherit_terminal_stdin() {
        let res = OsRunner::new().output(&cmd("sh", ["-c", r#"read answer; echo "answer=[$answer]""#])).unwrap();
        assert_eq!(res.stdout, "answer=[]\n", "a prompting child must read EOF immediately");
    }

    #[test]
    fn spawned_programs_get_their_own_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("pgid");
        let script = format!("ps -o pgid= -p $$ | tr -d ' ' > {}.tmp && mv {0}.tmp {0}", marker.display());
        OsRunner::new().spawn(&cmd("sh", ["-c", &script])).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            thread::sleep(std::time::Duration::from_millis(20));
        }
        let pgid = std::fs::read_to_string(&marker).expect("spawned program ran");
        assert_ne!(pgid.trim(), rustix::process::getpgrp().as_raw_nonzero().to_string());
        let err = OsRunner::new().spawn(&cmd("definitely-not-a-binary-n4yk0", Vec::<String>::new())).unwrap_err();
        assert!(matches!(err, Error::Spawn { .. }), "{err}");
    }

    #[test]
    fn last_lines_keeps_the_tail() {
        assert_eq!(last_lines("a\nb\nc\nd\n", 3), "b | c | d");
        assert_eq!(last_lines("  \n", 3), "");
    }
}
