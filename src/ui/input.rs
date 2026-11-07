use std::io::{self, Read};
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::event::{PollFd, PollFlags, Timespec, poll};

const POLL_TICK: Timespec = Timespec { tv_sec: 0, tv_nsec: 100_000_000 };

/// Lets Ctrl-C abort a pending prompt: SIGINT is caught for the whole run, so a plain
/// blocking read would keep waiting for Enter after the operator asked to stop.
pub struct InterruptibleInput<R> {
    inner: R,
    interrupted: Arc<AtomicBool>,
}

impl<R> InterruptibleInput<R> {
    pub fn new(inner: R, interrupted: Arc<AtomicBool>) -> Self {
        InterruptibleInput { inner, interrupted }
    }
}

impl<R: Read + AsFd> Read for InterruptibleInput<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.interrupted.load(Ordering::Relaxed) {
                return Err(io::Error::other("interrupted by operator"));
            }
            let ready = poll(&mut [PollFd::new(&self.inner, PollFlags::IN)], Some(&POLL_TICK));
            match ready {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Ok(_) => return self.inner.read(buf),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn reads_lines_once_data_arrives() {
        let (mut tx, rx) = UnixStream::pair().unwrap();
        let mut input = BufReader::new(InterruptibleInput::new(rx, Arc::new(AtomicBool::new(false))));
        tx.write_all(b"y\n").unwrap();
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        assert_eq!(line, "y\n");
        drop(tx);
        assert_eq!(input.read_line(&mut String::new()).unwrap(), 0, "a closed input is EOF");
    }

    #[test]
    fn interrupt_unblocks_a_waiting_prompt() {
        let (_tx, rx) = UnixStream::pair().unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let mut input = BufReader::new(InterruptibleInput::new(rx, flag.clone()));
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        assert!(input.read_line(&mut String::new()).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        setter.join().unwrap();
    }
}
