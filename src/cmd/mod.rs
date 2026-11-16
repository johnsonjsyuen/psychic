//! Shell history search (Ctrl-R): recording, and the `psychic history` actions.
//!
//! The specification is `docs/ctrl-r/` (start at its README); this module is
//! Phase A, "Record": the command tables (`store`), `psychic track-command`
//! (`track`), opt-in and privacy (`privacy`), templating (`template`), the
//! repository root (`repo`) and the shell history import (`backfill`). Phase
//! D0 adds the benchmark corpora: `psychic internal bench-history` (`bench`)
//! and the synthetic and agent-transcript corpus builders (`corpus`).
//!
//! Identifiers say `cmd`/`command`, never `history`: `src/history.rs` and
//! `history_mode` already mean directory back/forward (ADR-1).
//!
//! Nothing here formats a command's text into a log line or an error message
//! (`docs/ctrl-r/01-record.md` section 10.6), and nothing here uses `eprintln!`.

mod backfill;
pub mod bench;
pub mod cli;
mod corpus;
mod privacy;
mod repo;
pub mod store;
mod template;
mod track;
mod tuning;

use anyhow::{Context, Result, bail};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use store::CmdStore;

/// `psychic track-command`. Called from `main` before the logger is set up.
pub fn run_track_command(data_dir: Option<&Path>, args: &cli::TrackCommandArgs) -> Result<()> {
    let ignore = std::env::var("PSYCHIC_HISTORY_IGNORE").ok();
    track::run(
        data_dir,
        args,
        jiff::Timestamp::now().as_second(),
        ignore.as_deref(),
    )
}

/// `psychic history`: one action on what is recorded, or (Phase B) the picker.
pub fn run_history(data_dir: &Path, args: cli::HistoryArgs) -> Result<()> {
    let now = jiff::Timestamp::now().as_second();
    let env = Env::from_process();
    let mut out = std::io::stdout().lock();
    if args.enable {
        enable(data_dir, !args.no_import, now, &env, &mut out)
    } else if args.disable {
        disable(data_dir, &mut out)
    } else if args.status {
        status(data_dir, &env, &mut out)
    } else if args.import {
        let store = CmdStore::open(data_dir)?;
        let report = import_history(&store, now, &env)?;
        writeln!(out, "{}", report.summary())?;
        Ok(())
    } else if args.purge {
        if !args.yes && !std::io::stdin().is_terminal() {
            bail!("stdin is not a terminal, so psychic can't ask before deleting; pass --yes");
        }
        let mut confirm = |question: &str| -> Result<bool> {
            if args.yes {
                return Ok(true);
            }
            print!("{}", question);
            std::io::stdout().flush()?;
            let mut answer = String::new();
            std::io::stdin().lock().read_line(&mut answer)?;
            Ok(matches!(answer.trim(), "y" | "Y"))
        };
        purge(data_dir, &mut confirm, &mut out)
    } else if args.retrain {
        bail!("psychic history: not built yet (Phase C)")
    } else {
        bail!("psychic history: not built yet (Phase B)")
    }
}

/// `psychic internal bench-history`. Refuses to run unless `--data-dir` was
/// given and is not the real data directory: benchmarks never write there.
pub fn run_bench(data_dir: &Path, data_dir_was_given: bool, args: bench::BenchArgs) -> Result<()> {
    let real = crate::cli::get_default_data_dir().ok();
    if !data_dir_was_given || real.is_some_and(|real| same_dir(data_dir, &real)) {
        bail!("bench-history needs an explicit --data-dir that is not your real data directory");
    }
    let env = bench::BenchEnv {
        now: jiff::Timestamp::now().as_second(),
        home: std::env::var_os("HOME").map(PathBuf::from),
        claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
    };
    bench::run(data_dir, args.command, &env, &mut std::io::stdout().lock())
}

/// Whether two paths name one directory: compared canonicalised when both
/// exist, else as absolute paths.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => match (std::path::absolute(a), std::path::absolute(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        },
    }
}

/// The environment variables the actions read, gathered once so the actions
/// themselves can be tested without touching the process environment.
struct Env {
    home: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    zdotdir: Option<PathBuf>,
    /// `PSYCHIC_HISTORY`.
    history: Option<String>,
    /// `PSYCHIC_HISTORY_IGNORE`.
    ignore: Option<String>,
}

impl Env {
    fn from_process() -> Self {
        Env {
            home: std::env::var_os("HOME").map(PathBuf::from),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            zdotdir: std::env::var_os("ZDOTDIR").map(PathBuf::from),
            history: std::env::var("PSYCHIC_HISTORY").ok(),
            ignore: std::env::var("PSYCHIC_HISTORY_IGNORE").ok(),
        }
    }

