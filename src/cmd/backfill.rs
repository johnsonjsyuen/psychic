//! Import existing shell history: Atuin first, then zsh, bash and fish files.
//!
//! Idempotent: a second run inserts nothing. Real timestamps make a run's
//! `(command, second)` unique among imports; untimestamped entries get
//! synthetic times that are not reproducible, so a file's untimestamped
//! entries are imported once only. Everything the hook has already recorded is
//! left to the hook. See `docs/ctrl-r/01-record.md` section 12.

use super::store::{CmdStore, NewRun, RunOutcome, Source};
use super::{privacy, repo, template, track};
use crate::import::shell_history::{self, HistoryCommand};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Where history comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryKind {
    Atuin,
    Zsh,
    Bash,
    Fish,
}

impl HistoryKind {
    fn name(self) -> &'static str {
        match self {
            HistoryKind::Atuin => "atuin",
            HistoryKind::Zsh => "zsh",
            HistoryKind::Bash => "bash",
            HistoryKind::Fish => "fish",
        }
    }
}

/// The files to read, in import order (section 12.1).
pub(super) struct Locations {
    pub atuin: PathBuf,
    /// `~/.zsh_history`, then `$ZDOTDIR/.zsh_history` if different.
    pub zsh: Vec<PathBuf>,
    pub bash: PathBuf,
    pub fish: PathBuf,
}

impl Locations {
    /// The standard places under `home`, given `$XDG_DATA_HOME` and `$ZDOTDIR`.
    pub(super) fn discover(
        home: &Path,
        xdg_data_home: Option<&Path>,
        zdotdir: Option<&Path>,
    ) -> Self {
        let data_home = xdg_data_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".local/share"));
        let mut zsh = vec![home.join(".zsh_history")];
        if let Some(zdotdir) = zdotdir {
            let file = zdotdir.join(".zsh_history");
            if !zsh.contains(&file) {
                zsh.push(file);
            }
        }
        Locations {
            atuin: data_home.join("atuin/history.db"),
            zsh,
            bash: home.join(".bash_history"),
            fish: data_home.join("fish/fish_history"),
        }
    }
}

/// What one source contributed.
pub(super) struct SourceReport {
    pub kind: HistoryKind,
    /// `<kind>:<path>`, the row in `command_imports`.
    #[allow(dead_code, reason = "for corpus statistics (Phase D0) and tests")]
    pub key: String,
    /// Entries read from the source.
    #[allow(dead_code, reason = "for corpus statistics (Phase D0) and tests")]
    pub found: u64,
    /// Runs inserted.
    pub inserted: u64,
    /// Entries read but not inserted (filtered, tombstoned, already there).
    #[allow(dead_code, reason = "for corpus statistics (Phase D0) and tests")]
    pub skipped: u64,
    /// Why the source could not be read at all (Atuin: `locked`,
    /// `unreadable`, `unexpected schema`); `None` when it was read or simply
    /// absent.
    pub unavailable: Option<&'static str>,
}

pub(super) struct BackfillReport {
    pub sources: Vec<SourceReport>,
}

impl BackfillReport {
    /// `Imported 18,402 commands (atuin 12,001, zsh 6,401, bash 0, fish 0).`
    pub(super) fn summary(&self) -> String {
        let inserted = |kind| -> u64 {
            self.sources
                .iter()
                .filter(|source| source.kind == kind)
                .map(|source| source.inserted)
                .sum()
        };
        let total: u64 = self.sources.iter().map(|source| source.inserted).sum();
        let atuin = match self
            .sources
            .iter()
            .find(|source| source.kind == HistoryKind::Atuin)
            .and_then(|source| source.unavailable)
        {
            Some(reason) => format!("atuin skipped ({})", reason),
            None => format!("atuin {}", thousands(inserted(HistoryKind::Atuin))),
        };
        format!(
            "Imported {} commands ({}, zsh {}, bash {}, fish {}).",
            thousands(total),
            atuin,
            thousands(inserted(HistoryKind::Zsh)),
            thousands(inserted(HistoryKind::Bash)),
            thousands(inserted(HistoryKind::Fish)),
        )
    }
}

