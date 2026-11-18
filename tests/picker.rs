//! The `psychic history` picker on a pty, alone and under the Ctrl-R widgets
//! of real zsh, bash and fish (`docs/ctrl-r/02-browse.md` section 14.2).
//!
//! Everything lives in a scratch directory: `HOME`, the data dir and
//! `HISTFILE` point there and the environment is cleared, so no real history
//! or data directory is read or written. A shell that is not installed is
//! skipped, with a line saying so.
//!
//! Each key waits until the terminal has been quiet for a moment, so it goes
//! to the picker (or the shell) once that has drawn, not before.

mod support;

use std::path::Path;
use std::process::Command;
use std::time::Duration;
use support::{BINARY, OptIn, Pty, Scratch, end_with_ctrl_d, installed, shell_command};

const QUIET: Duration = Duration::from_millis(300);
const SETTLE_LIMIT: Duration = Duration::from_secs(10);
const ENTER: &[u8] = b"\r";
const ESC: &[u8] = b"\x1b";
const CTRL_R: &[u8] = b"\x12";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Record `text` as run `ago` seconds ago in `cwd`, in session `seed`.
fn seed(scratch: &Scratch, text: &str, ago: i64, cwd: &str) {
    let started_at = (now() - ago).to_string();
    scratch.psychic(&[
        "track-command",
        "--cwd",
        cwd,
        "--exit-code",
        "0",
        "--started-at",
        &started_at,
        "--session",
        "seed",
        "--",
        text,
    ]);
}

fn settle(pty: &Pty, what: &str) {
    assert!(
        pty.wait_quiet(QUIET, SETTLE_LIMIT),
        "{what} never settled; the terminal showed:\n{}",
        pty.output()
    );
}