    fn home(&self) -> Result<&Path> {
        self.home
            .as_deref()
            .context("HOME is not set, so psychic can't find your shell history")
    }
}

/// Run the shell history import (section 12) with the files `env` points at.
fn import_history(store: &CmdStore, now: i64, env: &Env) -> Result<backfill::BackfillReport> {
    let home = env.home()?;
    let locations =
        backfill::Locations::discover(home, env.xdg_data_home.as_deref(), env.zdotdir.as_deref());
    backfill::run(store, &locations, home, now, env.ignore.as_deref())
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// `--enable`: section 13.1.
fn enable(data_dir: &Path, import: bool, now: i64, env: &Env, out: &mut dyn Write) -> Result<()> {
    let store = CmdStore::open(data_dir)?;
    let marker = privacy::marker_path(data_dir);
    let first = !marker.exists() && store.import_count()? == 0;
    if first && import {
        // Fail before the marker is written: recording stays off.
        env.home()?;
    }

    std::fs::write(&marker, format!("{}\n", now))
        .with_context(|| format!("could not write {}", marker.display()))?;
    privacy::tighten_permissions(data_dir)?;

    if first && import {
        writeln!(out, "{}", import_history(&store, now, env)?.summary())?;
    }
    write!(
        out,
        "Shell history search is on.
  Recording: every command you run from now on, with its directory, exit code and duration.
  Not recorded: commands your shell keeps out of its own history (leading space,
    HISTIGNORE / HISTORY_IGNORE), anything matching $PSYCHIC_HISTORY_IGNORE, psychic's own commands.
  Stored in: {}/events.db (readable only by you). Nothing leaves this machine.
  Ctrl-R: open a new shell to get the binding.
  Undo: psychic history --disable   Delete everything: psychic history --purge
",
        data_dir.display()
    )?;
    Ok(())
}

/// `--disable`: section 13.2.
fn disable(data_dir: &Path, out: &mut dyn Write) -> Result<()> {
    let marker = privacy::marker_path(data_dir);
    match std::fs::remove_file(&marker) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).with_context(|| format!("could not remove {}", marker.display()));
        }
        _ => {}
    }
    writeln!(
        out,
        "Shell history search is off. Recorded commands are kept; delete them with: psychic history --purge"
    )?;
    Ok(())
}

/// `--status`: section 13.3.
fn status(data_dir: &Path, env: &Env, out: &mut dyn Write) -> Result<()> {
    let store = CmdStore::open(data_dir)?;
    let counts = store.counts()?;
    let marker = privacy::marker_path(data_dir).exists();
    let overridden = match env.history.as_deref() {
        Some(value @ ("0" | "1")) => format!(" (PSYCHIC_HISTORY={} overrides)", value),
        _ => String::new(),
    };
    writeln!(out, "recording: {}{}", on_off(marker), overridden)?;
    writeln!(out, "data dir: {}", data_dir.display())?;
    writeln!(out, "commands: {}", counts.commands)?;
    writeln!(
        out,
        "runs: {} (hook {}, import {})",
        counts.hook_runs + counts.import_runs,
        counts.hook_runs,
        counts.import_runs
    )?;
    writeln!(out, "picks: {}", counts.picks)?;
    writeln!(out, "model: none")?;
    Ok(())
}

