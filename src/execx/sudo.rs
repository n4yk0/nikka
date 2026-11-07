use std::io;
use std::process::{self, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(120);

pub trait RootAccess: Send + Sync {
    fn ensure(&self) -> io::Result<()>;
}

/// Caches root credentials once, interactively, so later `sudo -n` commands never prompt mid-output.
pub struct Sudo {
    is_root: bool,
    prompt: String,
    keep_alive: Mutex<Option<Sender<()>>>,
}

impl Sudo {
    pub fn new(prompt: &str) -> Self {
        Sudo { is_root: rustix::process::geteuid().is_root(), prompt: prompt.to_string(), keep_alive: Mutex::new(None) }
    }
}

impl RootAccess for Sudo {
    fn ensure(&self) -> io::Result<()> {
        let mut keep_alive = self.keep_alive.lock().map_err(|_| io::Error::other("sudo state poisoned"))?;
        if self.is_root || keep_alive.is_some() {
            return Ok(());
        }
        if !sudo(&["-n", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?.success() {
            let status = sudo(&["-v", "-p", &self.prompt]).stdout(io::stderr()).status()?;
            if !status.success() {
                return Err(io::Error::other(format!("sudo -v: {status}")));
            }
        }
        let (stop, ticks) = mpsc::channel::<()>();
        thread::spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = ticks.recv_timeout(KEEP_ALIVE_INTERVAL) {
                let _ = sudo(&["-n", "-v"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
        });
        *keep_alive = Some(stop);
        Ok(())
    }
}

fn sudo(args: &[&str]) -> process::Command {
    let mut c = process::Command::new("sudo");
    c.args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_never_runs_sudo() {
        let s = Sudo { is_root: true, prompt: String::new(), keep_alive: Mutex::new(None) };
        s.ensure().unwrap();
        assert!(s.keep_alive.lock().unwrap().is_none());
    }
}
