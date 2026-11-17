//! The command tables in `events.db`, and every SQL statement that touches them.
//!
//! No other file contains SQL on `commands`, `command_runs`, `command_events`,
//! `command_imports` or `deleted_commands`. Each statement is a named constant
//! whose ID (S1, R2, D3, ...) is the one `docs/ctrl-r/01-record.md` section 6
//! uses, and `plan_tests` below checks the index each one must use.
//!
//! Errors never carry command text: SQLite failures are reported by operation
//! and error code only (section 10.6), through [`SqlContext::op`].

use super::privacy;
use super::tuning::CMD_SEQUENCE_MAX_GAP_SECS;
use crate::db::Database;
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::Path;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Every table and index, in the order of section 5. `deleted_commands` is
/// created last, so its presence means all the rest exist.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS commands (
    id             INTEGER PRIMARY KEY,
    text           TEXT    NOT NULL UNIQUE,
    template       TEXT    NOT NULL,
    first_run_at   INTEGER NOT NULL,
    last_run_at    INTEGER NOT NULL,
    run_count      INTEGER NOT NULL DEFAULT 0,
    success_count  INTEGER NOT NULL DEFAULT 0,
    fail_count     INTEGER NOT NULL DEFAULT 0,
    pick_count     INTEGER NOT NULL DEFAULT 0,
    last_exit_code INTEGER
);
CREATE INDEX IF NOT EXISTS idx_commands_template ON commands(template);

CREATE TABLE IF NOT EXISTS command_runs (
    id              INTEGER PRIMARY KEY,
    command_id      INTEGER NOT NULL,
    started_at      INTEGER NOT NULL,
    cwd             TEXT,
    repo_root       TEXT,
    exit_code       INTEGER,
    duration_ms     INTEGER,
    shell_session   TEXT    NOT NULL,
    prev_command_id INTEGER,
    source          TEXT    NOT NULL CHECK (source IN ('hook', 'import'))
);
CREATE INDEX IF NOT EXISTS idx_command_runs_cwd     ON command_runs(cwd, command_id, started_at) WHERE cwd IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_command_runs_repo    ON command_runs(repo_root, command_id)       WHERE repo_root IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_command_runs_time    ON command_runs(started_at, command_id, exit_code);
CREATE INDEX IF NOT EXISTS idx_command_runs_prev    ON command_runs(prev_command_id, command_id) WHERE prev_command_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_command_runs_session ON command_runs(shell_session, started_at);
CREATE INDEX IF NOT EXISTS idx_command_runs_command ON command_runs(command_id, started_at);
CREATE INDEX IF NOT EXISTS idx_command_runs_hook    ON command_runs(started_at) WHERE source = 'hook';
CREATE UNIQUE INDEX IF NOT EXISTS idx_command_runs_import ON command_runs(command_id, started_at) WHERE source = 'import';

CREATE TABLE IF NOT EXISTS command_events (
    id              INTEGER PRIMARY KEY,
    timestamp       INTEGER NOT NULL,
    action          TEXT    NOT NULL CHECK (action IN ('impression', 'pick')),
    command_id      INTEGER NOT NULL,
    query           TEXT    NOT NULL,
    rank            INTEGER,
    session_id      TEXT    NOT NULL,
    subsession_id   INTEGER NOT NULL,
    episode_queries TEXT,
    cwd             TEXT,
    repo_root       TEXT,
    shell_session   TEXT
);
CREATE INDEX IF NOT EXISTS idx_command_events_pick    ON command_events(timestamp, command_id) WHERE action = 'pick';
CREATE INDEX IF NOT EXISTS idx_command_events_command ON command_events(command_id);

