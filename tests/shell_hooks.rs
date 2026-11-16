//! The recording hooks of `shell/psychic.{zsh,bash,fish}`, in real interactive
//! shells on a pty (`docs/ctrl-r/01-record.md` section 15.3).
//!
//! `preexec`, `PROMPT_COMMAND`, `history 1` and `fish_postexec` only behave
//! truthfully in an interactive shell, so nothing here calls the hook
//! functions directly. A shell that is not installed is skipped, with a line
//! saying so.
//!
//! Everything lives in a scratch directory under the system temp dir: `HOME`,
//! the data dir and `HISTFILE` all point there, and the environment is cleared
//! first, so no real history or data directory is read or written.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::Pty;

const BINARY: &str = env!("CARGO_BIN_EXE_psychic");
const EXPECTED_RUNS: i64 = 5;

/// `T` of the spec: home, data, work/sub, and the hook script.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(shell: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("psychic-shell-{}-{}", shell, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("work/sub")).unwrap();
        // The shells report physical paths for a directory they start in.
        let root = root.canonicalize().unwrap();
        // With the scratch HOME: every subcommand but track-command opens
        // app.log in the default data dir, which must not be the real one.
        let hook = Command::new(BINARY)
            .env_clear()
            .env("HOME", root.join("home"))
            .arg(shell)
            .output()
            .expect("psychic runs");
        assert!(hook.status.success(), "psychic {shell} prints the hook");
        std::fs::write(root.join("hook"), hook.stdout).unwrap();
        Scratch { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn display(&self, relative: &str) -> String {
        self.path(relative).to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn installed(shell: &str) -> bool {
    Command::new(shell)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// The shell, cleared environment and all, ready to put on a pty.
fn shell_command(shell: &str, scratch: &Scratch) -> Command {
    let mut command = Command::new(shell);
    command
        .env_clear()
        .env("HOME", scratch.path("home"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TERM", "dumb")
        .env("PSYCHIC_BIN", BINARY)
        .env("PSYCHIC_DATA_DIR", scratch.path("data"))
        .env("PSYCHIC_HISTORY", "1")
        .env("PSYCHIC_NO_RETRAIN", "1")
        .env("HISTFILE", scratch.path("home/hist"))
        .current_dir(scratch.path("work"));
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
            let source = format!("source {}", scratch.display("hook"));
            // Not `--no-config`: fish 4 turns on private mode with it, and the
            // hook rightly records nothing in private mode. With HOME and the
            // XDG variables pointing into the scratch dir, the only config
            // fish finds is the system's.
            command
                .env("XDG_CONFIG_HOME", scratch.path("home/.config"))
                .env("XDG_DATA_HOME", scratch.path("home/.local/share"))
                .args(["-i", "-C", source.as_str()]);
        }
        other => unreachable!("no shell test for {other}"),
    }
    command
}

/// What gets typed, one line at a time.
fn lines(shell: &str, scratch: &Scratch) -> Vec<String> {
    let mut lines = Vec::new();
    if shell == "zsh" {
        lines.push("setopt histignorespace".to_string());
        lines.push("HISTORY_IGNORE='secret*'".to_string());
    }
    if shell != "fish" {
        lines.push(format!("source {}", scratch.display("hook")));
    }
    for line in [
        "true",
        "false",
        "cd sub",
        "echo in-sub",
        " echo leading-space",
    ] {
        lines.push(line.to_string());
    }
    if shell != "fish" {
        lines.push("secret-thing".to_string());
    }
    lines.push("echo 'multi".to_string());
    lines.push("line'".to_string());
    lines
}

/// One recorded run, as the assertions want it.
struct Row {
    text: String,
    template: String,
    cwd: String,
    exit_code: Option<i64>,
    source: String,
    shell_session: String,
}

fn count_runs(db: &Path) -> Option<i64> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.query_row("SELECT COUNT(*) FROM command_runs", [], |row| row.get(0))
        .ok()
}

/// Poll until `EXPECTED_RUNS` runs are there (the hook's calls run in the
/// background and may finish after the shell), or 5 s pass.
fn wait_for_runs(db: &Path) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count = count_runs(db).unwrap_or(0);
        if count >= EXPECTED_RUNS || Instant::now() > deadline {
            // A moment more, so a run beyond the expected ones would show up.
            std::thread::sleep(Duration::from_millis(200));
            return count_runs(db).unwrap_or(0);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_rows(db: &Path) -> Vec<Row> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = conn
        .prepare(
            "SELECT c.text, c.template, r.cwd, r.exit_code, r.source, r.shell_session
             FROM command_runs r JOIN commands c ON c.id = r.command_id
             ORDER BY r.started_at, r.id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(Row {
                text: row.get(0)?,
                template: row.get(1)?,
                cwd: row.get(2)?,
                exit_code: row.get(3)?,
                source: row.get(4)?,
                shell_session: row.get(5)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Ctrl-D on an empty line ends all three shells without a command that could
/// itself be recorded. fish 4 sometimes drops a Ctrl-D that arrives while it is
/// still finishing the previous command, so it is sent again every 2 s (a
/// second one on an empty line does nothing else), for up to 30 s.
fn end_with_ctrl_d(pty: &mut Pty) -> Option<std::process::ExitStatus> {
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

fn check_shell(shell: &str) {
    if !installed(shell) {
        println!("skipped: {shell} not installed");
        return;
    }
    let scratch = Scratch::new(shell);
    let mut pty = Pty::spawn(shell_command(shell, &scratch), 24, 80).expect("the shell starts");
    std::thread::sleep(Duration::from_millis(500));
    for line in lines(shell, &scratch) {
        pty.write_line(&line).unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }
    let status = end_with_ctrl_d(&mut pty);
    assert!(
        status.is_some(),
        "{shell} did not exit; it printed:\n{}",
        pty.output()
    );

    let db = scratch.path("data/events.db");
    let count = wait_for_runs(&db);
    let rows = if count > 0 {
        read_rows(&db)
    } else {
        Vec::new()
    };
    let summary: Vec<String> = rows
        .iter()
        .map(|row| {
            format!(
                "{:?} in {} exit {:?} ({})",
                row.text, row.cwd, row.exit_code, row.source
            )
        })
        .collect();
    assert_eq!(
        count,
        EXPECTED_RUNS,
        "{shell} recorded {summary:#?}\nterminal:\n{}",
        pty.output()
    );

    let work = scratch.display("work");
    let sub = scratch.display("work/sub");
    let simple: Vec<(&str, &str, Option<i64>, &str)> = rows[..4]
        .iter()
        .map(|row| {
            (
                row.text.as_str(),
                row.cwd.as_str(),
                row.exit_code,
                row.source.as_str(),
            )
        })
        .collect();
    assert_eq!(
        simple,
        vec![
            ("true", work.as_str(), Some(0), "hook"),
            ("false", work.as_str(), Some(1), "hook"),
            ("cd sub", work.as_str(), Some(0), "hook"),
            ("echo in-sub", sub.as_str(), Some(0), "hook"),
        ],
        "{shell}"
    );
    let multi = &rows[4];
    assert!(
        multi.text.starts_with("echo 'multi")
            && (multi.text.contains('\n') || multi.text.contains(';'))
            && multi.text.ends_with("line'"),
        "{shell} multi-line text: {:?}",
        multi.text
    );
    assert_eq!(
        (multi.cwd.as_str(), multi.exit_code, multi.source.as_str()),
        (sub.as_str(), Some(0), "hook")
    );
    assert!(
        rows.iter()
            .all(|row| !row.text.contains("leading-space") && !row.text.contains("secret")),
        "{shell} recorded an opted-out command: {summary:#?}"
    );
    assert!(
        rows.iter()
            .all(|row| row.shell_session == rows[0].shell_session),
        "{shell}: one session"
    );
    assert_eq!(rows[2].template, "cd PATH");
    println!("{shell}: {} runs recorded as expected", rows.len());
}

#[test]
fn zsh_records_what_the_user_ran() {
    check_shell("zsh");
}

#[test]
fn bash_records_what_the_user_ran() {
    check_shell("bash");
}

#[test]
fn fish_records_what_the_user_ran() {
    check_shell("fish");
}
