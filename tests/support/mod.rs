//! A child process on a pseudo-terminal, for tests that drive interactive
//! programs (shells here; the history picker in Phase B).
//!
//! The child gets the pty's slave side as stdin, stdout and stderr, in a new
//! session with the pty as its controlling terminal, so it behaves exactly as
//! it would in a terminal window. A thread drains the master side into a
//! buffer, so the child never blocks on a full pty.

#![allow(dead_code, reason = "each test crate uses a different part")]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Pty {
    master: File,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    /// Start `command` on a new `rows` x `cols` pty.
    pub fn spawn(mut command: Command, rows: u16, cols: u16) -> std::io::Result<Pty> {
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the out-pointers are valid; name and termios may be null.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        };
        if opened != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openpty succeeded, so both are open descriptors we now own.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // SAFETY: plain fcntl on a descriptor we own. Keeps the master out of
        // the child, which only needs the slave.
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };

        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        // The parent's copies of the slave live in `command`; closing them is
        // what lets the reader see the end once the child is gone.
        drop(command);

        let master = File::from(master);
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut source = master.try_clone()?;
        let sink = Arc::clone(&output);
        // Detached: a background job of the child may hold the slave open a
        // little longer than the child itself lives.
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            // Linux reports EIO once every slave descriptor is closed.
            while let Ok(read @ 1..) = source.read(&mut buffer) {
                sink.lock().unwrap().extend_from_slice(&buffer[..read]);
            }
        });
        Ok(Pty {
            master,
            child,
            output,
        })
    }

    /// Type `bytes` as they are.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.master.write_all(bytes)?;
        self.master.flush()
    }

    /// Type `line` and Enter.
    pub fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.write(format!("{}\n", line).as_bytes())
    }

    /// Wait up to `timeout` for the child to exit; `None` if it is still
    /// running (it is killed when the `Pty` is dropped).
    pub fn wait(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Everything the child has written so far, lossily decoded.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
