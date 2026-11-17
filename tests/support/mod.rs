//! A child process on a pseudo-terminal, for tests that drive interactive
//! programs: the shells (`tests/shell_hooks.rs`) and the history picker
//! (`tests/picker.rs`). Also the scratch home and the shell set-up both use.
//!
//! The child gets the pty's slave side as stdin, stdout and stderr (stdout
//! can go to a file instead), in a new session with the pty as its
//! controlling terminal, so it behaves exactly as it would in a terminal
//! window. A thread drains the master side into a buffer, so the child never
//! blocks on a full pty.

#![allow(dead_code, reason = "each test crate uses a different part")]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
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
    pub fn spawn(command: Command, rows: u16, cols: u16) -> std::io::Result<Pty> {
        Self::spawn_with_stdout(command, rows, cols, None)
    }

    /// As [`Pty::spawn`], with stdout going to `stdout` when given (what a
    /// shell widget's `$(...)` does) rather than to the pty.
    pub fn spawn_with_stdout(
        mut command: Command,
        rows: u16,
        cols: u16,
        stdout: Option<File>,
    ) -> std::io::Result<Pty> {
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

        let out = match stdout {
            Some(file) => Stdio::from(file),
            None => Stdio::from(slave.try_clone()?),
        };
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(out)
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

    /// Wait until the child has written something and then nothing for
    /// `quiet` (a TUI has drawn and settled), for up to `limit`. Returns
    /// whether it settled.
    pub fn wait_quiet(&self, quiet: Duration, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut seen = self.output.lock().unwrap().len();
        let mut changed = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let now = self.output.lock().unwrap().len();
            if now != seen {
                seen = now;
                changed = Instant::now();
            } else if seen > 0 && changed.elapsed() >= quiet {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// Wait up to `limit` for the child to write `needle`; returns whether it did.
    pub fn wait_for(&self, needle: &str, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if self.output().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The child's process id, to send it a signal.
    pub fn pid(&self) -> libc::pid_t {
        libc::pid_t::try_from(self.child.id()).expect("a pid fits in pid_t")
    }

    /// Whether the terminal is in canonical (cooked) mode with echo, as a
    /// shell expects to find it; `false` when a program left it raw. Read
    /// from the slave side, through its name: Linux keeps a separate
    /// termios for the master.
    pub fn is_cooked(&self) -> bool {
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: the buffer is valid for its length; the master is open.
        let found =
            unsafe { libc::ptsname_r(self.master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
        assert_eq!(found, 0, "the pty has a slave name");
        // SAFETY: ptsname_r wrote a NUL-terminated string.
        let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(std::ffi::OsStr::from_bytes(name.to_bytes()))
            .expect("the slave opens");
        // SAFETY: termios is plain data; tcgetattr fills it.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: a valid descriptor and out-pointer.
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut termios) },
            0
        );
        termios.c_lflag & (libc::ICANON | libc::ECHO) == (libc::ICANON | libc::ECHO)
    }

    /// How many bytes the child has written so far.
    pub fn output_len(&self) -> usize {
        self.output.lock().unwrap().len()
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

// ---------------------------------------------------------------------------
// Scratch home and shells
// ---------------------------------------------------------------------------

pub const BINARY: &str = env!("CARGO_BIN_EXE_psychic");

/// `T` of the specs: home, data, work/sub, and the shell script (`hook`).
pub struct Scratch {
    pub root: PathBuf,
}

impl Scratch {
    /// A scratch dir with `shell`'s script in `hook`.
    pub fn new(shell: &str, tag: &str) -> Self {
        let scratch = Self::empty(shell, tag);
        let hook = scratch.psychic(&[shell]);
        std::fs::write(scratch.path("hook"), hook).unwrap();
        scratch
    }

    /// A scratch dir with no shell script. `kind` and `tag` name it; each
    /// pair must be unique across all tests of the crate.
    pub fn empty(kind: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "psychic-shell-{}-{}-{}",
            kind,
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("work/sub")).unwrap();
        // The shells report physical paths for a directory they start in.
        let root = root.canonicalize().unwrap();
        Scratch { root }
    }

    /// Run psychic with the scratch HOME and data dir: every subcommand but
    /// track-command opens app.log, which must not be in the real data dir.
    pub fn psychic(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new(BINARY)
            .env_clear()
            .env("HOME", self.path("home"))
            .arg("--data-dir")
            .arg(self.path("data"))
            .args(args)
            .output()
            .expect("psychic runs");
        assert!(output.status.success(), "psychic {args:?} succeeds");
        output.stdout
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn display(&self, relative: &str) -> String {
        self.path(relative).to_string_lossy().into_owned()
    }

    pub fn db(&self) -> PathBuf {
        self.path("data/events.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

pub fn installed(shell: &str) -> bool {
    let found = Command::new(shell)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if !found {
        println!("skipped: {shell} not installed");
    }
    found
}

/// How recording gets turned on.
#[derive(Clone, Copy)]
pub enum OptIn {
    /// `PSYCHIC_HISTORY=1`.
    Variable,
    /// The marker file `psychic history --enable --no-import` writes.
    Marker,
    /// Neither: recording stays off.
    Off,
}

/// The shell, cleared environment and all, ready to put on a pty.
pub fn shell_command(shell: &str, scratch: &Scratch, opt_in: OptIn) -> Command {
    let mut command = Command::new(shell);
    command
        .env_clear()
        .env("HOME", scratch.path("home"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TERM", "dumb")
        .env("PSYCHIC_BIN", BINARY)
        .env("PSYCHIC_DATA_DIR", scratch.path("data"))
        .env("PSYCHIC_NO_RETRAIN", "1")
        .env("HISTFILE", scratch.path("home/hist"))
        .current_dir(scratch.path("work"));
    match opt_in {
        OptIn::Variable => {
            command.env("PSYCHIC_HISTORY", "1");
            // Create the database now: the first two `track-command`s of a
            // session would otherwise both create it, and one can lose the
            // race to set up WAL mode ("database is locked"), dropping a run.
            scratch.psychic(&["history", "--status"]);
        }
        OptIn::Marker => {
            scratch.psychic(&["history", "--enable", "--no-import"]);
            assert!(scratch.path("data/history-enabled").exists());
        }
        OptIn::Off => {}
    }
    match shell {
        "zsh" => {
            command
                .env("ZDOTDIR", scratch.path("home"))
                .args(["-f", "-i"]);
        }
        "bash" => {
            command
                .env("HISTCONTROL", "ignorespace")
                .env("HISTIGNORE", "secret*")
                .args(["--norc", "--noprofile", "-i"]);
        }
        "fish" => {
            // Not `--no-config`: fish 4 turns on private mode with it, and the
            // hook rightly records nothing in private mode. With HOME and the
            // XDG variables pointing into the scratch dir, the only config
            // fish finds is the system's. The hook is sourced before the first
            // prompt, so the `source` line itself is never a command.
            // `fish-pre`, when there, runs before the hook (a binding the
            // user made first, say); `fish-init` after it.
            let mut init = String::new();
            if scratch.path("fish-pre").exists() {
                init.push_str(&format!("source {}; ", scratch.display("fish-pre")));
            }
            init.push_str(&format!("source {}", scratch.display("hook")));
            if scratch.path("fish-init").exists() {
                init.push_str(&format!("; source {}", scratch.display("fish-init")));
            }
            command
                .env("XDG_CONFIG_HOME", scratch.path("home/.config"))
                .env("XDG_DATA_HOME", scratch.path("home/.local/share"))
                .args(["-i", "-C", init.as_str()]);
        }
        other => unreachable!("no shell test for {other}"),
    }
    command
}

/// One thing done at the terminal.
pub enum Step {
    /// Type a line and Enter.
    Line(String),
    /// Ctrl-C at the prompt.
    CtrlC,
    /// Another shell appends a command to the shared HISTFILE.
    OtherShellWrites(&'static str),
}

pub fn line(text: &str) -> Step {
    Step::Line(text.to_string())
}

/// zsh and bash load the hook by typing `source`; fish loads it at start.
pub fn load_hook(shell: &str, scratch: &Scratch) -> Vec<Step> {
    match shell {
        "fish" => Vec::new(),
        _ => vec![Step::Line(format!("source {}", scratch.display("hook")))],
    }
}

/// Start `command` on a pty, play `steps` with `pause` after each, end with
/// Ctrl-D, and return what the terminal showed.
pub fn play(scratch: &Scratch, command: Command, steps: &[Step], pause: Duration) -> String {
    let mut pty = Pty::spawn(command, 24, 80).expect("the shell starts");
    std::thread::sleep(Duration::from_millis(500));
    for step in steps {
        match step {
            Step::Line(text) => pty.write_line(text).unwrap(),
            Step::CtrlC => pty.write(&[0x03]).unwrap(),
            Step::OtherShellWrites(text) => {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(scratch.path("home/hist"))
                    .unwrap();
                writeln!(file, "{text}").unwrap();
            }
        }
        std::thread::sleep(pause);
    }
    let status = end_with_ctrl_d(&mut pty);
    assert!(
        status.is_some(),
        "the shell did not exit; it printed:\n{}",
        pty.output()
    );
    pty.output()
}

/// Ctrl-D on an empty line ends all three shells without a command that could
/// itself be recorded. fish 4 sometimes drops a Ctrl-D that arrives while it is
/// still finishing the previous command, so it is sent again every 2 s (a
/// second one on an empty line does nothing else), for up to 30 s.
pub fn end_with_ctrl_d(pty: &mut Pty) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        // Writing fails once the shell is gone and the pty closed.
        let _ = pty.write(&[0x04]);
        if let Some(status) = pty.wait(Duration::from_secs(2)) {
            return Some(status);
        }
    }
    None
}