/// One query, read-only, every column as text joined by `|`.
fn query(db: &Path, sql: &str) -> Vec<String> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = conn.prepare(sql).unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|i| {
                    Ok(match row.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => "NULL".to_string(),
                        rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                        rusqlite::types::ValueRef::Text(t) => {
                            String::from_utf8_lossy(t).into_owned()
                        }
                        other => format!("{other:?}"),
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()
                .map(|cells| cells.join("|"))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

// ---------------------------------------------------------------------------
// The picker alone (IT-B-05, IT-B-07)
// ---------------------------------------------------------------------------

/// Run the picker on a pty with stdout going to a file, type `keys` (each
/// after the terminal settles), and return its exit status and stdout.
fn pick(scratch: &Scratch, keys: &[&[u8]]) -> (std::process::ExitStatus, String, String) {
    let out = scratch.path("out");
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("HOME", scratch.path("home"))
        .env("PATH", scratch.path_env())
        .env("TERM", "xterm-256color")
        .env("PSYCHIC_NO_RETRAIN", "1")
        .arg("--data-dir")
        .arg(scratch.path("data"))
        .args(["history", "--cwd", "/tmp"]);
    let file = std::fs::File::create(&out).unwrap();
    let mut pty = Pty::spawn_with_stdout(command, 30, 100, Some(file)).expect("the picker starts");
    for key in keys {
        settle(&pty, "the picker");
        pty.write(key).unwrap();
    }
    let status = pty
        .wait(Duration::from_secs(10))
        .unwrap_or_else(|| panic!("the picker did not exit:\n{}", pty.output()));
    let stdout = std::fs::read_to_string(&out).unwrap();
    (status, stdout, pty.output())
}

#[test]
fn it_b_05_enter_prints_the_pick_and_escape_prints_nothing() {
    let scratch = Scratch::empty("picker", "alone");
    seed(&scratch, "echo one", 100, "/w");
    seed(&scratch, "echo two", 50, "/w");

    let (status, stdout, terminal) = pick(&scratch, &[ENTER]);
    assert!(status.success(), "{terminal}");
    assert_eq!(stdout, "echo two\n", "the newest is row 1");
    assert!(
        !terminal.contains("printed only"),
        "no hint when stdout is captured: {terminal:?}"
    );

    let (status, stdout, _) = pick(&scratch, &[b"one", ENTER]);
    assert!(status.success());
    assert_eq!(stdout, "echo one\n");

    let (status, stdout, _) = pick(&scratch, &[ESC]);
    assert!(status.success());
    assert_eq!(stdout, "");

    let log = std::fs::read_to_string(scratch.path("data/app.log")).unwrap();
    assert!(log.contains("\"op\":\"cmd_first_full_render\""), "{log}");
    assert!(log.contains("\"op\":\"cmd_query\""), "{log}");
    assert!(!log.contains("echo"), "a command reached app.log:\n{log}");

    // Both picks were logged, each with an impression of the pick.
    let picks = query(
        &scratch.db(),
        "SELECT c.text, e.query, e.rank, e.cwd, e.shell_session, c.pick_count \
         FROM command_events e JOIN commands c ON c.id = e.command_id \
         WHERE e.action = 'pick' ORDER BY e.id",
    );
    assert_eq!(
        picks,
        ["echo two||1|/tmp|NULL|1", "echo one|one|1|/tmp|NULL|1"]
    );
    let impressions = query(
        &scratch.db(),
        "SELECT COUNT(*) FROM command_events WHERE action = 'impression'",
    );
    assert!(
        impressions[0].parse::<u32>().unwrap() >= 2,
        "{impressions:?}"
    );
}

/// IT-B-19: `psychic history` typed at the prompt (stdout the terminal):
/// the pick is printed, then one line on stderr says how to get it onto the
/// prompt instead, tailored to whether the integration is loaded.
#[test]
fn it_b_19_by_hand_the_pick_is_printed_with_a_hint() {
    let scratch = Scratch::empty("picker", "by-hand");
    seed(&scratch, "echo by-hand", 100, "/w");
    for (shell, init) in [(None, true), (Some("zsh"), false)] {
        let mut command = Command::new(BINARY);
        command
            .env_clear()
            .env("HOME", scratch.path("home"))
            .env("PATH", scratch.path_env())
            .env("TERM", "xterm-256color")
            .env("PSYCHIC_NO_RETRAIN", "1")
            .arg("--data-dir")
            .arg(scratch.path("data"))
            .args(["history", "--cwd", "/tmp"]);
        if let Some(shell) = shell {
            command.env("PSYCHIC_SHELL", shell);
        }
        let mut pty = Pty::spawn(command, 30, 100).expect("the picker starts");
        settle(&pty, "the picker");
        pty.write(ENTER).unwrap();
        let status = pty.wait(Duration::from_secs(10)).expect("the picker exits");
        assert!(status.success());
        settle(&pty, "the picker's exit");
        let terminal = pty.output();
        let left = terminal.rfind("\x1b[?1049l").expect("the picker left");
        let after = &terminal[left..];
        let pick = after.find("echo by-hand\r\n").expect("the pick is printed");
        let hint = after
            .find("psychic: printed only, not put on your prompt.")
            .unwrap_or_else(|| panic!("no hint: {after:?}"));
        assert!(pick < hint, "the pick first: {after:?}");
        for needed in ["Ctrl-R", "type ph", "psychic history --status"] {
            assert!(after[hint..].contains(needed), "{needed}: {after:?}");
        }
        assert_eq!(after[hint..].contains("psychic init"), init, "{after:?}");
    }
}

#[test]
fn it_b_12_enter_prints_the_pick_at_once_while_another_process_holds_the_write_lock() {
    let scratch = Scratch::empty("picker", "write-locked");
    seed(&scratch, "echo locked-one", 100, "/w");
    // An import or a purge's VACUUM in another process, holding the lock
    // for longer than the picker's whole run.
    let other = rusqlite::Connection::open(scratch.db()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();

    let out = scratch.path("out");
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("HOME", scratch.path("home"))
        .env("PATH", scratch.path_env())
        .env("TERM", "xterm-256color")
        .env("PSYCHIC_NO_RETRAIN", "1")
        .arg("--data-dir")
        .arg(scratch.path("data"))
        .args(["history", "--cwd", "/tmp"]);
    let file = std::fs::File::create(&out).unwrap();
    let mut pty = Pty::spawn_with_stdout(command, 30, 100, Some(file)).expect("the picker starts");
    settle(&pty, "the picker");
    let pressed = std::time::Instant::now();
    pty.write(ENTER).unwrap();
    let status = pty
        .wait(Duration::from_secs(15))
        .unwrap_or_else(|| panic!("the picker did not exit:\n{}", pty.output()));
    let took = pressed.elapsed();

    assert!(status.success(), "{}", pty.output());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "echo locked-one\n");
    // The pick waits at most CMD_EVENT_WAIT_MS (100 ms) on the lock; the
    // rest is the exit. Before, it waited busy_timeout (5 s) twice.
    assert!(took < Duration::from_secs(1), "Enter to exit took {took:?}");
    other.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        query(
            &scratch.db(),
            "SELECT COUNT(*) FROM command_events WHERE action = 'pick'"
        ),
        ["0"],
        "the pick was not logged"
    );
    let log = std::fs::read_to_string(scratch.path("data/app.log")).unwrap();
    assert!(
        log.contains("command event logging failed: starting a transaction failed: database busy"),
        "{log}"
    );
}

#[test]
fn it_b_15_escape_right_after_a_delete_still_deletes() {
    let scratch = Scratch::empty("picker", "delete-then-esc");
    seed(&scratch, "echo keep-me", 100, "/w");
    seed(&scratch, "echo delete-me", 50, "/w");
    let mut pty = start_picker(&scratch, &[]);
    settle(&pty, "the picker");

    // Another process holds the write lock, so the worker's delete waits in
    // it (busy_timeout, 5 s) while Esc is pressed: what a slow delete
    // transaction does, made certain.
    let other = rusqlite::Connection::open(scratch.db()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    pty.write(b"\x1b[3~").unwrap(); // Delete: the newest row, "echo delete-me"
    settle(&pty, "the delete question");
    pty.write(b"y").unwrap();
    settle(&pty, "the confirmed delete");
    pty.write(ESC).unwrap();
    std::thread::sleep(HOLD);
    other.execute_batch("ROLLBACK").unwrap();
    drop(other);

    let status = pty
        .wait(Duration::from_secs(10))
        .unwrap_or_else(|| panic!("the picker did not exit:\n{}", pty.output()));
    assert!(status.success(), "{}", pty.output());
    assert_eq!(std::fs::read_to_string(scratch.path("out")).unwrap(), "");
    assert_eq!(
        query(&scratch.db(), "SELECT text FROM commands ORDER BY id"),
        ["echo keep-me"],
        "the delete finished before the picker exited"
    );
    assert_eq!(
        query(&scratch.db(), "SELECT COUNT(*) FROM deleted_commands"),
        ["1"]
    );
}

#[test]
fn it_b_15_the_wait_for_a_delete_at_exit_is_bounded() {
    let scratch = Scratch::empty("picker", "delete-bounded");
    seed(&scratch, "echo keep-me", 100, "/w");
    seed(&scratch, "echo delete-me", 50, "/w");
    let mut pty = start_picker(&scratch, &[]);
    settle(&pty, "the picker");

    // The lock is held for longer than the picker waits at exit.
    let other = rusqlite::Connection::open(scratch.db()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    pty.write(b"\x1b[3~").unwrap();
    settle(&pty, "the delete question");
    pty.write(b"y").unwrap();
    settle(&pty, "the confirmed delete");
    let pressed = std::time::Instant::now();
    pty.write(ESC).unwrap();
    let status = pty
        .wait(Duration::from_secs(4))
        .unwrap_or_else(|| panic!("the picker did not exit:\n{}", pty.output()));
    let took = pressed.elapsed();
    other.execute_batch("ROLLBACK").unwrap();

    assert!(status.success(), "{}", pty.output());
    // CMD_EXIT_DELETE_WAIT_MS (1 s), then the exit; well short of the
    // worker's own busy_timeout (5 s).
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_millis(2500),
        "Esc to exit took {took:?}"
    );
    let log = std::fs::read_to_string(scratch.path("data/app.log")).unwrap();
    assert!(log.contains("a delete may not have finished"), "{log}");
}

/// How long [`it_b_15_escape_right_after_a_delete_still_deletes`] holds the
/// lock after Esc: well inside the picker's exit wait for a delete
/// (`CMD_EXIT_DELETE_WAIT_MS`, 1 s).
const HOLD: Duration = Duration::from_millis(300);

/// What `input::leave_tui` writes, in its order: keyboard flags popped,
/// mouse capture off (its last sequence), main screen, cursor shown.
const RESTORE: [&str; 4] = ["\x1b[<1u", "\x1b[?1000l", "\x1b[?1049l", "\x1b[?25h"];

/// The picker on a pty with `env` added and stdout to a file, settled.
fn start_picker(scratch: &Scratch, env: &[(&str, &str)]) -> Pty {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("HOME", scratch.path("home"))
        .env("PATH", scratch.path_env())
        .env("TERM", "xterm-256color")
        .env("PSYCHIC_NO_RETRAIN", "1")
        .envs(env.iter().copied())
        .arg("--data-dir")
        .arg(scratch.path("data"))
        .args(["history", "--cwd", "/tmp"]);
    let file = std::fs::File::create(scratch.path("out")).unwrap();
    Pty::spawn_with_stdout(command, 30, 100, Some(file)).expect("the picker starts")
}

/// The terminal was given back after it was set up: every sequence of
/// [`RESTORE`] after the last switch to the alternate screen, in order, and
/// the pty in cooked mode. Returns where the restore ends in `terminal`.
fn assert_restored(pty: &Pty, terminal: &str) -> usize {
    let entered = terminal
        .rfind("\x1b[?1049h")
        .unwrap_or_else(|| panic!("the picker never set up the terminal:\n{terminal:?}"));
    let mut at = entered;
    for sequence in RESTORE {
        at += terminal[at..]
            .find(sequence)
            .unwrap_or_else(|| panic!("{sequence:?} missing after set-up:\n{terminal:?}"));
    }
    assert!(pty.is_cooked(), "the terminal was left in raw mode");
    at
}

#[test]
fn e_b_20_a_panic_in_the_picker_gives_the_terminal_back_before_its_message() {
    let scratch = Scratch::empty("picker", "panic");
    seed(&scratch, "echo panic-one", 100, "/w");
    let mut pty = start_picker(&scratch, &[("PSYCHIC_HISTORY_TEST_PANIC", "1")]);
    let status = pty
        .wait(SETTLE_LIMIT)
        .unwrap_or_else(|| panic!("the picker did not exit:\n{}", pty.output()));
    // Let the reader thread drain what the picker wrote last.
    std::thread::sleep(QUIET);

    assert_eq!(status.code(), Some(101), "a panic's exit status");
    let terminal = pty.output();
    let restored = assert_restored(&pty, &terminal);
    let message = terminal
        .find("PSYCHIC_HISTORY_TEST_PANIC is set")
        .unwrap_or_else(|| panic!("no panic message:\n{terminal:?}"));
    assert!(message > restored, "the message printed on the main screen");
    assert_eq!(std::fs::read_to_string(scratch.path("out")).unwrap(), "");
}

#[test]
fn e_b_21_a_termination_signal_gives_the_terminal_back_and_prints_nothing() {
    use std::os::unix::process::ExitStatusExt;
    for (tag, signal) in [("sigterm", libc::SIGTERM), ("sighup", libc::SIGHUP)] {
        let scratch = Scratch::empty("picker", tag);
        seed(&scratch, "echo signal-one", 100, "/w");
        let mut pty = start_picker(&scratch, &[]);
        settle(&pty, "the picker");
        // SAFETY: a plain kill of the child this test started.
        assert_eq!(unsafe { libc::kill(pty.pid(), signal) }, 0);
        let status = pty
            .wait(SETTLE_LIMIT)
            .unwrap_or_else(|| panic!("{tag}: the picker did not exit:\n{}", pty.output()));
        std::thread::sleep(QUIET);

        assert_eq!(status.signal(), Some(signal), "{tag}: died of the signal");
        assert_restored(&pty, &pty.output());
        assert_eq!(
            std::fs::read_to_string(scratch.path("out")).unwrap(),
            "",
            "{tag}: no pick"
        );
    }
}

#[test]
fn it_b_07_the_query_never_reaches_app_log() {
    let scratch = Scratch::empty("picker", "no-query-in-log");
    seed(&scratch, "echo one", 100, "/w");
    let (status, stdout, _) = pick(&scratch, &[b"SENTINELq", ENTER]);
    assert!(status.success());
    assert_eq!(stdout, "", "nothing matches, so Enter prints nothing");
    let log = std::fs::read_to_string(scratch.path("data/app.log")).unwrap();
    assert!(
        log.contains("\"op\":\"cmd_query\""),
        "queries were run: {log}"
    );
    assert!(
        !log.contains("SENTINELq"),
        "the query reached app.log:\n{log}"
    );
}

#[test]
fn e_b_17_an_empty_history_with_recording_off_says_how_to_turn_it_on() {
    let scratch = Scratch::empty("picker", "empty");
    let (status, stdout, terminal) = pick(&scratch, &[ENTER]);
    assert!(status.success());
    assert_eq!(stdout, "");
    // The terminal gets only the cells that changed, so the words of a line
    // arrive with cursor moves between them.
    for word in ["recorded", "yet.", "Recording", "--enable"] {
        assert!(terminal.contains(word), "{word} missing:\n{terminal}");
    }
}

#[test]
fn e_b_02_without_a_terminal_the_picker_fails_and_prints_nothing() {
    let scratch = Scratch::empty("picker", "no-tty");
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("HOME", scratch.path("home"))
        .arg("--data-dir")
        .arg(scratch.path("data"))
        .arg("history")
        .stdin(std::process::Stdio::null());
    // A new session: no controlling terminal, so no /dev/tty.
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty(), "the error is on stderr");
}

// ---------------------------------------------------------------------------
// The Ctrl-R widgets in real shells (section 11)
// ---------------------------------------------------------------------------

/// A shell with the script loaded, on a 30 x 100 pty.
fn start_shell(shell: &str, scratch: &Scratch, mut command: Command) -> Pty {
    command.env("TERM", "xterm-256color");
    let mut pty = Pty::spawn(command, 30, 100).expect("the shell starts");
    std::thread::sleep(Duration::from_millis(500));
    if shell == "fish" {
        // fish 4 asks the terminal for its attributes and, with no answer,
        // waits 2 s before its first prompt (and never again). Keys typed
        // in that time are read in one go, after the prompt, so a key meant
        // for the picker could reach fish first. Wait for the prompt's mark.
        pty.wait_for("133;A", Duration::from_secs(10));
    } else {
        pty.write_line(&format!("source {}", scratch.display("hook")))
            .unwrap();
    }
    settle(&pty, shell);
    pty
}

fn finish(shell: &str, mut pty: Pty) -> String {
    settle(&pty, shell);
    assert!(
        end_with_ctrl_d(&mut pty).is_some(),
        "{shell} did not exit:\n{}",
        pty.output()
    );
    pty.output()
}

/// Wait until `sql` returns `expected` rows (the hook records in the
/// background), for up to 5 s.
fn rows_when(db: &Path, sql: &str, expected: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = query(db, sql);
        if rows.len() >= expected || std::time::Instant::now() >= deadline {
            std::thread::sleep(Duration::from_millis(300));
            return query(db, sql);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Type `beta`, Ctrl-R, Enter in the picker, Enter in the shell: the pick
/// lands on the prompt, runs, is recorded once, and its pick event is linked
/// to that run (same command and shell session, run within 300 s after).
/// Then Ctrl-R and Escape over `echo kept`: the line is left alone.
fn check_widget_picks(shell: &str) {
    if !installed(shell) {
        return;
    }
    let scratch = Scratch::new(shell, "widget-pick");
    let work = scratch.display("work");
    seed(&scratch, "echo alpha", 100, &work);
    seed(&scratch, "echo beta", 200, &work);
    let mut pty = start_shell(
        shell,
        &scratch,
        shell_command(shell, &scratch, OptIn::Marker),
    );

    pty.write(b"beta").unwrap();
    settle(&pty, shell);
    pty.write(CTRL_R).unwrap();
    settle(&pty, "the picker");
    pty.write(ENTER).unwrap();
    settle(&pty, shell);
    pty.write(ENTER).unwrap();
    settle(&pty, shell);

    pty.write(b"echo kept").unwrap();
    settle(&pty, shell);
    pty.write(CTRL_R).unwrap();
    settle(&pty, "the picker");
    pty.write(ESC).unwrap();
    settle(&pty, shell);
    pty.write(ENTER).unwrap();
    let terminal = finish(shell, pty);

    let db = scratch.db();
    let runs = rows_when(
        &db,
        "SELECT c.text, r.started_at, r.shell_session FROM command_runs r \
         JOIN commands c ON c.id = r.command_id \
         WHERE r.shell_session <> 'seed' AND c.text IN ('echo beta', 'echo kept', 'echo beta beta') \
         ORDER BY r.id",
        2,
    );
    let texts: Vec<&str> = runs.iter().map(|r| r.split('|').next().unwrap()).collect();
    assert_eq!(
        texts,
        ["echo beta", "echo kept"],
        "{shell}: the pick ran once, the kept line once\nterminal:\n{terminal}"
    );
    let beta: Vec<&str> = runs[0].split('|').collect();
    let (run_at, run_session): (i64, &str) = (beta[1].parse().unwrap(), beta[2]);

    let picks = query(
        &db,
        "SELECT c.text, e.timestamp, e.shell_session, e.cwd, e.query FROM command_events e \
         JOIN commands c ON c.id = e.command_id WHERE e.action = 'pick'",
    );
    assert_eq!(
        picks.len(),
        1,
        "{shell}: one pick, the Escape logged none: {picks:?}"
    );
    let pick: Vec<&str> = picks[0].split('|').collect();
    assert_eq!(pick[0], "echo beta");
    assert_eq!(
        pick[2], run_session,
        "{shell}: the pick names the shell's session"
    );
    assert_eq!(
        pick[3], work,
        "{shell}: the picker ran in the shell's directory"
    );
    assert_eq!(pick[4], "beta", "{shell}: what was typed became the query");
    let after = run_at - pick[1].parse::<i64>().unwrap();
    assert!(
        (0..=300).contains(&after),
        "{shell}: the run is {after} s after the pick"
    );
    println!("{shell}: Ctrl-R picked, ran once, linked; Escape kept the line");
}

#[test]
fn zsh_ctrl_r_puts_the_pick_on_the_prompt() {
    check_widget_picks("zsh");
}

#[test]
fn bash_ctrl_r_puts_the_pick_on_the_prompt() {
    check_widget_picks("bash");
}

#[test]
fn fish_ctrl_r_puts_the_pick_on_the_prompt() {
    check_widget_picks("fish");
}

/// What Ctrl-R is bound to, as the shell lists it, written to `bound`.
fn show_binding(shell: &str, scratch: &Scratch) -> String {
    let bound = scratch.display("bound");
    match shell {
        "zsh" => format!("bindkey -M emacs '^R' > {bound}; bindkey -M viins '^R' >> {bound}"),
        "bash" => format!("bind -X > {bound}; bind -p | grep 'C-r\"' >> {bound}"),
        "fish" => format!("bind \\cr > {bound} 2>&1; bind -M insert \\cr >> {bound} 2>&1"),
        other => unreachable!("{other}"),
    }
}

/// Bindings a user made before loading psychic: Ctrl-R does something else.
fn own_bindings(shell: &str) -> &'static [&'static str] {
    match shell {
        "zsh" => &["bindkey -M emacs '^R' clear-screen; bindkey -M viins '^R' clear-screen"],
        // A readline function, and the `bind -x` shape fzf and Atuin use.
        "bash" => &[
            "bind '\"\\C-r\": forward-search-history'",
            "my_fzf() { :; }; bind -x '\"\\C-r\": my_fzf'",
        ],
        "fish" => &["bind \\cr 'echo mine'; bind -M insert \\cr 'echo mine'"],
        other => unreachable!("{other}"),
    }
}

/// The Ctrl-R binding once the script is loaded with `env` set and after
/// `user_binding`, the user's own Ctrl-R, when given.
fn binding_after(
    shell: &str,
    tag: &str,
    opt_in: OptIn,
    env: &[(&str, &str)],
    user_binding: Option<&str>,
) -> String {
    let scratch = Scratch::new(shell, tag);
    if let Some(user_binding) = user_binding {
        if shell == "fish" {
            std::fs::write(scratch.path("fish-pre"), user_binding).unwrap();
        } else {
            let hook = std::fs::read_to_string(scratch.path("hook")).unwrap();
            let hook = format!("{}\n{}", user_binding, hook);
            std::fs::write(scratch.path("hook"), hook).unwrap();
        }
    }
    let mut command = shell_command(shell, &scratch, opt_in);
    command.envs(env.iter().copied());
    let mut pty = start_shell(shell, &scratch, command);
    pty.write_line(&show_binding(shell, &scratch)).unwrap();
    let terminal = finish(shell, pty);
    std::fs::read_to_string(scratch.path("bound"))
        .unwrap_or_else(|_| panic!("{shell} listed no binding:\n{terminal}"))
}

fn check_binding_guards(shell: &str) {
    if !installed(shell) {
        return;
    }
    const WIDGET: &str = "__psychic_history_widget";

    let bound = binding_after(shell, "bind-on", OptIn::Marker, &[], None);
    assert!(
        bound.contains(WIDGET),
        "{shell}: bound when recording is on: {bound}"
    );

    let opt_out = [("PSYCHIC_NO_CTRL_R", "1")];
    let bound = binding_after(shell, "bind-opt-out", OptIn::Marker, &opt_out, None);
    assert!(
        !bound.contains(WIDGET),
        "{shell}: PSYCHIC_NO_CTRL_R: {bound}"
    );

    let bound = binding_after(shell, "bind-off", OptIn::Off, &[], None);
    assert!(!bound.contains(WIDGET), "{shell}: recording off: {bound}");

    // The user's own Ctrl-R (fzf, Atuin) is never taken over.
    for (i, own) in own_bindings(shell).iter().enumerate() {
        let tag = format!("bind-taken-{i}");
        let bound = binding_after(shell, &tag, OptIn::Marker, &[], Some(own));
        assert!(!bound.contains(WIDGET), "{shell}: {own:?} stays: {bound}");
    }
    if shell == "fish" {
        // Each mode on its own: a user's insert-mode binding stays, and the
        // default mode, still fish's own, gets the widget.
        let own = "bind -M insert \\cr 'echo mine'";
        let bound = binding_after(shell, "bind-insert-taken", OptIn::Marker, &[], Some(own));
        assert!(
            bound.contains("bind ctrl-r __psychic_history_widget"),
            "{bound}"
        );
        assert!(
            bound.contains("bind -M insert ctrl-r 'echo mine'"),
            "{bound}"
        );
    }
    println!("{shell}: Ctrl-R bound only when it should be");
}

#[test]
fn zsh_ctrl_r_is_bound_only_when_it_should_be() {
    check_binding_guards("zsh");
}

#[test]
fn bash_ctrl_r_is_bound_only_when_it_should_be() {
    check_binding_guards("bash");
}

#[test]
fn fish_ctrl_r_is_bound_only_when_it_should_be() {
    check_binding_guards("fish");
}

#[test]
fn fish_ctrl_r_without_the_binary_is_quiet() {
    if !installed("fish") {
        return;
    }
    let scratch = Scratch::new("fish", "widget-no-binary");
    let missing = scratch.display("no-such-psychic");
    let mut command = shell_command("fish", &scratch, OptIn::Marker);
    command.env("PSYCHIC_BIN", &missing);
    let mut pty = start_shell("fish", &scratch, command);
    pty.write(b"echo still-here").unwrap();
    settle(&pty, "fish");
    pty.write(CTRL_R).unwrap();
    settle(&pty, "fish");
    pty.write(ENTER).unwrap();
    let terminal = finish("fish", pty);
    assert!(
        !terminal.contains(&missing) && !terminal.contains("Unknown command"),
        "fish complained about the missing binary:\n{terminal}"
    );
    assert!(
        terminal.contains("still-here\r\n"),
        "the line ran as typed:\n{terminal}"
    );
}

/// The line goes to the picker verbatim, even starting with `-` and holding
/// quotes and `$` (`--query=`, never `--query "..."`).
fn check_widget_query_verbatim(shell: &str) {
    if !installed(shell) {
        return;
    }
    let scratch = Scratch::new(shell, "widget-verbatim");
    let work = scratch.display("work");
    seed(&scratch, "echo -x '$HOME'", 100, &work);
    let mut pty = start_shell(
        shell,
        &scratch,
        shell_command(shell, &scratch, OptIn::Marker),
    );
    let typed = "-x '$H";
    pty.write(typed.as_bytes()).unwrap();
    settle(&pty, shell);
    pty.write(CTRL_R).unwrap();
    settle(&pty, "the picker");
    pty.write(ENTER).unwrap();
    settle(&pty, shell);
    pty.write(ENTER).unwrap();
    let terminal = finish(shell, pty);
    let picks = query(
        &scratch.db(),
        "SELECT e.query, c.text FROM command_events e JOIN commands c ON c.id = e.command_id \
         WHERE e.action = 'pick'",
    );
    assert_eq!(
        picks,
        [format!("{typed}|echo -x '$HOME'")],
        "{shell}:\n{terminal}"
    );
    assert!(
        terminal.contains("-x $HOME\r\n"),
        "{shell}: the pick ran:\n{terminal}"
    );
}

#[test]
fn zsh_ctrl_r_passes_the_line_verbatim() {
    check_widget_query_verbatim("zsh");
}

#[test]
fn bash_ctrl_r_passes_the_line_verbatim() {
    check_widget_query_verbatim("bash");
}

#[test]
fn fish_ctrl_r_passes_the_line_verbatim() {
    check_widget_query_verbatim("fish");
}

/// E-B-15: the picker fails under the widget; the line stays as typed.
fn check_widget_survives_a_failing_picker(shell: &str) {
    if !installed(shell) {
        return;
    }
    let scratch = Scratch::new(shell, "widget-fails");
    let mut command = shell_command(shell, &scratch, OptIn::Marker);
    command.env("PSYCHIC_BIN", "false");
    let mut pty = start_shell(shell, &scratch, command);
    pty.write(b"echo still-here").unwrap();
    settle(&pty, shell);
    pty.write(CTRL_R).unwrap();
    settle(&pty, shell);
    pty.write(ENTER).unwrap();
    let terminal = finish(shell, pty);
    assert!(
        terminal.contains("still-here\r\n"),
        "{shell}: the line ran as typed:\n{terminal}"
    );
}

#[test]
fn zsh_ctrl_r_leaves_the_line_alone_when_the_picker_fails() {
    check_widget_survives_a_failing_picker("zsh");
}

#[test]
fn bash_ctrl_r_leaves_the_line_alone_when_the_picker_fails() {
    check_widget_survives_a_failing_picker("bash");
}

#[test]
fn fish_ctrl_r_leaves_the_line_alone_when_the_picker_fails() {
    check_widget_survives_a_failing_picker("fish");
}

#[test]
fn it_b_06_every_script_ships_the_widget_with_its_guards() {
    let scratch = Scratch::empty("picker", "scripts");
    for shell in ["zsh", "bash", "fish"] {
        let script = String::from_utf8(scratch.psychic(&[shell])).unwrap();
        for needed in [
            "__psychic_history_widget",
            "PSYCHIC_NO_CTRL_R",
            "__psychic_history_enabled",
            "--query=",
            "PSYCHIC_CTRL_R",
            "PSYCHIC_NO_PH",
            "__psychic_history_typed",
        ] {
            assert!(script.contains(needed), "{shell} lacks {needed}");
        }
        assert!(
            !script.contains("--query \""),
            "{shell} passes the query as a separate word"
        );
    }
}

// ---------------------------------------------------------------------------
// One Enter in the picker, one at the prompt (IT-B-16 to IT-B-18)
// ---------------------------------------------------------------------------

/// The prompt every shell here is given, to find it on the screen.
const PROMPT: &str = "PROMPT> ";
const UP: &[u8] = b"\x1b[A";

/// A shell set up the way a user's startup file might: `before` runs before
/// the psychic script is loaded, `after` after it.
struct Setup {
    name: &'static str,
    before: &'static [&'static str],
    after: &'static [&'static str],
}