/// `18402` -> `18,402`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Import every source in `locations` into `store`. `home` is where the `cd`
/// replay starts, `now` the current Unix second, `env_ignore` the value of
/// `PSYCHIC_HISTORY_IGNORE`.
pub(super) fn run(
    store: &CmdStore,
    locations: &Locations,
    home: &Path,
    now: i64,
    env_ignore: Option<&str>,
) -> Result<BackfillReport> {
    let context = ImportContext {
        hook_start: store.first_hook_run()?.unwrap_or(i64::MAX),
        now,
        ignore: env_ignore,
        home,
    };
    let mut sources = vec![import_atuin(store, &locations.atuin, &context)?];
    let files = locations
        .zsh
        .iter()
        .map(|path| (HistoryKind::Zsh, path))
        .chain([
            (HistoryKind::Bash, &locations.bash),
            (HistoryKind::Fish, &locations.fish),
        ]);
    for (kind, path) in files {
        sources.push(import_file(store, kind, path, home, &context)?);
    }
    Ok(BackfillReport { sources })
}

/// What every source is filtered against.
struct ImportContext<'a> {
    /// The first hook-recorded run; `i64::MAX` when there is none.
    hook_start: i64,
    now: i64,
    ignore: Option<&'a str>,
    /// Where the search for a repository stops (section 8).
    home: &'a Path,
}

/// One entry of a source, ready to be filtered and recorded.
struct Candidate {
    /// Trimmed.
    text: String,
    started_at: i64,
    /// Whether `started_at` was made up (section 12.3 step 2).
    synthetic: bool,
    cwd: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
    shell_session: String,
}

/// Filter `candidates` (already in import order) and record the rest in one
/// transaction that also marks `key` imported. Returns how many were inserted.
fn record_candidates(
    store: &CmdStore,
    key: &str,
    candidates: &[Candidate],
    context: &ImportContext,
) -> Result<u64> {
    let already = store.import_done(key)?;
    store.transaction(|tx| {
        let mut inserted = 0;
        for candidate in candidates {
            if !should_import(store, candidate, already, context)? {
                continue;
            }
            let cwd = candidate.cwd.as_deref();
            let repo_root =
                cwd.and_then(|cwd| repo::find_repo_root(Path::new(cwd), Some(context.home)));
            let repo_root = repo_root.as_deref().map(Path::to_string_lossy);
            let template = template::templatize(&candidate.text);
            let outcome = CmdStore::record_run_in(
                tx,
                &NewRun {
                    text: &candidate.text,
                    template: &template,
                    started_at: candidate.started_at,
                    cwd,
                    repo_root: repo_root.as_deref(),
                    exit_code: candidate.exit_code,
                    duration_ms: candidate.duration_ms,
                    shell_session: &candidate.shell_session,
                    source: Source::Import,
                },
            )?;
            if matches!(outcome, RunOutcome::Inserted { .. }) {
                inserted += 1;
            }
        }
        store.mark_import(key, context.now, inserted)?;
        Ok(inserted)
    })
}

/// Section 12.3 step 4.
fn should_import(
    store: &CmdStore,
    candidate: &Candidate,
    already: bool,
    context: &ImportContext,
) -> Result<bool> {
    let text = candidate.text.as_str();
    if !track::is_recordable(text, context.ignore) {
        return Ok(false);
    }
    if store.is_tombstoned(privacy::fnv1a64(text))? {
        return Ok(false);
    }
    if candidate.synthetic {
        // Not comparable by time: anything the hook has recorded is the hook's,
        // and a file's untimestamped entries are imported only once.
        if already || (context.hook_start != i64::MAX && store.has_hook_run(text)?) {
            return Ok(false);
        }
    } else if candidate.started_at >= context.hook_start {
        return Ok(false);
    }
    Ok(true)
}

