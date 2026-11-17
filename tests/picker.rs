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
        ] {
            assert!(script.contains(needed), "{shell} lacks {needed}");
        }
        assert!(
            !script.contains("--query \""),
            "{shell} passes the query as a separate word"
        );
    }
}