/// The set-ups each shell is tried in: plain, vi editing turned on after the
/// script (as a later line of the startup file would), and for bash the
/// prompt of the report that started this: a two-line `PS1` and another
/// tool's `PROMPT_COMMAND` entry.
fn setups(shell: &str) -> Vec<Setup> {
    match shell {
        "bash" => vec![
            Setup {
                name: "plain",
                before: &["PS1='PROMPT> '"],
                after: &[],
            },
            Setup {
                name: "two-line",
                before: &[
                    "PS1='top line\\nPROMPT> '",
                    "_other_hook() { local ret=$?; return $ret; }",
                    "PROMPT_COMMAND='_other_hook;'",
                ],
                after: &[],
            },
            Setup {
                name: "vi",
                before: &["PS1='PROMPT> '"],
                after: &["set -o vi"],
            },
        ],
        "zsh" => vec![
            Setup {
                name: "plain",
                before: &["PROMPT='PROMPT> '"],
                after: &[],
            },
            Setup {
                name: "two-line",
                before: &["PROMPT=$'top line\\nPROMPT> '"],
                after: &[],
            },
            Setup {
                name: "vi",
                before: &["PROMPT='PROMPT> '"],
                after: &["bindkey -v"],
            },
        ],
        "fish" => vec![
            Setup {
                name: "plain",
                before: &["function fish_prompt; printf 'PROMPT> '; end"],
                after: &[],
            },
            Setup {
                name: "vi",
                before: &["function fish_prompt; printf 'PROMPT> '; end"],
                after: &["fish_vi_key_bindings"],
            },
        ],
        other => unreachable!("{other}"),
    }
}