/// Record one source's candidates, turning a failed transaction into an empty
/// report rather than an error, so the other sources still import (E-R-14).
fn finish(
    store: &CmdStore,
    kind: HistoryKind,
    key: String,
    candidates: Vec<Candidate>,
    context: &ImportContext,
) -> SourceReport {
    let found = candidates.len() as u64;
    let inserted = match record_candidates(store, &key, &candidates, context) {
        Ok(inserted) => inserted,
        Err(error) => {
            // The error names the operation and SQLite code only.
            log::error!("import of {} failed: {}", key, error);
            0
        }
    };
    SourceReport {
        kind,
        key,
        found,
        inserted,
        skipped: found - inserted,
        unavailable: None,
    }
}

// ---------------------------------------------------------------------------
// Shell history files
// ---------------------------------------------------------------------------

/// When an entry ran, as recorded or made up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AssignedTime {
    pub at: i64,
    pub synthetic: bool,
}

/// Give every entry a time in one backward pass. A real timestamp not in the
/// future is kept, even out of order (several terminals share a zsh file). An
/// untimestamped (or future) entry sits one second before the entry after it,
/// the last one at `ceiling - 1`, so an old plain bash history does not pose
/// as "run just now".
pub(super) fn assign_times(
    timestamps: &[Option<i64>],
    now: i64,
    ceiling: i64,
) -> Vec<AssignedTime> {
    assert!(ceiling <= now, "the ceiling is at most now");
    let mut out = vec![
        AssignedTime {
            at: 0,
            synthetic: true
        };
        timestamps.len()
    ];
    let mut next = ceiling;
    for (i, timestamp) in timestamps.iter().enumerate().rev() {
        out[i] = match timestamp {
            Some(t) if *t <= now => AssignedTime {
                at: *t,
                synthetic: false,
            },
            _ => AssignedTime {
                at: next - 1,
                synthetic: true,
            },
        };
        next = out[i].at;
    }
    out
}

/// The first 8 hex digits of the path's FNV-1a hash: tells two files of the
/// same kind apart in `shell_session` without storing the path again.
fn path_tag(path: &Path) -> String {
    let hash = format!("{:016x}", privacy::fnv1a64(&path.to_string_lossy()) as u64);
    hash[..8].to_string()
}

fn import_file(
    store: &CmdStore,
    kind: HistoryKind,
    path: &Path,
    home: &Path,
    context: &ImportContext,
) -> Result<SourceReport> {
    let key = format!("{}:{}", kind.name(), path.display());
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            log::info!("import: {} history not readable", kind.name());
            return Ok(SourceReport {
                kind,
                key,
                found: 0,
                inserted: 0,
                skipped: 0,
                unavailable: None,
            });
        }
    };
    let commands: Vec<HistoryCommand> = match kind {
        HistoryKind::Zsh => shell_history::parse_zsh(&bytes),
        HistoryKind::Bash => shell_history::parse_bash_entries(&bytes),
        HistoryKind::Fish => shell_history::parse_fish(&bytes),
        HistoryKind::Atuin => unreachable!("Atuin is a database, not a file"),
    };
    let cwds = shell_history::replay_cwds(&commands, home, &mut |dir| {
        crate::import::is_dir_within_limits(dir, home)
    });
    let mtime = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(context.now, |since| since.as_secs() as i64);
    let ceiling = context.now.min(mtime + 1);
    let timestamps: Vec<Option<i64>> = commands.iter().map(|command| command.timestamp).collect();
    let times = assign_times(&timestamps, context.now, ceiling);

    let tag = path_tag(path);
    let mut candidates: Vec<Candidate> = commands
        .iter()
        .zip(cwds)
        .zip(&times)
        .map(|((command, cwd), time)| Candidate {
            text: command.text.trim().to_string(),
            started_at: time.at,
            synthetic: time.synthetic,
            cwd: cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
            exit_code: None,
            duration_ms: None,
            shell_session: if time.synthetic {
                format!("import-synth:{}:{}", kind.name(), tag)
            } else {
                format!("import:{}:{}", kind.name(), tag)
            },
        })
        .collect();
    // Stable, so entries in the same second keep their file order.
    candidates.sort_by_key(|candidate| candidate.started_at);
    Ok(finish(store, kind, key, candidates, context))
}

