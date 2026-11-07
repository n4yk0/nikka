use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Mutex;

use super::{Command, Error, Output, Runner};

/// Scripted runner keyed by the command's display form; unknown commands fail with exit code 127.
#[derive(Default)]
pub struct Fake {
    responses: HashMap<String, Result<Output, (Output, Error)>>,
    binaries: HashSet<String>,
    ran: Mutex<Vec<String>>,
}

impl Fake {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn binary(mut self, name: &str) -> Self {
        self.binaries.insert(name.to_string());
        self
    }

    pub fn on(mut self, c: Command, stdout: &str) -> Self {
        self.responses.insert(c.to_string(), Ok(Output { stdout: stdout.to_string(), ..Output::default() }));
        self
    }

    pub fn on_error(mut self, c: Command, out: Output, err: Error) -> Self {
        self.responses.insert(c.to_string(), Err((out, err)));
        self
    }

    pub fn ran(&self) -> Vec<String> {
        self.ran.lock().map(|r| r.clone()).unwrap_or_default()
    }

    fn reply(&self, c: &Command) -> (Output, Option<Error>) {
        let key = c.to_string();
        if let Ok(mut ran) = self.ran.lock() {
            ran.push(key.clone());
        }
        match self.responses.get(&key) {
            Some(Ok(out)) => (out.clone(), None),
            Some(Err((out, err))) => (out.clone(), Some(err.clone())),
            None => (
                Output { exit_code: 127, ..Output::default() },
                Some(Error::Exit { command: key, exit_code: 127, stderr: "fake: unscripted command".to_string() }),
            ),
        }
    }
}

impl Runner for Fake {
    fn output(&self, c: &Command) -> Result<Output, Error> {
        match self.reply(c) {
            (out, None) => Ok(out),
            (_, Some(err)) => Err(err),
        }
    }

    fn stream(&self, c: &Command, w: &mut (dyn Write + Send)) -> Result<(), Error> {
        let (out, err) = self.reply(c);
        let _ = w.write_all(out.stdout.as_bytes());
        err.map_or(Ok(()), Err)
    }

    fn spawn(&self, c: &Command) -> Result<(), Error> {
        self.output(c).map(drop)
    }

    fn available(&self, binary: &str) -> bool {
        self.binaries.contains(binary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execx::cmd;

    #[test]
    fn unscripted_commands_fail_with_127_and_are_recorded() {
        let f = Fake::new();
        assert_eq!(f.output(&cmd("nope", Vec::<String>::new())).unwrap_err().exit_code(), Some(127));
        assert_eq!(f.ran(), ["nope"]);
    }

    #[test]
    fn scripted_errors_still_stream_their_stdout() {
        let c = cmd("apt-get", ["-y", "upgrade"]);
        let err = Error::Exit { command: c.to_string(), exit_code: 100, stderr: "E: lock".to_string() };
        let f = Fake::new().on_error(c.clone(), Output { stdout: "partial\n".to_string(), ..Output::default() }, err);
        let mut buf = Vec::new();
        assert_eq!(f.stream(&c, &mut buf).unwrap_err().exit_code(), Some(100));
        assert_eq!(buf, b"partial\n");
    }
}