/// A scratch dir for `shell` in `setup`, recording on, with one command
/// recorded whose run leaves a file behind: returns it and that command.
fn marker_scratch(shell: &str, setup: &Setup, tag: &str) -> (Scratch, String) {
    let scratch = Scratch::new(shell, &format!("{tag}-{}", setup.name));
    let command = format!("touch {}", scratch.display("marker"));
    seed(&scratch, &command, 100, &scratch.display("work"));
    let join = |lines: &[&str]| lines.iter().map(|l| format!("{l}\n")).collect::<String>();
    if shell == "fish" {
        if !setup.before.is_empty() {
            std::fs::write(scratch.path("fish-pre"), join(setup.before)).unwrap();
        }
        if !setup.after.is_empty() {
            std::fs::write(scratch.path("fish-init"), join(setup.after)).unwrap();
        }
    } else {
        let hook = std::fs::read_to_string(scratch.path("hook")).unwrap();
        let hook = format!("{}{}\n{}", join(setup.before), hook, join(setup.after));
        std::fs::write(scratch.path("hook"), hook).unwrap();
    }
    (scratch, command)
}

/// Remove the terminal's control sequences (CSI, OSC, DCS and two-byte
/// escapes), keeping the text a reader would see.
fn visible(raw: &str) -> String {
    let mut text = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            text.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']' | 'P') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    text
}