// ---------------------------------------------------------------------------
// Atuin
// ---------------------------------------------------------------------------

/// Columns the import needs from Atuin's `history` table.
const ATUIN_COLUMNS: &[&str] = &["command", "timestamp", "cwd", "exit", "duration", "session"];

/// A timestamp in nanoseconds (Atuin), microseconds, milliseconds or seconds,
/// told apart by magnitude, in seconds.
fn to_seconds(timestamp: i64) -> i64 {
    match timestamp {
        t if t >= 100_000_000_000_000_000 => t / 1_000_000_000,
        t if t >= 100_000_000_000_000 => t / 1_000_000,
        t if t >= 100_000_000_000 => t / 1_000,
        t => t,
    }
}

/// Atuin writes `unknown` for rows it imported itself.
fn atuin_cwd(cwd: Option<String>) -> Option<String> {
    cwd.filter(|cwd| cwd.starts_with('/'))
}

/// Read Atuin's history, read-only. `Err` carries the reason to report.
fn read_atuin(path: &Path) -> std::result::Result<Vec<Candidate>, &'static str> {
    let open = || -> rusqlite::Result<Connection> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 2000;")?;
        Ok(conn)
    };
    let conn = open().map_err(|error| atuin_failure(&error))?;
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('history')")
        .and_then(|mut s| {
            s.query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()
        })
        .map_err(|error| atuin_failure(&error))?;
    let has = |name: &str| columns.iter().any(|column| column == name);
    if !ATUIN_COLUMNS.iter().all(|column| has(column)) {
        return Err("unexpected schema");
    }
    let sql = format!(
        "SELECT command, timestamp, duration, exit, cwd, session FROM history {} ORDER BY timestamp, id",
        if has("deleted_at") {
            "WHERE deleted_at IS NULL"
        } else {
            ""
        }
    );
    let mut statement = conn.prepare(&sql).map_err(|_| "unexpected schema")?;
    let rows = statement
        .query_map([], |row| {
            let duration: Option<i64> = row.get(2)?;
            let exit: Option<i64> = row.get(3)?;
            let session: Option<String> = row.get(5)?;
            Ok(Candidate {
                text: row.get::<_, String>(0)?.trim().to_string(),
                started_at: to_seconds(row.get(1)?),
                synthetic: false,
                cwd: atuin_cwd(row.get(4)?),
                exit_code: exit
                    .filter(|code| *code >= 0)
                    .and_then(|code| i32::try_from(code).ok()),
                duration_ms: duration.filter(|ns| *ns >= 0).map(|ns| ns / 1_000_000),
                shell_session: format!("atuin:{}", session.unwrap_or_default()),
            })
        })
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>());
    rows.map_err(|error| atuin_failure(&error))
}

/// Why reading Atuin's database failed, as the report words it: `locked`
/// when Atuin (or anything) held it past the busy timeout, `unreadable` for
/// everything else (not a database, damaged, no permission).
fn atuin_failure(error: &rusqlite::Error) -> &'static str {
    use rusqlite::ErrorCode;
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "locked",
        _ => "unreadable",
    }
}