/// `--purge`: section 10.5. `confirm` is asked the question and says whether
/// to go ahead (it answers yes by itself under `--yes`).
fn purge(
    data_dir: &Path,
    confirm: &mut dyn FnMut(&str) -> Result<bool>,
    out: &mut dyn Write,
) -> Result<()> {
    let store = CmdStore::open(data_dir)?;
    let counts = store.counts()?;
    let question = format!(
        "Delete all recorded commands ({} commands, {} runs)? [y/N] ",
        counts.commands,
        counts.hook_runs + counts.import_runs
    );
    if !confirm(&question)? {
        writeln!(out, "Nothing deleted.")?;
        return Ok(());
    }
    let deleted = store.purge()?;
    privacy::remove_model_files(data_dir).with_context(|| {
        format!(
            "could not remove the command model in {}",
            data_dir.display()
        )
    })?;
    writeln!(
        out,
        "Deleted {} commands and {} runs. Recording is still {}.",
        deleted.commands,
        deleted.runs,
        on_off(privacy::marker_path(data_dir).exists())
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("psychic-cmd-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Only what the test gives: never the real `$XDG_DATA_HOME` or `$ZDOTDIR`.
    fn env(home: Option<&Path>) -> Env {
        Env {
            home: home.map(Path::to_path_buf),
            xdg_data_home: None,
            zdotdir: None,
            history: None,
            ignore: None,
        }
    }

    fn text(out: Vec<u8>) -> String {
        String::from_utf8(out).unwrap()
    }

    fn track(data_dir: &Path, command: &str, at: i64) {
        let args = cli::TrackCommandArgs {
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: Some(at),
            duration_ms: Some(12),
            session: "s1".to_string(),
            command: OsString::from(command),
        };
        track::run(Some(data_dir), &args, at, None).unwrap();
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn enable_status_purge_and_disable() {
        let root = scratch("actions");
        let data = root.join("data");
        let marker = privacy::marker_path(&data);

        let mut out = Vec::new();
        enable(&data, false, 1000, &env(None), &mut out).unwrap();
        assert!(marker.exists());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "1000\n");
        assert_eq!(
            (mode(&data), mode(&data.join("events.db")), mode(&marker)),
            (0o700, 0o600, 0o600)
        );
        let enabled = text(out);
        assert!(
            enabled.starts_with("Shell history search is on.\n"),
            "{enabled}"
        );
        assert!(enabled.contains(&format!(
            "  Stored in: {}/events.db (readable",
            data.display()
        )));

        track(&data, "echo hello", 1001);
        track(&data, "echo hello", 1002);
        let mut out = Vec::new();
        status(&data, &env(None), &mut out).unwrap();
        assert_eq!(
            text(out),
            format!(
                "recording: on\ndata dir: {}\ncommands: 1\nruns: 2 (hook 2, import 0)\npicks: 0\nmodel: none\n",
                data.display()
            )
        );

        let mut out = Vec::new();
        let mut asked = Vec::new();
        purge(
            &data,
            &mut |question| {
                asked.push(question.to_string());
                Ok(false)
            },
            &mut out,
        )
        .unwrap();
        assert_eq!(
            asked,
            vec!["Delete all recorded commands (1 commands, 2 runs)? [y/N] "]
        );
        assert_eq!(text(out), "Nothing deleted.\n");

        std::fs::write(data.join("cmd_model.txt"), "tree").unwrap();
        let mut out = Vec::new();
        purge(&data, &mut |_| Ok(true), &mut out).unwrap();
        assert_eq!(
            text(out),
            "Deleted 1 commands and 2 runs. Recording is still on.\n"
        );
        assert!(!data.join("cmd_model.txt").exists());
        assert!(marker.exists(), "purge leaves the marker alone");
        let store = CmdStore::open(&data).unwrap();
        for table in [
            "commands",
            "command_runs",
            "command_events",
            "command_imports",
            "deleted_commands",
        ] {
            assert_eq!(
                store::test_support::table_rows(&store, table),
                0,
                "{table} is empty"
            );
        }

        let mut out = Vec::new();
        disable(&data, &mut out).unwrap();
        assert!(!marker.exists());
        assert_eq!(
            text(out),
            "Shell history search is off. Recorded commands are kept; delete them with: psychic history --purge\n"
        );
        let mut out = Vec::new();
        disable(&data, &mut out).expect("disabling twice is fine");

        let mut out = Vec::new();
        let overriding = Env {
            history: Some("1".to_string()),
            ..env(None)
        };
        status(&data, &overriding, &mut out).unwrap();
        assert!(text(out).starts_with("recording: off (PSYCHIC_HISTORY=1 overrides)\n"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_first_enable_imports_and_later_ones_do_not() {
        let root = scratch("enable-import");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".zsh_history"), ": 1700000000:0;make\n").unwrap();
        let data = root.join("data");

        let mut out = Vec::new();
        enable(&data, true, 1_800_000_000, &env(Some(&home)), &mut out).unwrap();
        let first = text(out);
        assert!(first.starts_with("Imported "), "{first}");

        let mut out = Vec::new();
        enable(&data, true, 1_800_000_001, &env(Some(&home)), &mut out).unwrap();
        assert!(
            text(out).starts_with("Shell history search is on."),
            "already enabled: no import"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_command_text_reaches_any_file_but_the_database() {
        let root = scratch("leak");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".bash_history"),
            "echo SENTINEL_c0ffee from bash\n",
        )
        .unwrap();
        let data = root.join("data");
        let mut out = Vec::new();
        enable(&data, false, 1000, &env(None), &mut out).unwrap();
        track(&data, "echo SENTINEL_c0ffee", 1001);
        let store = CmdStore::open(&data).unwrap();
        import_history(&store, 2000, &env(Some(&home))).unwrap();
        drop(store);

        for entry in std::fs::read_dir(&data).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("events.db") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes.windows(15).any(|window| window == b"SENTINEL_c0ffee"),
                "{name} holds command text"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