/// After the picker gave the terminal back: the prompt was drawn again and
/// `command` is on it, not printed as output, and has not run.
fn assert_on_prompt(shell: &str, setup: &Setup, scratch: &Scratch, pty: &Pty, command: &str) {
    let terminal = pty.output();
    let left = terminal
        .rfind("\x1b[?1049l")
        .unwrap_or_else(|| panic!("{shell}: the picker never left:\n{terminal:?}"));
    // A long line wraps, and shells move the cursor back with `\r`: the
    // prompt and the start of the pick are enough.
    let after = visible(&terminal[left..]).replace('\r', "");
    let start = command.split(' ').next().unwrap();
    assert!(
        after.contains(&format!("{PROMPT}{start} ")),
        "{shell} {}: the prompt, then the pick on it; after the picker:\n{after:?}",
        setup.name
    );
    assert!(
        !scratch.path("marker").exists(),
        "{shell} {}: the pick ran before the second Enter:\n{after:?}",
        setup.name
    );
}

/// Enter at the shell ran the pick: its file appears (within 5 s).
fn assert_ran(shell: &str, setup: &Setup, scratch: &Scratch, pty: &Pty) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !scratch.path("marker").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        scratch.path("marker").exists(),
        "{shell} {}: Enter at the prompt did not run the pick:\n{}",
        setup.name,
        visible(&pty.output())
    );
}