CREATE TABLE IF NOT EXISTS command_imports (
    source_key     TEXT PRIMARY KEY,
    imported_at    INTEGER NOT NULL,
    runs           INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS deleted_commands (
    text_hash      INTEGER PRIMARY KEY,
    deleted_at     INTEGER NOT NULL
);
";

/// Create the command tables if they are missing. Called once per database
/// file per process from `Database::prepare_schema`; after the first time it
/// costs one indexed lookup.
pub fn create_schema(conn: &Connection) -> Result<()> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'deleted_commands'",
            [],
            |_| Ok(()),
        )
        .optional()
        .op("checking the command tables")?
        .is_some();
    if !exists {
        conn.execute_batch(SCHEMA)
            .op("creating the command tables")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

const S1: &str = "INSERT INTO commands (text, template, first_run_at, last_run_at) VALUES (?1, ?2, ?3, ?3) ON CONFLICT(text) DO NOTHING";
const S2: &str = "SELECT id FROM commands WHERE text = ?1";
const S3: &str = "SELECT command_id, started_at FROM command_runs WHERE shell_session = ?1 AND started_at <= ?2 ORDER BY started_at DESC, id DESC LIMIT 1";
const S4: &str = "INSERT OR IGNORE INTO command_runs (command_id, started_at, cwd, repo_root, exit_code, duration_ms, shell_session, prev_command_id, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
const S5: &str = "UPDATE commands SET run_count = run_count + 1, success_count = success_count + ?2, fail_count = fail_count + ?3, first_run_at = MIN(first_run_at, ?4), last_exit_code = CASE WHEN ?4 >= last_run_at THEN ?5 ELSE last_exit_code END, last_run_at = MAX(last_run_at, ?4) WHERE id = ?1";

const R1: &str = "SELECT id, text, last_run_at, run_count, success_count, fail_count, pick_count, last_exit_code FROM commands";
const R2: &str = "SELECT command_id, COUNT(*), MAX(started_at) FROM command_runs WHERE cwd = ?1 GROUP BY command_id";
const R3: &str =
    "SELECT command_id, COUNT(*) FROM command_runs WHERE repo_root = ?1 GROUP BY command_id";
const R4: &str = "SELECT command_id, started_at, exit_code FROM command_runs WHERE started_at > ?1 ORDER BY started_at";
const R5: &str = "SELECT c.template FROM command_runs r JOIN commands c ON c.id = r.command_id WHERE r.shell_session = ?1 ORDER BY r.started_at DESC, r.id DESC LIMIT 3";
/// R6 for 1, 2 and 3 templates: one placeholder per template.
const R6: [&str; 3] = [
    "SELECT r.command_id, p.template, COUNT(*) FROM commands p CROSS JOIN command_runs r ON r.prev_command_id = p.id WHERE p.template IN (?1) GROUP BY r.command_id, p.template",
    "SELECT r.command_id, p.template, COUNT(*) FROM commands p CROSS JOIN command_runs r ON r.prev_command_id = p.id WHERE p.template IN (?1, ?2) GROUP BY r.command_id, p.template",
    "SELECT r.command_id, p.template, COUNT(*) FROM commands p CROSS JOIN command_runs r ON r.prev_command_id = p.id WHERE p.template IN (?1, ?2, ?3) GROUP BY r.command_id, p.template",
];
const R7: &str = "SELECT DISTINCT command_id FROM command_runs WHERE shell_session = ?1";
const R8: &str = "SELECT command_id, query, timestamp, episode_queries FROM command_events WHERE action = 'pick' AND timestamp > ?1 ORDER BY timestamp, id";
const R9: &str = "SELECT cwd, COUNT(*), MAX(started_at) FROM command_runs WHERE command_id = ?1 AND cwd IS NOT NULL GROUP BY cwd ORDER BY MAX(started_at) DESC LIMIT 5";
const R10: &str = "SELECT p.template, COUNT(*) FROM command_runs r JOIN commands p ON p.id = r.prev_command_id WHERE r.command_id = ?1 GROUP BY p.template ORDER BY COUNT(*) DESC, p.template LIMIT 3";

const E1: &str = "INSERT INTO command_events (timestamp, action, command_id, query, rank, session_id, subsession_id, episode_queries, cwd, repo_root, shell_session) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";
const E2: &str = "UPDATE commands SET pick_count = pick_count + 1 WHERE id = ?1";

const D1: &str = "INSERT OR REPLACE INTO deleted_commands (text_hash, deleted_at) VALUES (?1, ?2)";
const D2: &str = "DELETE FROM command_events WHERE command_id = ?1";
const D3: &str = "DELETE FROM command_runs WHERE command_id = ?1";
const D4: &str = "UPDATE command_runs SET prev_command_id = NULL WHERE prev_command_id = ?1";
const D5: &str = "DELETE FROM commands WHERE id = ?1";

const P1: &str = "DELETE FROM command_events; DELETE FROM command_runs; DELETE FROM commands; DELETE FROM command_imports; DELETE FROM deleted_commands;";
/// Before P1: the pages the deletes free are overwritten with zeros.
const P1_SECURE: &str = "PRAGMA secure_delete = ON";
/// After P1 and `VACUUM`: copy the log into the database and cut it to zero
/// bytes, so the old pages it still holds leave the disk. Column 0 is 1 when
/// another connection kept it from finishing.
const P1_CHECKPOINT: &str = "PRAGMA wal_checkpoint(TRUNCATE)";

const I1: &str = "SELECT MIN(started_at) FROM command_runs WHERE source = 'hook'";
const I2_SELECT: &str = "SELECT 1 FROM command_imports WHERE source_key = ?1";
const I2_INSERT: &str =
    "INSERT OR REPLACE INTO command_imports (source_key, imported_at, runs) VALUES (?1, ?2, ?3)";
const I3: &str = "SELECT 1 FROM deleted_commands WHERE text_hash = ?1";
const I4: &str = "SELECT COUNT(*) FROM command_imports";
const I5: &str = "SELECT 1 FROM commands c JOIN command_runs r ON r.command_id = c.id WHERE c.text = ?1 AND r.source = 'hook' LIMIT 1";

const T1_COMMANDS: &str = "SELECT COUNT(*) FROM commands";
const T1_RUNS: &str = "SELECT source, COUNT(*) FROM command_runs GROUP BY source";
const T1_PICKS: &str = "SELECT COUNT(*) FROM command_events WHERE action = 'pick'";
const T2: &str = "SELECT id, text, template FROM commands";
const T3: &str = "SELECT id, command_id, started_at, cwd, repo_root, exit_code, shell_session, prev_command_id, source FROM command_runs ORDER BY started_at, id";
const T4: &str = "SELECT id, timestamp, action, command_id, query, rank, session_id, subsession_id, episode_queries, cwd, repo_root, shell_session FROM command_events ORDER BY id";

// ---------------------------------------------------------------------------
// Errors without command text
// ---------------------------------------------------------------------------

/// Turns a rusqlite error into one that names the operation and the SQLite
/// error code, never `{e}`, whose text could embed a bound value.
trait SqlContext<T> {
    fn op(self, operation: &'static str) -> Result<T>;
}

impl<T> SqlContext<T> for rusqlite::Result<T> {
    fn op(self, operation: &'static str) -> Result<T> {
        self.map_err(|error| anyhow!("{} failed: {}", operation, describe(&error)))
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn describe(error: &rusqlite::Error) -> String {
    use rusqlite::ErrorCode;
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "database busy".to_string(),
        Some(ErrorCode::DiskFull) => "disk full".to_string(),
        Some(ErrorCode::ReadOnly) => "database is read-only".to_string(),
        Some(code) => format!("SQLite error {:?}", code),
        None => "unexpected result from SQLite".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Row types (none derives Debug: they hold command text)
// ---------------------------------------------------------------------------

/// Where a run came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// The shell hook, through `track-command`.
    Hook,
    /// A shell history file or Atuin.
    Import,
}

impl Source {
    fn as_sql(self) -> &'static str {
        match self {
            Source::Hook => "hook",
            Source::Import => "import",
        }
    }

    fn from_sql(text: &str) -> Self {
        match text {
            "hook" => Source::Hook,
            "import" => Source::Import,
            other => unreachable!("the CHECK constraint forbids source {:?}", other),
        }
    }
}

/// One execution to record.
pub(super) struct NewRun<'a> {
    pub text: &'a str,
    pub template: &'a str,
    pub started_at: i64,
    pub cwd: Option<&'a str>,
    pub repo_root: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub shell_session: &'a str,
    pub source: Source,
}

/// What `record_run` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunOutcome {
    Inserted {
        command_id: i64,
        run_id: i64,
    },
    /// An imported `(command, second)` that was already there.
    Duplicate,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct CommandRow {
    pub id: i64,
    pub text: String,
    pub last_run_at: i64,
    pub run_count: u32,
    pub success_count: u32,
    pub fail_count: u32,
    pub pick_count: u32,
    pub last_exit_code: Option<i32>,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct CwdRunsRow {
    pub command_id: i64,
    pub runs: u32,
    pub last_run_at: i64,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct CountRow {
    pub command_id: i64,
    pub runs: u32,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct RecentRunRow {
    pub command_id: i64,
    pub started_at: i64,
    pub exit_code: Option<i32>,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct FollowerRow {
    pub command_id: i64,
    pub template: String,
    pub runs: u32,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct PickRow {
    pub command_id: i64,
    pub query: String,
    pub timestamp: i64,
    pub episode_queries: Vec<String>,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct DirUse {
    pub dir: String,
    pub runs: u32,
    pub last_run_at: i64,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct PrecededBy {
    pub template: String,
    pub runs: u32,
}

/// `command_events.action`.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) enum EventAction {
    Impression,
    Pick,
}

impl EventAction {
    fn as_sql(self) -> &'static str {
        match self {
            EventAction::Impression => "impression",
            EventAction::Pick => "pick",
        }
    }
}

/// One `command_events` row to write; the timestamp is the `now` of
/// [`CmdStore::log_events`].
pub(super) struct NewEvent<'a> {
    pub action: EventAction,
    pub command_id: i64,
    pub query: &'a str,
    pub rank: Option<u32>,
    pub session_id: &'a str,
    pub subsession_id: u64,
    /// Picks only; written as a JSON array.
    pub episode_queries: Option<&'a [String]>,
    pub cwd: Option<&'a str>,
    pub repo_root: Option<&'a str>,
    pub shell_session: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct PurgeCounts {
    pub commands: u64,
    pub runs: u64,
    /// Whether the database was compacted and its write-ahead log emptied.
    /// When another connection held it, the deleted text can still be in
    /// `events.db` or `events.db-wal` until a later purge (or the last
    /// connection closing) clears it.
    pub log_cleared: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct StoreCounts {
    pub commands: u64,
    pub hook_runs: u64,
    pub import_runs: u64,
    pub picks: u64,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct CommandRecord {
    pub id: i64,
    pub text: String,
    pub template: String,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct RunRecord {
    pub id: i64,
    pub command_id: i64,
    pub started_at: i64,
    pub cwd: Option<String>,
    pub repo_root: Option<String>,
    pub exit_code: Option<i32>,
    pub shell_session: String,
    pub prev_command_id: Option<i64>,
    pub source: Source,
}

#[allow(
    dead_code,
    reason = "read by the picker (Phase B) or training (Phase C)"
)]
pub(super) struct EventRecord {
    pub id: i64,
    pub timestamp: i64,
    pub is_pick: bool,
    pub command_id: i64,
    pub query: String,
    pub rank: Option<u32>,
    pub session_id: String,
    pub subsession_id: u64,
    pub episode_queries: Vec<String>,
    pub cwd: Option<String>,
    pub repo_root: Option<String>,
    pub shell_session: Option<String>,
}

/// A JSON array of strings; anything else (NULL, junk) is an empty list.
fn parse_queries(json: Option<String>) -> Vec<String> {
    json.and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// The command tables of one `events.db`, behind one connection.
pub(super) struct CmdStore {
    db: Database,
}

#[allow(
    dead_code,
    reason = "the read path serves the picker (Phase B) and training (Phase C)"
)]
impl CmdStore {
    /// Create `data_dir` if needed, open its `events.db` (pragmas, schema),
    /// and make both private (section 10.4).
    pub(super) fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("could not create {}", data_dir.display()))?;
        let db = Database::new(&Database::get_db_path(data_dir))?;
        privacy::tighten_permissions(data_dir)?;
        Ok(CmdStore { db })
    }

    #[cfg(test)]
    pub(super) fn in_memory() -> Self {
        CmdStore {
            db: Database::new(Path::new(":memory:")).expect("an in-memory database opens"),
        }
    }

    fn conn(&self) -> &Connection {
        self.db.connection()
    }

    // --- Write path --------------------------------------------------------

    /// `BEGIN IMMEDIATE`; `f`; `COMMIT`. Rolls back when `f` returns `Err`.
    /// Never nested.
    pub(super) fn transaction<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let tx = Transaction::new_unchecked(self.conn(), TransactionBehavior::Immediate)
            .op("starting a transaction")?;
        let value = f(&tx)?; // dropping `tx` on the error path rolls back
        tx.commit().op("committing a transaction")?;
        Ok(value)
    }

    /// S1-S5 on an open transaction: make sure the command exists, link the
    /// run to its predecessor in the same shell session, insert it, and bump
    /// the command's aggregates only if the run was really inserted.
    pub(super) fn record_run_in(tx: &Transaction, run: &NewRun) -> Result<RunOutcome> {
        assert!(!run.text.is_empty(), "an empty command is never recorded");
        assert!(!run.shell_session.is_empty(), "every run has a session");
        const OP: &str = "recording a command";

        tx.prepare_cached(S1)
            .and_then(|mut s| s.execute(params![run.text, run.template, run.started_at]))
            .op(OP)?;
        let command_id: i64 = tx
            .prepare_cached(S2)
            .and_then(|mut s| s.query_row(params![run.text], |row| row.get(0)))
            .op(OP)?;
        let previous: Option<(i64, i64)> = tx
            .prepare_cached(S3)
            .and_then(|mut s| {
                s.query_row(params![run.shell_session, run.started_at], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()
            })
            .op(OP)?;
        let prev_command_id = previous
            .filter(|&(_, at)| at >= run.started_at - CMD_SEQUENCE_MAX_GAP_SECS)
            .map(|(id, _)| id);

        let inserted = tx
            .prepare_cached(S4)
            .and_then(|mut s| {
                s.execute(params![
                    command_id,
                    run.started_at,
                    run.cwd,
                    run.repo_root,
                    run.exit_code,
                    run.duration_ms,
                    run.shell_session,
                    prev_command_id,
                    run.source.as_sql(),
                ])
            })
            .op(OP)?;
        if inserted != 1 {
            return Ok(RunOutcome::Duplicate);
        }
        let run_id = tx.last_insert_rowid();

        let success = i64::from(run.exit_code == Some(0));
        let failure = i64::from(matches!(run.exit_code, Some(code) if code != 0));
        tx.prepare_cached(S5)
            .and_then(|mut s| {
                s.execute(params![
                    command_id,
                    success,
                    failure,
                    run.started_at,
                    run.exit_code
                ])
            })
            .op(OP)?;
        Ok(RunOutcome::Inserted { command_id, run_id })
    }

    /// One run, one transaction.
    pub(super) fn record_run(&self, run: &NewRun) -> Result<RunOutcome> {
        self.transaction(|tx| Self::record_run_in(tx, run))
    }

    // --- Read path at picker start -----------------------------------------

    pub(super) fn load_commands(&self) -> Result<Vec<CommandRow>> {
        self.rows(R1, [], "loading commands", |row| {
            Ok(CommandRow {
                id: row.get(0)?,
                text: row.get(1)?,
                last_run_at: row.get(2)?,
                run_count: row.get(3)?,
                success_count: row.get(4)?,
                fail_count: row.get(5)?,
                pick_count: row.get(6)?,
                last_exit_code: row.get(7)?,
            })
        })
    }

    pub(super) fn runs_in_cwd(&self, cwd: &str) -> Result<Vec<CwdRunsRow>> {
        self.rows(R2, params![cwd], "loading runs in a directory", |row| {
            Ok(CwdRunsRow {
                command_id: row.get(0)?,
                runs: row.get(1)?,
                last_run_at: row.get(2)?,
            })
        })
    }

    pub(super) fn runs_in_repo(&self, repo_root: &str) -> Result<Vec<CountRow>> {
        self.rows(
            R3,
            params![repo_root],
            "loading runs in a repository",
            |row| {
                Ok(CountRow {
                    command_id: row.get(0)?,
                    runs: row.get(1)?,
                })
            },
        )
    }

    pub(super) fn runs_since(&self, cutoff: i64) -> Result<Vec<RecentRunRow>> {
        self.rows(R4, params![cutoff], "loading recent runs", |row| {
            Ok(RecentRunRow {
                command_id: row.get(0)?,
                started_at: row.get(1)?,
                exit_code: row.get(2)?,
            })
        })
    }

    /// Templates of the session's last 3 runs, newest first.
    pub(super) fn session_tail(&self, shell_session: &str) -> Result<Vec<String>> {
        self.rows(
            R5,
            params![shell_session],
            "loading the session tail",
            |row| row.get(0),
        )
    }

    /// Runs whose previous command has one of `templates` (1 to 3, distinct).
    pub(super) fn followers(&self, templates: &[String]) -> Result<Vec<FollowerRow>> {
        assert!(
            (1..=3).contains(&templates.len()),
            "R6 takes 1 to 3 templates, got {}",
            templates.len()
        );
        self.rows(
            R6[templates.len() - 1],
            rusqlite::params_from_iter(templates),
            "loading followers",
            |row| {
                Ok(FollowerRow {
                    command_id: row.get(0)?,
                    template: row.get(1)?,
                    runs: row.get(2)?,
                })
            },
        )
    }

    pub(super) fn session_commands(&self, shell_session: &str) -> Result<Vec<i64>> {
        self.rows(
            R7,
            params![shell_session],
            "loading session commands",
            |row| row.get(0),
        )
    }

    pub(super) fn picks_since(&self, cutoff: i64) -> Result<Vec<PickRow>> {
        self.rows(R8, params![cutoff], "loading picks", |row| {
            Ok(PickRow {
                command_id: row.get(0)?,
                query: row.get(1)?,
                timestamp: row.get(2)?,
                episode_queries: parse_queries(row.get(3)?),
            })
        })
    }

    // --- Read path on selection change -------------------------------------

    pub(super) fn command_dirs(&self, command_id: i64) -> Result<Vec<DirUse>> {
        self.rows(
            R9,
            params![command_id],
            "loading a command's directories",
            |row| {
                Ok(DirUse {
                    dir: row.get(0)?,
                    runs: row.get(1)?,
                    last_run_at: row.get(2)?,
                })
            },
        )
    }

    pub(super) fn command_predecessors(&self, command_id: i64) -> Result<Vec<PrecededBy>> {
        self.rows(
            R10,
            params![command_id],
            "loading a command's predecessors",
            |row| {
                Ok(PrecededBy {
                    template: row.get(0)?,
                    runs: row.get(1)?,
                })
            },
        )
    }

    // --- Events, delete, purge ---------------------------------------------

    /// Write `events`, all stamped `now`, in one transaction.
    pub(super) fn log_events(&self, now: i64, events: &[NewEvent]) -> Result<()> {
        const OP: &str = "logging command events";
        self.transaction(|tx| {
            let mut statement = tx.prepare_cached(E1).op(OP)?;
            for event in events {
                let episode = event
                    .episode_queries
                    .map(|queries| serde_json::to_string(queries).expect("strings serialize"));
                statement
                    .execute(params![
                        now,
                        event.action.as_sql(),
                        event.command_id,
                        event.query,
                        event.rank,
                        event.session_id,
                        event.subsession_id as i64,
                        episode,
                        event.cwd,
                        event.repo_root,
                        event.shell_session,
                    ])
                    .op(OP)?;
            }
            Ok(())
        })
    }

    pub(super) fn count_pick(&self, command_id: i64) -> Result<()> {
        self.conn()
            .prepare_cached(E2)
            .and_then(|mut s| s.execute(params![command_id]))
            .op("counting a pick")?;
        Ok(())
    }

    /// Forget one command: a tombstone of its text's hash (so an import does
    /// not restore it), then its events, runs, links and row (D1-D5).
    pub(super) fn delete_command(&self, command_id: i64, text: &str, now: i64) -> Result<()> {
        const OP: &str = "deleting a command";
        self.transaction(|tx| {
            tx.prepare_cached(D1)
                .and_then(|mut s| s.execute(params![privacy::fnv1a64(text), now]))
                .op(OP)?;
            for statement in [D2, D3, D4, D5] {
                tx.prepare_cached(statement)
                    .and_then(|mut s| s.execute(params![command_id]))
                    .op(OP)?;
            }
            Ok(())
        })
    }

    /// Delete everything in the command tables (P1), then `VACUUM` so the
    /// text is gone from the file too. Returns what there was.
    pub(super) fn purge(&self) -> Result<PurgeCounts> {
        let before = self.counts()?;
        self.conn()
            .execute_batch(P1_SECURE)
            .op("turning on secure delete")?;
        self.transaction(|tx| tx.execute_batch(P1).op("deleting recorded commands"))?;
        // The rows are gone now. Another connection holding the database
        // busy is reported through `log_cleared`, so the caller still says
        // what was deleted.
        let vacuumed = match self.conn().execute_batch("VACUUM") {
            Err(error) if is_busy(&error) => false,
            result => {
                result.op("compacting the database")?;
                true
            }
        };
        let busy: i64 = self
            .conn()
            .query_row(P1_CHECKPOINT, [], |row| row.get(0))
            .op("emptying the write-ahead log")?;
        Ok(PurgeCounts {
            commands: before.commands,
            runs: before.hook_runs + before.import_runs,
            log_cleared: vacuumed && busy == 0,
        })
    }

    // --- Import bookkeeping -------------------------------------------------

    /// The earliest run the hook recorded; imports stop there.
    pub(super) fn first_hook_run(&self) -> Result<Option<i64>> {
        self.conn()
            .prepare_cached(I1)
            .and_then(|mut s| s.query_row([], |row| row.get(0)))
            .op("finding the first recorded run")
    }

    pub(super) fn import_done(&self, source_key: &str) -> Result<bool> {
        self.exists(I2_SELECT, params![source_key], "checking an import")
    }

    pub(super) fn mark_import(&self, source_key: &str, now: i64, runs: u64) -> Result<()> {
        self.conn()
            .prepare_cached(I2_INSERT)
            .and_then(|mut s| s.execute(params![source_key, now, runs as i64]))
            .op("marking an import")?;
        Ok(())
    }

    pub(super) fn is_tombstoned(&self, text_hash: i64) -> Result<bool> {
        self.exists(I3, params![text_hash], "checking deleted commands")
    }

    pub(super) fn import_count(&self) -> Result<u64> {
        self.count(I4, "counting imports")
    }

    pub(super) fn has_hook_run(&self, text: &str) -> Result<bool> {
        self.exists(I5, params![text], "checking recorded runs")
    }

    // --- Status and full reads ----------------------------------------------

    pub(super) fn counts(&self) -> Result<StoreCounts> {
        const OP: &str = "counting recorded commands";
        let mut counts = StoreCounts {
            commands: self.count(T1_COMMANDS, OP)?,
            hook_runs: 0,
            import_runs: 0,
            picks: self.count(T1_PICKS, OP)?,
        };
        let by_source: Vec<(String, i64)> =
            self.rows(T1_RUNS, [], OP, |row| Ok((row.get(0)?, row.get(1)?)))?;
        for (source, runs) in by_source {
            match Source::from_sql(&source) {
                Source::Hook => counts.hook_runs = runs as u64,
                Source::Import => counts.import_runs = runs as u64,
            }
        }
        Ok(counts)
    }

    pub(super) fn all_commands(&self) -> Result<Vec<CommandRecord>> {
        self.rows(T2, [], "reading commands", |row| {
            Ok(CommandRecord {
                id: row.get(0)?,
                text: row.get(1)?,
                template: row.get(2)?,
            })
        })
    }

    pub(super) fn all_runs(&self) -> Result<Vec<RunRecord>> {
        self.rows(T3, [], "reading runs", |row| {
            Ok(RunRecord {
                id: row.get(0)?,
                command_id: row.get(1)?,
                started_at: row.get(2)?,
                cwd: row.get(3)?,
                repo_root: row.get(4)?,
                exit_code: row.get(5)?,
                shell_session: row.get(6)?,
                prev_command_id: row.get(7)?,
                source: Source::from_sql(&row.get::<_, String>(8)?),
            })
        })
    }

    pub(super) fn all_events(&self) -> Result<Vec<EventRecord>> {
        self.rows(T4, [], "reading command events", |row| {
            Ok(EventRecord {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                is_pick: row.get::<_, String>(2)? == "pick",
                command_id: row.get(3)?,
                query: row.get(4)?,
                rank: row.get(5)?,
                session_id: row.get(6)?,
                subsession_id: row.get::<_, i64>(7)? as u64,
                episode_queries: parse_queries(row.get(8)?),
                cwd: row.get(9)?,
                repo_root: row.get(10)?,
                shell_session: row.get(11)?,
            })
        })
    }

    // --- Helpers --------------------------------------------------------------

    fn rows<T, P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
        operation: &'static str,
        map: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut statement = self.conn().prepare_cached(sql).op(operation)?;
        let rows = statement.query_map(params, map).op(operation)?;
        rows.collect::<rusqlite::Result<Vec<T>>>().op(operation)
    }

    fn exists<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
        operation: &'static str,
    ) -> Result<bool> {
        self.conn()
            .prepare_cached(sql)
            .and_then(|mut s| s.exists(params))
            .op(operation)
    }

    fn count(&self, sql: &str, operation: &'static str) -> Result<u64> {
        let count: i64 = self
            .conn()
            .prepare_cached(sql)
            .and_then(|mut s| s.query_row([], |row| row.get(0)))
            .op(operation)?;
        Ok(count as u64)
    }
}

#[cfg(test)]
pub(super) mod test_support {
    //! Shorthand for building runs in tests.
    use super::*;

    /// A hook run of `text` in `session` at `started_at`, with `exit_code`.
    pub(in crate::cmd) fn hook_run<'a>(
        text: &'a str,
        session: &'a str,
        started_at: i64,
        exit_code: Option<i32>,
    ) -> NewRun<'a> {
        NewRun {
            text,
            template: text,
            started_at,
            cwd: Some("/w"),
            repo_root: None,
            exit_code,
            duration_ms: None,
            shell_session: session,
            source: Source::Hook,
        }
    }

    pub(in crate::cmd) fn command_id(store: &CmdStore, text: &str) -> Option<i64> {
        store
            .conn()
            .query_row(S2, [text], |row| row.get(0))
            .optional()
            .unwrap()
    }

    /// `SELECT COUNT(*) FROM <table>`.
    pub(in crate::cmd) fn table_rows(store: &CmdStore, table: &str) -> i64 {
        store
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Any read-only query, one column of text per row joined by `|`.
    pub(in crate::cmd) fn query_strings(store: &CmdStore, sql: &str) -> Vec<String> {
        let mut statement = store.conn().prepare(sql).unwrap();
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
                            other => format!("{:?}", other),
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|cells| cells.join("|"))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn record(store: &CmdStore, run: &NewRun) -> RunOutcome {
        store.record_run(run).expect("records")
    }

    #[test]
    fn create_schema_is_idempotent() {
        let store = CmdStore::in_memory();
        for _ in 0..3 {
            create_schema(store.conn()).expect("runs again");
        }
        // Run the DDL itself again too, not just the early return.
        store.conn().execute_batch(SCHEMA).expect("IF NOT EXISTS");
        let indexes: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'idx_command%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 11);
    }

    #[test]
    fn an_older_database_gains_the_command_tables_and_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!("psychic-cmd-older-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = Database::get_db_path(&dir);
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE events (
                     id INTEGER PRIMARY KEY, timestamp INTEGER NOT NULL, query TEXT NOT NULL,
                     file_path TEXT NOT NULL, full_path TEXT NOT NULL, mtime INTEGER,
                     file_size INTEGER, subsession_id INTEGER, action TEXT NOT NULL,
                     session_id TEXT NOT NULL, episode_queries TEXT, rank INTEGER, is_dir INTEGER);
                 CREATE TABLE sessions (
                     session_id TEXT PRIMARY KEY, cwd TEXT NOT NULL, timezone TEXT NOT NULL,
                     created_at INTEGER NOT NULL);
                 INSERT INTO events (timestamp, query, file_path, full_path, action, session_id)
                     VALUES (1, 'q', 'f', '/f', 'click', 's');
                 INSERT INTO sessions VALUES ('s', '/tmp', 'UTC', 1);",
            )
            .unwrap();
        }

        let db = Database::new(&path).expect("an older database still opens");

        let count = |sql: &str| -> i64 {
            db.connection()
                .query_row(sql, [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(
            count("SELECT COUNT(*) FROM events WHERE full_path = '/f'"),
            1
        );
        assert_eq!(count("SELECT COUNT(*) FROM sessions WHERE cwd = '/tmp'"), 1);
        assert_eq!(
            count(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN
                 ('commands', 'command_runs', 'command_events', 'command_imports', 'deleted_commands')"
            ),
            5
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn aggregates_follow_every_run_and_out_of_order_runs_keep_the_last_exit_code() {
        let store = CmdStore::in_memory();
        record(&store, &hook_run("make", "s", 100, Some(0)));
        record(&store, &hook_run("make", "s", 200, Some(1)));
        record(&store, &hook_run("make", "s", 150, None));

        let row = store
            .load_commands()
            .unwrap()
            .into_iter()
            .find(|row| row.text == "make")
            .unwrap();
        assert_eq!(
            (
                row.run_count,
                row.success_count,
                row.fail_count,
                row.last_run_at,
                row.last_exit_code
            ),
            (3, 1, 1, 200, Some(1))
        );
        assert_eq!(
            query_strings(&store, "SELECT first_run_at FROM commands"),
            vec!["100"]
        );
    }

    fn prev_of(store: &CmdStore, text: &str) -> String {
        query_strings(
            store,
            &format!(
                "SELECT IFNULL((SELECT text FROM commands WHERE id = r.prev_command_id), 'NULL')
                 FROM command_runs r JOIN commands c ON c.id = r.command_id WHERE c.text = '{}'",
                text
            ),
        )
        .join(",")
    }

    #[test]
    fn the_previous_command_is_in_the_same_session_and_within_the_gap() {
        let store = CmdStore::in_memory();
        record(&store, &hook_run("A", "s", 100, Some(0)));
        record(&store, &hook_run("B", "s", 160, Some(0)));
        record(&store, &hook_run("C", "s", 160 + 1801, Some(0)));
        record(&store, &hook_run("D", "t", 170, Some(0)));
        record(&store, &hook_run("E", "s", 160 + 1801 + 1800, Some(0)));

        assert_eq!(prev_of(&store, "A"), "NULL");
        assert_eq!(prev_of(&store, "B"), "A");
        assert_eq!(prev_of(&store, "C"), "NULL", "1801 s is past the gap");
        assert_eq!(prev_of(&store, "D"), "NULL", "another session");
        assert_eq!(prev_of(&store, "E"), "C", "exactly 1800 s is linked");
    }

    #[test]
    fn an_import_of_the_same_command_and_second_is_a_duplicate() {
        let store = CmdStore::in_memory();
        let import = NewRun {
            source: Source::Import,
            ..hook_run("ls", "import:zsh:1", 100, None)
        };
        assert!(matches!(
            record(&store, &import),
            RunOutcome::Inserted { .. }
        ));
        assert_eq!(record(&store, &import), RunOutcome::Duplicate);
        assert_eq!(store.load_commands().unwrap()[0].run_count, 1);

        // Hook rows with the same pair are both kept.
        record(&store, &hook_run("ls", "s", 100, Some(0)));
        record(&store, &hook_run("ls", "s", 100, Some(0)));
        assert_eq!(store.load_commands().unwrap()[0].run_count, 3);
    }

    #[test]
    fn a_failed_transaction_rolls_back() {
        let store = CmdStore::in_memory();
        let result: Result<()> = store.transaction(|tx| {
            CmdStore::record_run_in(tx, &hook_run("ls", "s", 1, Some(0)))?;
            anyhow::bail!("stop")
        });
        assert!(result.is_err());
        assert_eq!(table_rows(&store, "commands"), 0);
        assert_eq!(table_rows(&store, "command_runs"), 0);
    }

    #[test]
    fn delete_command_removes_it_everywhere_and_leaves_a_tombstone() {
        let store = CmdStore::in_memory();
        for at in [100, 110, 120] {
            record(&store, &hook_run("A", "s", at, Some(0)));
        }
        record(&store, &hook_run("B", "s", 130, Some(0)));
        let a = command_id(&store, "A").unwrap();
        let b = command_id(&store, "B").unwrap();
        let event = |action| NewEvent {
            action,
            command_id: a,
            query: "a",
            rank: Some(1),
            session_id: "p",
            subsession_id: 1,
            episode_queries: None,
            cwd: None,
            repo_root: None,
            shell_session: None,
        };
        store
            .log_events(
                140,
                &[event(EventAction::Impression), event(EventAction::Pick)],
            )
            .unwrap();
        assert_eq!(prev_of(&store, "B"), "A");

        store.delete_command(a, "A", 150).unwrap();

        assert_eq!(command_id(&store, "A"), None);
        assert_eq!(
            query_strings(
                &store,
                &format!("SELECT COUNT(*) FROM command_runs WHERE command_id = {a}")
            ),
            vec!["0"]
        );
        assert_eq!(table_rows(&store, "command_events"), 0);
        assert_eq!(
            query_strings(
                &store,
                &format!(
                    "SELECT IFNULL(prev_command_id, 'NULL') FROM command_runs WHERE command_id = {b}"
                )
            ),
            vec!["NULL"]
        );
        assert!(store.is_tombstoned(privacy::fnv1a64("A")).unwrap());
        assert!(!store.is_tombstoned(privacy::fnv1a64("B")).unwrap());
    }

    #[test]
    fn purge_empties_every_command_table_and_reports_what_there_was() {
        let store = CmdStore::in_memory();
        record(&store, &hook_run("A", "s", 100, Some(0)));
        record(&store, &hook_run("A", "s", 101, Some(0)));
        record(&store, &hook_run("B", "s", 102, Some(0)));
        store.mark_import("zsh:/h", 1, 0).unwrap();
        store
            .delete_command(command_id(&store, "B").unwrap(), "B", 5)
            .unwrap();

        let counts = store.purge().unwrap();

        assert_eq!((counts.commands, counts.runs), (1, 2));
        for table in [
            "commands",
            "command_runs",
            "command_events",
            "command_imports",
            "deleted_commands",
        ] {
            assert_eq!(table_rows(&store, table), 0, "{table} is empty");
        }
    }

    const SENTINEL: &[u8] = b"SENTINEL_purge_5ca1ab1e";

    /// A store on disk holding enough runs of a sentinel command that its
    /// pages reach the write-ahead log, as a real history does.
    struct SentinelStore {
        dir: std::path::PathBuf,
        store: CmdStore,
    }

    fn store_with_sentinel(tag: &str) -> SentinelStore {
        let dir = std::env::temp_dir().join(format!("psychic-cmd-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = CmdStore::open(&dir).unwrap();
        let text = String::from_utf8(SENTINEL.to_vec()).unwrap();
        for i in 0..200 {
            let command = format!("echo {} {}", text, i);
            record(&store, &hook_run(&command, "s", 100 + i, Some(0)));
        }
        SentinelStore { dir, store }
    }

    /// The files of the database (`events.db` and its `-wal`) that still hold
    /// the sentinel's bytes.
    fn files_holding_sentinel(dir: &Path) -> Vec<String> {
        ["events.db", "events.db-wal"]
            .into_iter()
            .filter(|name| {
                std::fs::read(dir.join(name)).is_ok_and(|bytes| {
                    bytes
                        .windows(SENTINEL.len())
                        .any(|window| window == SENTINEL)
                })
            })
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn purge_leaves_no_command_text_on_disk() {
        let SentinelStore { dir, store } = store_with_sentinel("purge-disk");
        assert!(
            !files_holding_sentinel(&dir).is_empty(),
            "the text is there"
        );

        let counts = store.purge().unwrap();

        assert!(counts.log_cleared);
        assert_eq!(files_holding_sentinel(&dir), Vec::<String>::new());
        drop(store);
        assert_eq!(files_holding_sentinel(&dir), Vec::<String>::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn purge_leaves_no_command_text_on_disk_while_another_connection_is_open() {
        let SentinelStore { dir, store } = store_with_sentinel("purge-disk-other");
        // Another psychic process, idle between statements.
        let other = Connection::open(Database::get_db_path(&dir)).unwrap();
        let rows: i64 = other
            .query_row("SELECT COUNT(*) FROM command_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 200);

        let counts = store.purge().unwrap();

        assert!(counts.log_cleared);
        assert_eq!(files_holding_sentinel(&dir), Vec::<String>::new());
        drop(other);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn purge_reports_a_log_another_connection_keeps_it_from_emptying() {
        let SentinelStore { dir, store } = store_with_sentinel("purge-busy");
        // Another process in the middle of a read holds the log's pages.
        let other = Connection::open(Database::get_db_path(&dir)).unwrap();
        other.execute_batch("BEGIN").unwrap();
        let _: i64 = other
            .query_row("SELECT COUNT(*) FROM command_runs", [], |row| row.get(0))
            .unwrap();

        let counts = store.purge().unwrap();

        assert!(!counts.log_cleared);
        assert_eq!(table_rows(&store, "commands"), 0, "the rows are gone");
        other.execute_batch("COMMIT").unwrap();
        drop(other);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_read_path_returns_what_was_recorded() {
        let store = CmdStore::in_memory();
        let run = |text, at, cwd| NewRun {
            cwd,
            repo_root: Some("/r"),
            template: if text == "cd a" { "cd PATH" } else { text },
            ..hook_run(text, "s", at, Some(0))
        };
        record(&store, &run("cd a", 100, Some("/r")));
        record(&store, &run("make", 110, Some("/r/a")));
        record(&store, &run("cd a", 120, Some("/r")));
        record(&store, &run("make", 130, Some("/r/a")));
        let cd = command_id(&store, "cd a").unwrap();
        let make = command_id(&store, "make").unwrap();

        let in_a: Vec<(i64, u32, i64)> = store
            .runs_in_cwd("/r/a")
            .unwrap()
            .into_iter()
            .map(|row| (row.command_id, row.runs, row.last_run_at))
            .collect();
        assert_eq!(in_a, vec![(make, 2, 130)]);
        assert_eq!(store.runs_in_repo("/r").unwrap().len(), 2);
        assert_eq!(store.runs_since(115).unwrap().len(), 2);
        assert_eq!(
            store.session_tail("s").unwrap(),
            vec!["make", "cd PATH", "make"]
        );
        let followers: Vec<(i64, String, u32)> = store
            .followers(&["cd PATH".to_string()])
            .unwrap()
            .into_iter()
            .map(|row| (row.command_id, row.template, row.runs))
            .collect();
        assert_eq!(followers, vec![(make, "cd PATH".to_string(), 2)]);
        let mut session = store.session_commands("s").unwrap();
        session.sort();
        assert_eq!(session, vec![cd.min(make), cd.max(make)]);
        let dirs: Vec<(String, u32, i64)> = store
            .command_dirs(make)
            .unwrap()
            .into_iter()
            .map(|row| (row.dir, row.runs, row.last_run_at))
            .collect();
        assert_eq!(dirs, vec![("/r/a".to_string(), 2, 130)]);
        let before: Vec<(String, u32)> = store
            .command_predecessors(make)
            .unwrap()
            .into_iter()
            .map(|row| (row.template, row.runs))
            .collect();
        assert_eq!(before, vec![("cd PATH".to_string(), 2)]);
        assert_eq!(store.first_hook_run().unwrap(), Some(100));
        assert!(store.has_hook_run("make").unwrap());
        assert!(!store.has_hook_run("cargo").unwrap());
        assert_eq!(store.all_commands().unwrap().len(), 2);
        let runs = store.all_runs().unwrap();
        assert_eq!(runs.len(), 4);
        assert!(runs[1].prev_command_id == Some(cd) && runs[1].source == Source::Hook);
        assert_eq!(runs[1].shell_session, "s");
        assert_eq!(runs[0].cwd.as_deref(), Some("/r"));
        assert_eq!(runs[0].repo_root.as_deref(), Some("/r"));
        assert_eq!(
            (
                runs[0].id,
                runs[0].command_id,
                runs[0].started_at,
                runs[0].exit_code
            ),
            (1, cd, 100, Some(0))
        );
    }

    #[test]
    fn picks_and_events_round_trip() {
        let store = CmdStore::in_memory();
        record(&store, &hook_run("make", "s", 100, Some(0)));
        let make = command_id(&store, "make").unwrap();
        let episode = vec!["m".to_string(), "ma".to_string()];
        store
            .log_events(
                200,
                &[NewEvent {
                    action: EventAction::Pick,
                    command_id: make,
                    query: "ma",
                    rank: Some(2),
                    session_id: "picker",
                    subsession_id: 7,
                    episode_queries: Some(&episode),
                    cwd: Some("/w"),
                    repo_root: None,
                    shell_session: Some("s"),
                }],
            )
            .unwrap();
        store.count_pick(make).unwrap();

        let picks: Vec<(i64, String, i64, Vec<String>)> = store
            .picks_since(150)
            .unwrap()
            .into_iter()
            .map(|row| {
                (
                    row.command_id,
                    row.query,
                    row.timestamp,
                    row.episode_queries,
                )
            })
            .collect();
        assert_eq!(picks, vec![(make, "ma".to_string(), 200, episode.clone())]);
        assert!(store.picks_since(200).unwrap().is_empty());
        assert_eq!(store.load_commands().unwrap()[0].pick_count, 1);

        let events = store.all_events().unwrap();
        let event = &events[0];
        assert!(event.is_pick);
        assert_eq!(
            (
                event.id,
                event.timestamp,
                event.command_id,
                event.rank,
                event.subsession_id
            ),
            (1, 200, make, Some(2), 7)
        );
        assert_eq!(
            (event.query.as_str(), event.session_id.as_str()),
            ("ma", "picker")
        );
        assert_eq!(event.episode_queries, episode);
        assert_eq!(
            (
                event.cwd.as_deref(),
                event.repo_root.as_deref(),
                event.shell_session.as_deref()
            ),
            (Some("/w"), None, Some("s"))
        );
        assert_eq!(store.counts().unwrap().picks, 1);
    }

    #[test]
    fn unparseable_episode_queries_read_as_empty() {
        assert_eq!(
            parse_queries(Some("not json".to_string())),
            Vec::<String>::new()
        );
        assert_eq!(parse_queries(None), Vec::<String>::new());
        assert_eq!(parse_queries(Some(r#"["a"]"#.to_string())), vec!["a"]);
    }

    #[test]
    fn counts_split_runs_by_source_and_imports_are_remembered() {
        let store = CmdStore::in_memory();
        record(&store, &hook_run("A", "s", 100, Some(0)));
        record(
            &store,
            &NewRun {
                source: Source::Import,
                ..hook_run("B", "i", 50, None)
            },
        );
        let counts = store.counts().unwrap();
        assert_eq!(
            (
                counts.commands,
                counts.hook_runs,
                counts.import_runs,
                counts.picks
            ),
            (2, 1, 1, 0)
        );
        assert!(!store.import_done("zsh:/h/.zsh_history").unwrap());
        assert_eq!(store.import_count().unwrap(), 0);
        store.mark_import("zsh:/h/.zsh_history", 10, 1).unwrap();
        assert!(store.import_done("zsh:/h/.zsh_history").unwrap());
        assert_eq!(store.import_count().unwrap(), 1);
    }
}

#[cfg(test)]
mod plan_tests {
    //! Every statement that must use an index, and the index it must use.
    //! A partial index is only used when SQLite can prove the query's WHERE
    //! implies the index's, and that proof breaks silently.
    use super::*;

    fn plan(store: &CmdStore, sql: &str) -> String {
        let mut statement = store
            .conn()
            .prepare(&format!("EXPLAIN QUERY PLAN {}", sql))
            .expect("query should parse");
        let nulls = vec![rusqlite::types::Null; statement.parameter_count()];
        statement
            .query_map(rusqlite::params_from_iter(nulls), |row| {
                row.get::<_, String>(3)
            })
            .expect("plan should run")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("plan rows")
            .join(" | ")
    }

    #[test]
    fn every_statement_uses_its_index() {
        // S1 and S4 are plain INSERTs, for which SQLite reports no plan.
        let store = CmdStore::in_memory();
        let cases: &[(&str, &str, &[&str])] = &[
            ("S2", S2, &["sqlite_autoindex_commands_1"]),
            ("S3", S3, &["idx_command_runs_session"]),
            ("S5", S5, &["INTEGER PRIMARY KEY"]),
            ("R1", R1, &["SCAN commands"]),
            ("R2", R2, &["USING COVERING INDEX idx_command_runs_cwd"]),
            ("R3", R3, &["USING COVERING INDEX idx_command_runs_repo"]),
            ("R4", R4, &["USING COVERING INDEX idx_command_runs_time"]),
            ("R5", R5, &["idx_command_runs_session"]),
            (
                "R6/1",
                R6[0],
                &[
                    "idx_commands_template",
                    "USING COVERING INDEX idx_command_runs_prev",
                ],
            ),
            (
                "R6/2",
                R6[1],
                &[
                    "idx_commands_template",
                    "USING COVERING INDEX idx_command_runs_prev",
                ],
            ),
            (
                "R6/3",
                R6[2],
                &[
                    "idx_commands_template",
                    "USING COVERING INDEX idx_command_runs_prev",
                ],
            ),
            ("R7", R7, &["idx_command_runs_session"]),
            ("R8", R8, &["idx_command_events_pick"]),
            ("R9", R9, &["idx_command_runs_command"]),
            ("R10", R10, &["idx_command_runs_command"]),
            ("D2", D2, &["idx_command_events_command"]),
            ("D3", D3, &["idx_command_runs_command"]),
            ("D4", D4, &["idx_command_runs_prev"]),
            ("I1", I1, &["idx_command_runs_hook"]),
        ];
        for (id, sql, wanted) in cases {
            let plan = plan(&store, sql);
            for needle in *wanted {
                assert!(plan.contains(needle), "{id} must use {needle}: {plan}");
            }
        }
        let r5 = plan(&store, R5);
        assert!(!r5.contains("TEMP B-TREE"), "R5 must not sort: {r5}");
    }
}
