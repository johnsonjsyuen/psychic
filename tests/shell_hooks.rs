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
//!
//! The hook's `track-command` calls run in the background, so runs that land
//! in the same second reach the database in no particular order: every
//! comparison here is of sorted rows.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};
use support::{OptIn, Scratch, Step, installed, line, load_hook, play, shell_command};

/// One recorded run, as the assertions want it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    text: String,
    cwd: String,
    exit_code: Option<i64>,
    source: String,
    template: String,
    shell_session: String,
}

impl std::fmt::Debug for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} in {} exit {:?} ({})",
            self.text, self.cwd, self.exit_code, self.source
        )
    }
}

fn count_runs(db: &Path) -> Option<usize> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.query_row("SELECT COUNT(*) FROM command_runs", [], |row| {
        row.get::<_, i64>(0)
    })
    .ok()
    .map(|count| count as usize)
}

/// Every run, sorted, once `expected` runs are there (the hook's calls run in
/// the background and may finish after the shell) or 5 s have passed.
fn runs(db: &Path, expected: usize) -> Vec<Row> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while count_runs(db).unwrap_or(0) < expected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    // A moment more, so a run beyond the expected ones would show up.
    std::thread::sleep(Duration::from_millis(500));
    if count_runs(db).is_none() {
        return Vec::new();
    }
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = conn
        .prepare(
            "SELECT c.text, r.cwd, r.exit_code, r.source, c.template, r.shell_session
             FROM command_runs r JOIN commands c ON c.id = r.command_id",
        )
        .unwrap();
    let mut rows: Vec<Row> = statement
        .query_map([], |row| {
            Ok(Row {
                text: row.get(0)?,
                cwd: row.get(1)?,
                exit_code: row.get(2)?,
                source: row.get(3)?,
                template: row.get(4)?,
                shell_session: row.get(5)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    rows.sort();
    rows
}

/// `(text, exit code)` of every run, sorted.
fn texts_and_codes(rows: &[Row]) -> Vec<(&str, Option<i64>)> {
    let mut pairs: Vec<(&str, Option<i64>)> = rows
        .iter()
        .map(|row| (row.text.as_str(), row.exit_code))
        .collect();
    pairs.sort();
    pairs
}

// ---------------------------------------------------------------------------
// The same session in all three shells (section 15.3)
// ---------------------------------------------------------------------------

fn standard_steps(shell: &str, scratch: &Scratch) -> Vec<Step> {
    let mut steps = Vec::new();
    if shell == "zsh" {
        steps.push(line("setopt histignorespace"));
        steps.push(line("HISTORY_IGNORE='secret*'"));
    }
    steps.extend(load_hook(shell, scratch));
    for text in [
        "true",
        "false",
        "cd sub",
        "echo in-sub",
        " echo leading-space",
    ] {
        steps.push(line(text));
    }
    if shell != "fish" {
        steps.push(line("secret-thing"));
    }
    steps.push(line("echo 'multi"));
    steps.push(line("line'"));
    steps
}

fn check_shell(shell: &str, opt_in: OptIn) {
    if !installed(shell) {
        return;
    }
    let tag = match opt_in {
        OptIn::Variable => "standard",
        OptIn::Marker => "marker",
        OptIn::Off => unreachable!("nothing is recorded with recording off"),
    };
    let scratch = Scratch::new(shell, tag);
    let terminal = play(
        &scratch,
        shell_command(shell, &scratch, opt_in),
        &standard_steps(shell, &scratch),
        Duration::from_millis(150),
    );
    let rows = runs(&scratch.db(), 5);
    assert_eq!(
        rows.len(),
        5,
        "{shell} recorded {rows:#?}\nterminal:\n{terminal}"
    );

    // The multi-line command: zsh and bash keep the newline, the text may
    // differ in how; everything else compares exactly.
    let (multi, simple): (Vec<&Row>, Vec<&Row>) = rows
        .iter()
        .partition(|row| row.text.starts_with("echo 'multi"));
    let work = scratch.display("work");
    let sub = scratch.display("work/sub");
    let mut simple: Vec<(&str, &str, Option<i64>, &str)> = simple
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
    simple.sort();
    let mut expected = vec![
        ("true", work.as_str(), Some(0), "hook"),
        ("false", work.as_str(), Some(1), "hook"),
        ("cd sub", work.as_str(), Some(0), "hook"),
        ("echo in-sub", sub.as_str(), Some(0), "hook"),
    ];
    expected.sort();
    assert_eq!(simple, expected, "{shell}");
    assert_eq!(multi.len(), 1, "{shell}: {rows:#?}");
    let multi = multi[0];
    assert!(
        (multi.text.contains('\n') || multi.text.contains(';')) && multi.text.ends_with("line'"),
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
        "{shell} recorded an opted-out command: {rows:#?}"
    );
    assert!(
        rows.iter()
            .all(|row| row.shell_session == rows[0].shell_session),
        "{shell}: one session"
    );
    let cd = rows.iter().find(|row| row.text == "cd sub").unwrap();
    assert_eq!(cd.template, "cd PATH");
    println!("{shell}: {} runs recorded as expected", rows.len());
}

#[test]
fn zsh_records_what_the_user_ran() {
    check_shell("zsh", OptIn::Variable);
}

#[test]
fn bash_records_what_the_user_ran() {
    check_shell("bash", OptIn::Variable);
}

#[test]
fn fish_records_what_the_user_ran() {
    check_shell("fish", OptIn::Variable);
}

#[test]
fn zsh_records_once_history_is_enabled_by_the_marker_file() {
    check_shell("zsh", OptIn::Marker);
}

#[test]
fn bash_records_once_history_is_enabled_by_the_marker_file() {
    check_shell("bash", OptIn::Marker);
}

#[test]
fn fish_records_once_history_is_enabled_by_the_marker_file() {
    check_shell("fish", OptIn::Marker);
}

// ---------------------------------------------------------------------------
// bash: only what this shell ran (section 11.3)
// ---------------------------------------------------------------------------

/// A bash session whose PROMPT_COMMAND also runs `sync` (set after the hook
/// is loaded, as a later line of ~/.bashrc would), while another shell
/// appends to the same HISTFILE. Nothing but this shell's own commands may be
/// recorded: not the other shell's lines, not an ignored command, nothing for
/// an empty line or Ctrl-C, and nothing twice.
fn check_bash_shared_histfile(tag: &str, sync: &str) {
    if !installed("bash") {
        return;
    }
    let scratch = Scratch::new("bash", tag);
    let set_prompt_command = format!("PROMPT_COMMAND=\"$PROMPT_COMMAND; {sync}\"");
    let mut steps = load_hook("bash", &scratch);
    steps.extend([
        line(&set_prompt_command),
        line("echo one"),
        Step::OtherShellWrites("echo from-another-shell"),
        line(""),
        line(" echo ignored"),
        Step::OtherShellWrites("echo from-another-shell-2"),
        line(""),
        Step::CtrlC,
        line("false"),
        line("echo two"),
    ]);
    let terminal = play(
        &scratch,
        shell_command("bash", &scratch, OptIn::Variable),
        &steps,
        Duration::from_millis(300),
    );
    let rows = runs(&scratch.db(), 4);
    assert_eq!(
        texts_and_codes(&rows),
        vec![
            (set_prompt_command.as_str(), Some(0)),
            ("echo one", Some(0)),
            ("echo two", Some(0)),
            ("false", Some(1)),
        ],
        "terminal:\n{terminal}"
    );
}

#[test]
fn bash_with_history_append_and_reload_records_only_its_own_commands() {
    check_bash_shared_histfile("append-read-new", "history -a; history -n");
}

#[test]
fn bash_with_history_clear_and_reread_records_only_its_own_commands() {
    check_bash_shared_histfile("append-clear-reread", "history -a; history -c; history -r");
}

#[test]
fn bash_with_history_reload_put_before_the_hook_records_only_its_own_commands() {
    if !installed("bash") {
        return;
    }
    let scratch = Scratch::new("bash", "prepended");
    let mut steps = load_hook("bash", &scratch);
    steps.extend([
        // Runs before the hook from the next prompt; the hook moves back to
        // the front, and records nothing at the prompt where it found itself
        // second (this line's own).
        line("PROMPT_COMMAND=\"history -a; history -n; $PROMPT_COMMAND\""),
        line("echo one"),
        Step::OtherShellWrites("echo from-another-shell"),
        line(" echo ignored"),
        Step::OtherShellWrites("echo from-another-shell-2"),
        line("echo two"),
        line("echo three"),
    ]);
    let terminal = play(
        &scratch,
        shell_command("bash", &scratch, OptIn::Variable),
        &steps,
        Duration::from_millis(300),
    );
    let rows = runs(&scratch.db(), 3);
    assert_eq!(
        texts_and_codes(&rows),
        vec![
            ("echo one", Some(0)),
            ("echo three", Some(0)),
            ("echo two", Some(0)),
        ],
        "terminal:\n{terminal}"
    );
}

/// What bash-preexec does to PROMPT_COMMAND at every prompt (re-pins
/// `__bp_precmd_invoke_cmd` first and `__bp_interactive_mode` last, as an
/// array element on bash 5.1+), without its DEBUG trap.
const BASH_PREEXEC_STAND_IN: &str = r#"__bp_interactive_mode() { :; }
__bp_install_prompt_command() {
    local pc="${PROMPT_COMMAND[0]//__bp_precmd_invoke_cmd$'\n'/}" i
    for i in "${!PROMPT_COMMAND[@]}"; do
        [[ ${PROMPT_COMMAND[i]} == __bp_interactive_mode ]] && unset 'PROMPT_COMMAND[i]'
    done
    PROMPT_COMMAND[0]=$'__bp_precmd_invoke_cmd\n'"$pc"
    PROMPT_COMMAND+=(__bp_interactive_mode)
}
__bp_precmd_invoke_cmd() { local s=$?; __bp_install_prompt_command; return $s; }
__bp_install_prompt_command
"#;

#[test]
fn bash_next_to_bash_preexec_records_every_command() {
    if !installed("bash") {
        return;
    }
    let scratch = Scratch::new("bash", "preexec");
    std::fs::write(scratch.path("bp"), BASH_PREEXEC_STAND_IN).unwrap();
    let source_bp = format!("source {}", scratch.display("bp"));
    let mut steps = load_hook("bash", &scratch);
    steps.extend([
        line(&source_bp),
        line("PROMPT_COMMAND=\"$PROMPT_COMMAND; history -a; history -n\""),
        line("echo one"),
        Step::OtherShellWrites("echo from-another-shell"),
        line(" echo ignored"),
        line("false"),
        line("echo two"),
    ]);
    let terminal = play(
        &scratch,
        shell_command("bash", &scratch, OptIn::Variable),
        &steps,
        Duration::from_millis(300),
    );
    let rows = runs(&scratch.db(), 5);
    assert_eq!(
        texts_and_codes(&rows),
        vec![
            (
                "PROMPT_COMMAND=\"$PROMPT_COMMAND; history -a; history -n\"",
                Some(0)
            ),
            ("echo one", Some(0)),
            ("echo two", Some(0)),
            ("false", Some(1)),
            (source_bp.as_str(), Some(0)),
        ],
        "terminal:\n{terminal}"
    );
}

#[test]
fn bash_with_ignoredups_records_an_immediate_repeat_once() {
    if !installed("bash") {
        return;
    }
    let scratch = Scratch::new("bash", "ignoredups");
    let mut command = shell_command("bash", &scratch, OptIn::Variable);
    command.env("HISTCONTROL", "ignoredups");
    let mut steps = load_hook("bash", &scratch);
    steps.extend([line("echo same"), line("echo same"), line("echo other")]);
    let terminal = play(&scratch, command, &steps, Duration::from_millis(300));
    let rows = runs(&scratch.db(), 2);
    // bash adds the repeat to no history, so it is not recorded (section 11.3).
    assert_eq!(
        texts_and_codes(&rows),
        vec![("echo other", Some(0)), ("echo same", Some(0))],
        "terminal:\n{terminal}"
    );
}

#[test]
fn bash_prompt_command_entries_after_the_hooks_see_the_commands_status() {
    if !installed("bash") {
        return;
    }
    let scratch = Scratch::new("bash", "status");
    let seen = scratch.display("seen");
    let mut steps = load_hook("bash", &scratch);
    steps.extend([
        Step::Line(format!(
            "PROMPT_COMMAND=\"$PROMPT_COMMAND; echo \\$? >> {seen}\""
        )),
        line("false"),
        line("true"),
        line("(exit 3)"),
    ]);
    let terminal = play(
        &scratch,
        shell_command("bash", &scratch, OptIn::Variable),
        &steps,
        Duration::from_millis(300),
    );
    let statuses = std::fs::read_to_string(scratch.path("seen")).unwrap_or_default();
    let statuses: Vec<&str> = statuses.lines().collect();
    assert!(
        statuses.ends_with(&["1", "0", "3"]),
        "a later PROMPT_COMMAND entry saw {statuses:?}\nterminal:\n{terminal}"
    );
}

// ---------------------------------------------------------------------------
// What the shell keeps out of its history (section 10.3)
// ---------------------------------------------------------------------------

#[test]
fn zsh_does_not_record_what_hist_no_store_keeps_out_of_history() {
    if !installed("zsh") {
        return;
    }
    let scratch = Scratch::new("zsh", "nostore");
    let mut steps = vec![line("setopt histnostore"), line("alias h=history")];
    steps.extend(load_hook("zsh", &scratch));
    steps.extend([
        line("history"),
        line("fc -rl"),
        line("builtin history 1"),
        // zsh judges the line with aliases expanded.
        line("h >/dev/null"),
        line("fc -W /dev/null"),
        line("echo history"),
        // `history` directly followed by `;` is kept by zsh.
        line("history;echo y"),
    ]);
    let terminal = play(
        &scratch,
        shell_command("zsh", &scratch, OptIn::Variable),
        &steps,
        Duration::from_millis(150),
    );
    let rows = runs(&scratch.db(), 3);
    assert_eq!(
        texts_and_codes(&rows),
        vec![
            ("echo history", Some(0)),
            ("fc -W /dev/null", Some(0)),
            ("history;echo y", Some(0)),
        ],
        "terminal:\n{terminal}"
    );
}

#[test]
fn fish_does_not_record_what_fish_should_add_to_history_rejects() {
    if !installed("fish") {
        return;
    }
    let scratch = Scratch::new("fish", "should-add");
    std::fs::write(
        scratch.path("fish-init"),
        "function fish_should_add_to_history
    string match -q -- 'secret*' $argv[1]; and return 1
    return 0
end
",
    )
    .unwrap();
    let steps = [
        line("echo kept"),
        line("secret-thing"),
        // fish_should_add_to_history decides alone: a leading space is kept.
        line(" echo spaced"),
    ];
    let terminal = play(
        &scratch,
        shell_command("fish", &scratch, OptIn::Variable),
        &steps,
        Duration::from_millis(150),
    );
    let rows = runs(&scratch.db(), 2);
    assert_eq!(
        texts_and_codes(&rows),
        vec![("echo kept", Some(0)), ("echo spaced", Some(0))],
        "terminal:\n{terminal}"
    );
}

// ---------------------------------------------------------------------------
// fish without the binary (E-R-01)
// ---------------------------------------------------------------------------

#[test]
fn fish_without_the_binary_prints_nothing_at_the_prompt() {
    if !installed("fish") {
        return;
    }
    let scratch = Scratch::new("fish", "no-binary");
    let missing = scratch.display("no-such-psychic");
    let mut command = shell_command("fish", &scratch, OptIn::Variable);
    command.env("PSYCHIC_BIN", &missing);
    let terminal = play(
        &scratch,
        command,
        &[line("echo first"), line("echo second")],
        Duration::from_millis(300),
    );
    assert!(
        !terminal.contains(&missing) && !terminal.contains("__psychic_postexec"),
        "fish complained about the missing binary:\n{terminal}"
    );
    assert!(terminal.contains("second"), "the commands ran:\n{terminal}");
}