/// The runs the shell recorded (not the seed's), as text.
fn recorded_runs(scratch: &Scratch, expected: usize) -> Vec<String> {
    rows_when(
        &scratch.db(),
        "SELECT c.text FROM command_runs r JOIN commands c ON c.id = r.command_id \
         WHERE r.shell_session <> 'seed' ORDER BY r.id",
        expected,
    )
}

/// IT-B-16: Ctrl-R, Enter in the picker: the pick is on a freshly drawn
/// prompt and has not run; Enter at the prompt runs it.
fn check_widget_runs_on_the_second_enter(shell: &str) {
    if !installed(shell) {
        return;
    }
    for setup in setups(shell) {
        let (scratch, command) = marker_scratch(shell, &setup, "second-enter");
        let mut pty = start_shell(
            shell,
            &scratch,
            shell_command(shell, &scratch, OptIn::Marker),
        );
        pty.write(CTRL_R).unwrap();
        settle(&pty, "the picker");
        pty.write(ENTER).unwrap();
        settle(&pty, shell);
        assert_on_prompt(shell, &setup, &scratch, &pty, &command);
        pty.write(ENTER).unwrap();
        assert_ran(shell, &setup, &scratch, &pty);
        let terminal = finish(shell, pty);
        assert_eq!(
            recorded_runs(&scratch, 1),
            [command.as_str()],
            "{shell} {}: the pick ran once\n{}",
            setup.name,
            visible(&terminal)
        );
        println!("{shell} {}: one Enter picks, the next runs", setup.name);
    }
}