fn import_atuin(store: &CmdStore, path: &Path, context: &ImportContext) -> Result<SourceReport> {
    let key = format!("atuin:{}", path.display());
    let empty = |unavailable| SourceReport {
        kind: HistoryKind::Atuin,
        key: key.clone(),
        found: 0,
        inserted: 0,
        skipped: 0,
        unavailable,
    };
    if !path.exists() {
        return Ok(empty(None));
    }
    match read_atuin(path) {
        Ok(candidates) => Ok(finish(store, HistoryKind::Atuin, key, candidates, context)),
        Err(reason) => {
            log::warn!("import: atuin skipped ({})", reason);
            Ok(empty(Some(reason)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::store::test_support::{command_id, hook_run, query_strings, table_rows};

    #[test]
    fn assign_times_fills_gaps_backwards_from_the_ceiling() {
        let pairs = |times: Vec<AssignedTime>| -> Vec<(i64, bool)> {
            times.into_iter().map(|t| (t.at, t.synthetic)).collect()
        };
        assert_eq!(
            pairs(assign_times(&[None, Some(50), None, None], 100, 100)),
            vec![(49, true), (50, false), (98, true), (99, true)]
        );
        assert_eq!(
            pairs(assign_times(&[Some(500)], 100, 100)),
            vec![(99, true)],
            "a future timestamp is not believed"
        );
        assert_eq!(
            pairs(assign_times(
                &[Some(30), Some(20), Some(10), Some(5)],
                100,
                100
            )),
            vec![(30, false), (20, false), (10, false), (5, false)],
            "an out-of-order file keeps its real times"
        );
        assert_eq!(pairs(assign_times(&[None], 100, 60)), vec![(59, true)]);
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(18402), "18,402");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn timestamps_are_scaled_to_seconds_by_magnitude() {
        assert_eq!(to_seconds(1_700_000_000_123_456_789), 1_700_000_000);
        assert_eq!(to_seconds(1_700_000_000_123_456), 1_700_000_000);
        assert_eq!(to_seconds(1_700_000_000_123), 1_700_000_000);
        assert_eq!(to_seconds(1_700_000_000), 1_700_000_000);
    }

    #[test]
    fn the_summary_names_every_kind_and_a_skipped_atuin() {
        let source = |kind, inserted, unavailable| SourceReport {
            kind,
            key: String::new(),
            found: inserted,
            inserted,
            skipped: 0,
            unavailable,
        };
        let report = BackfillReport {
            sources: vec![
                source(HistoryKind::Atuin, 12001, None),
                source(HistoryKind::Zsh, 6000, None),
                source(HistoryKind::Zsh, 401, None),
                source(HistoryKind::Bash, 0, None),
                source(HistoryKind::Fish, 0, None),
            ],
        };
        assert_eq!(
            report.summary(),
            "Imported 18,402 commands (atuin 12,001, zsh 6,401, bash 0, fish 0)."
        );
        let report = BackfillReport {
            sources: vec![source(HistoryKind::Atuin, 0, Some("locked"))],
        };
        assert_eq!(
            report.summary(),
            "Imported 0 commands (atuin skipped (locked), zsh 0, bash 0, fish 0)."
        );
    }

    /// A scratch home with its own data dir; never the real `$HOME`.
    struct Scratch {
        root: PathBuf,
        home: PathBuf,
        store: CmdStore,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("psychic-cmd-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.join("home");
            std::fs::create_dir_all(&home).unwrap();
            let store = CmdStore::open(&root.join("data")).unwrap();
            Scratch { root, home, store }
        }

        fn locations(&self) -> Locations {
            Locations::discover(&self.home, None, None)
        }

        fn import(&self, now: i64) -> BackfillReport {
            run(&self.store, &self.locations(), &self.home, now, None).unwrap()
        }

        fn inserted(report: &BackfillReport, kind: HistoryKind) -> u64 {
            report
                .sources
                .iter()
                .filter(|source| source.kind == kind)
                .map(|source| source.inserted)
                .sum()
        }

        fn runs(&self) -> Vec<String> {
            query_strings(
                &self.store,
                "SELECT c.text, r.started_at, r.shell_session, IFNULL(r.cwd, 'NULL'),
                        IFNULL(r.exit_code, 'NULL'), IFNULL(r.duration_ms, 'NULL')
                 FROM command_runs r JOIN commands c ON c.id = r.command_id
                 ORDER BY r.started_at, r.id",
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn set_mtime(path: &Path, secs: i64) {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    const NOW: i64 = 1_800_000_000;
    const M: i64 = 1_750_000_000;

    #[test]
    fn a_second_import_inserts_nothing() {
        let scratch = Scratch::new("backfill-idem");
        std::fs::write(
            scratch.home.join(".zsh_history"),
            ": 1700000000:0;p\n: 1700000001:0;ls\n: 1700000002:0;git status\n: 1700000002:0;make\n",
        )
        .unwrap();
        let bash = scratch.home.join(".bash_history");
        std::fs::write(&bash, "echo one\necho two\necho three\n").unwrap();
        set_mtime(&bash, M);

        let first = scratch.import(NOW);
        assert_eq!(Scratch::inserted(&first, HistoryKind::Zsh), 3);
        assert_eq!(Scratch::inserted(&first, HistoryKind::Bash), 3);
        let tag = path_tag(&bash);
        let rows = scratch.runs();
        assert_eq!(
            rows[3..],
            [
                format!("echo one|{}|import-synth:bash:{tag}|/HOME|NULL|NULL", M - 2),
                format!("echo two|{}|import-synth:bash:{tag}|/HOME|NULL|NULL", M - 1),
                format!("echo three|{}|import-synth:bash:{tag}|/HOME|NULL|NULL", M),
            ]
            .map(|row| row.replace("/HOME", &scratch.home.to_string_lossy()))
        );
        assert!(rows[0].starts_with("ls|1700000001|import:zsh:"));

        let second = scratch.import(NOW);
        assert_eq!(Scratch::inserted(&second, HistoryKind::Zsh), 0);
        assert_eq!(Scratch::inserted(&second, HistoryKind::Bash), 0);
        assert_eq!(scratch.runs(), rows);
        assert_eq!(
            second.summary(),
            "Imported 0 commands (atuin 0, zsh 0, bash 0, fish 0)."
        );
    }

    #[test]
    fn a_multi_line_bash_entry_is_one_command() {
        let scratch = Scratch::new("backfill-bash-multi");
        std::fs::write(
            scratch.home.join(".bash_history"),
            "#1700000000\nfor f in *; do\n  echo \"$f\"\ndone\n#1700000005\nls\n",
        )
        .unwrap();

        let report = scratch.import(NOW);

        assert_eq!(Scratch::inserted(&report, HistoryKind::Bash), 2);
        let texts: Vec<String> = scratch
            .runs()
            .into_iter()
            .map(|row| row.split('|').take(2).collect::<Vec<_>>().join("|"))
            .collect();
        assert_eq!(
            texts,
            vec![
                "for f in *; do\n  echo \"$f\"\ndone|1700000000",
                "ls|1700000005"
            ]
        );
    }

    #[test]
    fn the_import_stops_at_the_first_hook_run() {
        let scratch = Scratch::new("backfill-hook");
        scratch
            .store
            .record_run(&hook_run("vim", "s", 1_700_000_050, Some(0)))
            .unwrap();
        std::fs::write(
            scratch.home.join(".zsh_history"),
            ": 1700000000:0;cargo build\n: 1700000100:0;cargo test\n",
        )
        .unwrap();

        scratch.import(NOW);

        let texts: Vec<String> = scratch
            .runs()
            .into_iter()
            .map(|row| row.split('|').next().unwrap().to_string())
            .collect();
        assert_eq!(texts, vec!["cargo build", "vim"]);
    }

    #[test]
    fn untimestamped_entries_the_hook_already_has_are_not_imported() {
        let scratch = Scratch::new("backfill-synth-hook");
        scratch
            .store
            .record_run(&hook_run("make", "s", NOW - 10, Some(0)))
            .unwrap();
        std::fs::write(scratch.home.join(".bash_history"), "make\nmake test\n").unwrap();

        scratch.import(NOW);

        assert_eq!(scratch.store.load_commands().unwrap().len(), 2);
        assert_eq!(scratch.store.counts().unwrap().import_runs, 1);
    }

    #[test]
    fn a_deleted_command_is_not_imported_again() {
        let scratch = Scratch::new("backfill-tomb");
        std::fs::write(
            scratch.home.join(".zsh_history"),
            ": 1700000000:0;rm -rf build\n: 1700000001:0;ls\n",
        )
        .unwrap();
        scratch.import(NOW);
        let id = command_id(&scratch.store, "rm -rf build").unwrap();
        scratch
            .store
            .delete_command(id, "rm -rf build", NOW)
            .unwrap();

        scratch.import(NOW);

        assert_eq!(command_id(&scratch.store, "rm -rf build"), None);
        assert!(command_id(&scratch.store, "ls").is_some());
    }

    #[test]
    fn a_deleted_command_stays_deleted_through_a_purge_and_a_new_import() {
        let scratch = Scratch::new("backfill-tomb-purge");
        std::fs::write(
            scratch.home.join(".zsh_history"),
            ": 1700000000:0;echo synthetic-secret-zsh\n: 1700000001:0;echo kept-zsh\n",
        )
        .unwrap();
        // Untimestamped: imported again only because purge forgets the import.
        std::fs::write(
            scratch.home.join(".bash_history"),
            "echo synthetic-secret-bash\necho kept-bash\n",
        )
        .unwrap();
        scratch.import(NOW);
        for text in ["echo synthetic-secret-zsh", "echo synthetic-secret-bash"] {
            let id = command_id(&scratch.store, text).unwrap();
            scratch.store.delete_command(id, text, NOW).unwrap();
        }

        scratch.store.purge().unwrap();
        assert_eq!(table_rows(&scratch.store, "commands"), 0);
        assert_eq!(table_rows(&scratch.store, "command_imports"), 0);
        assert_eq!(table_rows(&scratch.store, "deleted_commands"), 2);
        scratch.import(NOW + 10);

        assert_eq!(
            command_id(&scratch.store, "echo synthetic-secret-zsh"),
            None
        );
        assert_eq!(
            command_id(&scratch.store, "echo synthetic-secret-bash"),
            None
        );
        assert!(command_id(&scratch.store, "echo kept-zsh").is_some());
        assert!(command_id(&scratch.store, "echo kept-bash").is_some());
    }

    #[test]
    fn replayed_directories_become_the_cwd_and_ignored_commands_are_skipped() {
        let scratch = Scratch::new("backfill-cwd");
        std::fs::create_dir_all(scratch.home.join("proj/.git")).unwrap();
        std::fs::write(
            scratch.home.join(".zsh_history"),
            ": 1700000000:0;cd proj\n: 1700000001:0;cargo build\n: 1700000002:0;export TOKEN=x\n",
        )
        .unwrap();

        let report = run(
            &scratch.store,
            &scratch.locations(),
            &scratch.home,
            NOW,
            Some("export *"),
        )
        .unwrap();

        let proj = scratch.home.join("proj").to_string_lossy().into_owned();
        assert_eq!(
            query_strings(
                &scratch.store,
                "SELECT c.text, IFNULL(r.repo_root, 'NULL') FROM command_runs r
                 JOIN commands c ON c.id = r.command_id ORDER BY r.started_at"
            ),
            vec!["cd proj|NULL".to_string(), format!("cargo build|{proj}")]
        );
        let zsh = &report.sources[1];
        assert_eq!((zsh.found, zsh.inserted, zsh.skipped), (3, 2, 1));
    }

    fn atuin_db(path: &Path, with_deleted_at: bool) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE history (
                 id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL, duration INTEGER NOT NULL,
                 exit INTEGER NOT NULL, command TEXT NOT NULL, cwd TEXT NOT NULL,
                 session TEXT NOT NULL, hostname TEXT NOT NULL{})",
            if with_deleted_at {
                ", deleted_at INTEGER"
            } else {
                ""
            }
        ))
        .unwrap();
    }

    #[test]
    fn atuin_rows_are_imported_read_only_with_their_units_converted() {
        let scratch = Scratch::new("backfill-atuin");
        let path = scratch.home.join(".local/share/atuin/history.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        atuin_db(&path, true);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "INSERT INTO history VALUES
                   ('a', 1700000000000000000, 2500000000, 0, 'make', '/x', 'S1', 'h', NULL),
                   ('b', 1700000001000000000, 1000000, 1, 'make test', 'unknown', 'S1', 'h', NULL),
                   ('c', 1700000002000000000, -1, -1, 'ls', '/y', 'S2', 'h', NULL),
                   ('d', 1700000003000000000, 5, 0, 'gone', '/z', 'S2', 'h', 1700000004000000000);",
            )
            .unwrap();
        }
        let before = std::fs::metadata(&path).unwrap();

        let report = scratch.import(NOW);

        assert_eq!(Scratch::inserted(&report, HistoryKind::Atuin), 3);
        assert_eq!(
            scratch.runs(),
            vec![
                "make|1700000000|atuin:S1|/x|0|2500",
                "make test|1700000001|atuin:S1|NULL|1|1",
                "ls|1700000002|atuin:S2|/y|NULL|NULL",
            ]
        );
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert_eq!(
            Scratch::inserted(&scratch.import(NOW), HistoryKind::Atuin),
            0,
            "the same rows again are duplicates"
        );
    }

    #[test]
    fn an_atuin_database_with_an_unexpected_schema_is_skipped() {
        let scratch = Scratch::new("backfill-atuin-schema");
        let path = scratch.home.join(".local/share/atuin/history.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE history (id TEXT, command TEXT)")
            .unwrap();
        std::fs::write(scratch.home.join(".bash_history"), "ls\n").unwrap();

        let report = scratch.import(NOW);

        assert!(
            report.summary().starts_with(
                "Imported 1 commands (atuin skipped (unexpected schema), zsh 0, bash 1"
            )
        );
    }

    #[test]
    fn an_atuin_file_that_is_no_database_is_unreadable_not_locked() {
        let scratch = Scratch::new("backfill-atuin-garbage");
        let path = scratch.home.join(".local/share/atuin/history.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![0x5a; 4096]).unwrap();

        let report = scratch.import(NOW);

        assert_eq!(report.sources[0].unavailable, Some("unreadable"));
        assert!(
            report
                .summary()
                .starts_with("Imported 0 commands (atuin skipped (unreadable), zsh 0")
        );
    }

    #[test]
    fn an_atuin_database_held_by_a_writer_is_locked() {
        let scratch = Scratch::new("backfill-atuin-locked");
        let path = scratch.home.join(".local/share/atuin/history.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        atuin_db(&path, false);
        // A rollback-journal database under an exclusive transaction: no
        // reader gets in until it ends.
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();

        let report = scratch.import(NOW);

        assert_eq!(report.sources[0].unavailable, Some("locked"));
        writer.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn locations_follow_xdg_data_home_and_zdotdir() {
        let home = Path::new("/h");
        let plain = Locations::discover(home, None, None);
        assert_eq!(plain.atuin, Path::new("/h/.local/share/atuin/history.db"));
        assert_eq!(plain.zsh, vec![PathBuf::from("/h/.zsh_history")]);
        assert_eq!(plain.bash, Path::new("/h/.bash_history"));
        assert_eq!(plain.fish, Path::new("/h/.local/share/fish/fish_history"));

        let custom = Locations::discover(home, Some(Path::new("/d")), Some(Path::new("/z")));
        assert_eq!(custom.atuin, Path::new("/d/atuin/history.db"));
        assert_eq!(
            custom.zsh,
            vec![
                PathBuf::from("/h/.zsh_history"),
                PathBuf::from("/z/.zsh_history")
            ]
        );
        assert_eq!(custom.fish, Path::new("/d/fish/fish_history"));
        assert_eq!(
            Locations::discover(home, None, Some(home)).zsh.len(),
            1,
            "ZDOTDIR=$HOME is not read twice"
        );
    }
}