#[test]
fn zsh_ctrl_r_runs_the_pick_only_on_the_second_enter() {
    check_widget_runs_on_the_second_enter("zsh");
}

#[test]
fn bash_ctrl_r_runs_the_pick_only_on_the_second_enter() {
    check_widget_runs_on_the_second_enter("bash");
}

#[test]
fn fish_ctrl_r_runs_the_pick_only_on_the_second_enter() {
    check_widget_runs_on_the_second_enter("fish");
}

/// IT-B-17: `ph` typed at the prompt, Enter in the picker: zsh and fish put
/// the pick on the next prompt; bash puts it in the history list and says to
/// press Up. Nothing runs until Enter at the prompt; the run is recorded
/// once, `ph` itself never, and the pick names the shell's session.
fn check_typed_function(shell: &str) {
    if !installed(shell) {
        return;
    }
    let setup = &setups(shell)[0];
    let (scratch, command) = marker_scratch(shell, setup, "typed");
    let mut pty = start_shell(
        shell,
        &scratch,
        shell_command(shell, &scratch, OptIn::Marker),
    );
    pty.write_line("ph touch").unwrap();
    settle(&pty, "the picker");
    pty.write(ENTER).unwrap();
    settle(&pty, shell);
    if shell == "bash" {
        let terminal = visible(&pty.output());
        assert!(
            terminal.contains("psychic: press Up to get the command on your prompt"),
            "bash says how to get the pick:\n{terminal}"
        );
        assert!(!scratch.path("marker").exists(), "bash ran the pick");
        pty.write(UP).unwrap();
        settle(&pty, shell);
        let after_up = visible(&pty.output()).replace('\r', "");
        let hint = after_up.rfind("press Up").unwrap();
        assert!(
            after_up[hint..].contains(&format!("{PROMPT}touch ")),
            "Up brought the pick back:\n{after_up}"
        );
    } else {
        assert_on_prompt(shell, setup, &scratch, &pty, &command);
    }
    assert!(!scratch.path("marker").exists(), "{shell}: the pick ran");
    pty.write(ENTER).unwrap();
    assert_ran(shell, setup, &scratch, &pty);
    let terminal = finish(shell, pty);

    let runs = recorded_runs(&scratch, 1);
    assert_eq!(
        runs,
        [command.as_str()],
        "{shell}: the pick ran once and `ph` is not recorded\n{}",
        visible(&terminal)
    );
    let picks = query(
        &scratch.db(),
        "SELECT e.query, e.shell_session = (SELECT shell_session FROM command_runs \
         WHERE shell_session <> 'seed') FROM command_events e WHERE e.action = 'pick'",
    );
    assert_eq!(
        picks,
        ["touch|1"],
        "{shell}: one pick, with the words as its query, in the shell's session"
    );
    println!("{shell}: ph puts the pick where one Enter runs it");
}

#[test]
fn zsh_ph_puts_the_pick_on_the_next_prompt() {
    check_typed_function("zsh");
}

#[test]
fn bash_ph_puts_the_pick_in_the_history_list() {
    check_typed_function("bash");
}

#[test]
fn fish_ph_puts_the_pick_on_the_next_prompt() {
    check_typed_function("fish");
}

/// IT-B-18: `ph` is never defined over the user's own `ph`, nor with
/// PSYCHIC_NO_PH; PSYCHIC_CTRL_R says what became of Ctrl-R.
fn check_ph_guard_and_ctrl_r_state(shell: &str) {
    if !installed(shell) {
        return;
    }
    // What `ph` is, with its body, then the state; and the user's own `ph`.
    // And Ctrl-R taken in every keymap or mode the script would bind.
    let (show, own, taken) = match shell {
        "zsh" => (
            "{ which ph; echo state=$PSYCHIC_CTRL_R; } > ",
            "alias ph='echo mine'",
            own_bindings(shell)[0],
        ),
        "bash" => (
            "{ type ph; echo state=$PSYCHIC_CTRL_R; } > ",
            "alias ph='echo mine'",
            "for m in emacs vi-insert vi-command; do \
             bind -m $m '\"\\C-r\": forward-search-history'; done",
        ),
        "fish" => (
            "begin; type ph; echo state=$PSYCHIC_CTRL_R; end 2>&1 > ",
            "function ph; echo mine; end",
            own_bindings(shell)[0],
        ),
        other => unreachable!("{other}"),
    };
    /// Whose `ph` the shell has.
    #[derive(Debug, PartialEq)]
    enum Ph {
        Ours,
        Users,
        None,
    }
    /// One start of the shell, and what it should end up with.
    struct Case<'a> {
        tag: &'a str,
        opt_in: OptIn,
        env: &'a [(&'a str, &'a str)],
        /// A line of the user's, run before the script.
        user_line: Option<&'a str>,
        ph: Ph,
        /// The expected `PSYCHIC_CTRL_R`.
        state: &'a str,
    }
    let cases = [
        Case {
            tag: "state-on",
            opt_in: OptIn::Marker,
            env: &[],
            user_line: None,
            ph: Ph::Ours,
            state: "bound",
        },
        Case {
            tag: "state-own",
            opt_in: OptIn::Marker,
            env: &[],
            user_line: Some(own),
            ph: Ph::Users,
            state: "bound",
        },
        Case {
            tag: "state-no-ph",
            opt_in: OptIn::Marker,
            env: &[("PSYCHIC_NO_PH", "1"), ("PSYCHIC_NO_CTRL_R", "1")],
            user_line: None,
            ph: Ph::None,
            state: "opted-out",
        },
        Case {
            tag: "state-off",
            opt_in: OptIn::Off,
            env: &[],
            user_line: None,
            ph: Ph::Ours,
            state: "recording-off",
        },
        Case {
            tag: "state-taken",
            opt_in: OptIn::Marker,
            env: &[],
            user_line: Some(taken),
            ph: Ph::Ours,
            state: "taken",
        },
    ];
    for Case {
        tag,
        opt_in,
        env,
        user_line,
        ph,
        state,
    } in cases
    {
        let scratch = Scratch::new(shell, tag);
        if let Some(user_line) = user_line {
            if shell == "fish" {
                std::fs::write(scratch.path("fish-pre"), user_line).unwrap();
            } else {
                let hook = std::fs::read_to_string(scratch.path("hook")).unwrap();
                std::fs::write(scratch.path("hook"), format!("{user_line}\n{hook}")).unwrap();
            }
        }
        let mut command = shell_command(shell, &scratch, opt_in);
        command.envs(env.iter().copied());
        let mut pty = start_shell(shell, &scratch, command);
        pty.write_line(&format!("{show}{}", scratch.display("state")))
            .unwrap();
        let terminal = finish(shell, pty);
        let shown = std::fs::read_to_string(scratch.path("state"))
            .unwrap_or_else(|_| panic!("{shell} {tag}: nothing shown:\n{terminal}"));
        let found = match (
            shown.contains("__psychic_history_typed"),
            shown.contains("mine"),
        ) {
            (true, false) => Ph::Ours,
            (false, true) => Ph::Users,
            (false, false) => Ph::None,
            (true, true) => panic!("{shell} {tag}: both: {shown}"),
        };
        assert_eq!(found, ph, "{shell} {tag}: {shown}");
        assert!(
            shown.contains(&format!("state={state}\n")),
            "{shell} {tag}: {shown}"
        );
    }
    println!("{shell}: ph and PSYCHIC_CTRL_R as expected");
}

#[test]
fn zsh_ph_and_ctrl_r_state_respect_the_user() {
    check_ph_guard_and_ctrl_r_state("zsh");
}

#[test]
fn bash_ph_and_ctrl_r_state_respect_the_user() {
    check_ph_guard_and_ctrl_r_state("bash");
}

#[test]
fn fish_ph_and_ctrl_r_state_respect_the_user() {
    check_ph_guard_and_ctrl_r_state("fish");
}
