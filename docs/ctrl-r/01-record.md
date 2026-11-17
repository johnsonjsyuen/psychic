# Phase A: Record (Implementation)

Document type: **Implementation** (HOW). Strategy and targets:
[00-blueprint.md](00-blueprint.md). This document is the single source for the
command schema, every SQL statement, `psychic track-command`, the shell
recording hooks, privacy, and the history import.

## 1. Scope, inputs, done when

| | |
|---|---|
| Builds | Tables, `track-command`, recording hooks, opt-in, permissions, purge, backfill, `psychic history --enable/--disable/--status/--import/--purge` |
| Inputs (must exist) | Commit `8b6246e`; nothing else |
| Leaves for later phases | `psychic history` with no action flag and `--retrain` exit with `psychic history: not built yet (Phase B)` / `(Phase C)` via `anyhow::bail!` |

Done when all of these pass, in this order:

```bash
just build                                   # clean, no warnings
cargo test                                   # all tests, old and new
cargo clippy                                 # clean
cargo test cmd::                             # the new unit tests by themselves
cargo test --test shell_hooks -- --nocapture # prints "skipped: <shell> not installed" per missing shell
./bench/train_parity.py                      # still "identical: N trees, byte for byte" (nothing in training changed)
./bench/run.py setup HEAD~1 && ./bench/run.py startup 6   # metric B9 (00-blueprint.md section 2)
D=/tmp/psychic-bench/cmd-a; rm -rf $D
cargo run --release -- --data-dir $D history --enable --no-import
cargo run --release -- --data-dir $D track-command --cwd /tmp --exit-code 0 --duration-ms 12 --session s1 -- 'echo hello'
cargo run --release -- --data-dir $D history --status      # prints "commands: 1" and "runs: 1 (hook 1, import 0)"
stat -c '%a' $D $D/events.db                               # 700 and 600
```

The commit message carries the `startup 6` table ([llm.md:54-62](../../llm.md)).

## 2. Files touched

New files: `src/cmd/{mod,cli,tuning,store,template,repo,privacy,track,backfill}.rs`,
`tests/shell_hooks.rs`, `tests/no_text_leaks.rs`, `tests/support/mod.rs`.

| Upstream file | Exact edit |
|---|---|
| [src/main.rs:1-26](../../src/main.rs) | Add `mod cmd;` after `mod cli;` |
| [src/main.rs:221-227](../../src/main.rs) | After `data_dir` is computed (221-224) and before `create_session_id()` (227), insert block M1 below |
| [src/main.rs:273](../../src/main.rs) (the `match command`) | Add arms M2 below |
| [src/cli.rs:84-88](../../src/cli.rs) | After `TrackVisit`, add variants C1 below |
| [src/db.rs:293](../../src/db.rs) | Replace `Self::migrate(conn)` with `Self::migrate(conn)?; crate::cmd::store::create_schema(conn)` |
| [src/search_worker.rs:2139-2165](../../src/search_worker.rs) | In `test_an_empty_data_directory_gets_the_whole_schema`, the expected tables become `["command_events", "command_imports", "command_runs", "commands", "deleted_commands", "events", "hidden_prefixes", "import_runs", "imported_paths", "sessions"]` and the expected indexes become `["idx_command_events_command", "idx_command_events_pick", "idx_command_runs_command", "idx_command_runs_cwd", "idx_command_runs_hook", "idx_command_runs_import", "idx_command_runs_prev", "idx_command_runs_repo", "idx_command_runs_session", "idx_command_runs_time", "idx_commands_template", "idx_events_action", "idx_events_engagement"]` (both sorted; the test's `names` helper already excludes `sqlite_%`). No other line of the file changes |
| [src/import/mod.rs:17](../../src/import/mod.rs) | `mod shell_history;` -> `pub(crate) mod shell_history;`; add `is_dir_within_limits` (section 12.2) |
| [src/import/shell_history.rs](../../src/import/shell_history.rs) | Section 12.2 |
| [src/shell_init.rs:328-331](../../src/shell_init.rs) | Section 10.2 |
| `shell/psychic.zsh`, `shell/psychic.bash`, `shell/psychic.fish` | Append the blocks of section 11 at end of file; in `shell/psychic.bash`, `__psychic_hook` also returns the status it received (section 11.3) |
| `docs/how-it-works.md` | New section "Shell history search: `src/cmd/`" describing Phase A |

M1 (`track-command` never reaches the logger set-up at [src/main.rs:229-265](../../src/main.rs)):

```rust
// Runs on every prompt, in the background. It never sets up the logger and
// never opens app.log: speed, and command text cannot reach a log that is not open.
if let Some(Commands::TrackCommand(args)) = &cli.command {
    return cmd::run_track_command(data_dir.as_deref(), args);
}
```

M2:

```rust
Commands::TrackCommand(_) => unreachable!("handled before the logger is set up"),
Commands::History(args) => {
    drop(log_rx); // nobody drains it in command mode; fern's sink then drops sends (main.rs:255-262)
    return cmd::run_history(&data_dir, args);
}
```

C1:

```rust
/// Search your shell history (what Ctrl-R runs), or manage what is recorded
///
/// Off until `psychic history --enable`. Nothing leaves your machine.
History(crate::cmd::cli::HistoryArgs),
/// Record a command that just ran (for shell integration hooks)
TrackCommand(crate::cmd::cli::TrackCommandArgs),
```

`src/help.rs` needs no edit: `cli_commands()` lists every clap subcommand
([src/help.rs:121-134](../../src/help.rs)), and `shell_commands` reads only `shell/psychic.zsh` and skips any function name
that is not purely ASCII alphanumeric ([src/help.rs:80-96](../../src/help.rs)),
so the `__psychic_*` functions added in section 11 do not appear on the
file-mode help screen. `history` and `track-command` do appear in its
"Command line" block, with the doc comments of C1.

`src/cmd/mod.rs` public surface (everything else in `src/cmd/` is `pub(super)` or private):

```rust
pub mod cli; pub mod store;            // store: only `create_schema` is used from outside
pub fn run_track_command(data_dir: Option<&Path>, args: &cli::TrackCommandArgs) -> anyhow::Result<()>;
pub fn run_history(data_dir: &Path, args: cli::HistoryArgs) -> anyhow::Result<()>;
```

The `history` actions of section 13 (`enable`, `disable`, `status`, `purge`, and the import call) are private functions in `mod.rs` that write to a `&mut dyn Write` and read the environment through a small `Env` struct, so tests drive them without touching the process environment. Items specified here but first read by a later phase (the R/T read path and its row types, `recording_enabled`, `word_count`, `CMD_EVENT_WINDOW_SECS`) carry `#[allow(dead_code, reason = "...")]` naming that phase; the phase that starts using an item removes its attribute.

Commit D0 adds `pub mod bench;` and `pub fn run_bench` ([04 section 2](04-bench.md#2-files-touched-and-the-two-commits)).

## 3. Constants (Phase A)

All in `src/cmd/tuning.rs`, each `pub(super) const` with a doc comment.

| Constant | Type | Value | Meaning |
|---|---|---|---|
| `CMD_MAX_TEXT_BYTES` | `usize` | `8192` | A command longer than this (after trimming) is not recorded |
| `CMD_SEQUENCE_MAX_GAP_SECS` | `i64` | `1800` | A run's predecessor counts as "previous command" only if it started at most this long before, in the same shell session |
| `CMD_MARKER_FILE` | `&str` | `"history-enabled"` | Opt-in marker in the data dir |
| `CMD_EVENT_WINDOW_SECS` | `i64` | `2_592_000` | 30 days: how far back individual runs (R4) and picks (R8) are loaded |
| `CMD_OWN_COMMANDS` | `&[&str]` | `["psychic", "p", "pd", "pc"]` | First words that are never recorded |
| `CMD_PATH_ARG_COMMANDS` | `&[&str]` | `["cd","pushd","ls","ll","la","cat","less","more","bat","vim","vi","nvim","nano","emacs","hx","code","rm","rmdir","mkdir","touch","cp","mv","ln","chmod","chown","source",".","open","xdg-open","head","tail","wc","du","tree"]` | Commands whose every non-flag argument is a path for templating |
| `CMD_WAL_TRUNCATE_BYTES` | `u64` | `2 * 1024 * 1024` | `track-command` checkpoints and truncates `events.db-wal` (W1) once it is longer than this (section 9.1) |

## 4. CLI surface

`src/cmd/cli.rs`. This table is the single definition of every flag of both
subcommands; the Owner column says which document specifies the behaviour.

### 4.1 `psychic track-command`

```rust
#[derive(clap::Args, Debug)]
pub struct TrackCommandArgs {
    /// Directory the command was typed in
    #[arg(long)] pub cwd: std::path::PathBuf,   // any bytes: a non-UTF-8 $PWD still parses (stored lossily, section 9)
    /// Exit status of the command
    #[arg(long, allow_negative_numbers = true)] pub exit_code: Option<i32>,
    /// When it started, Unix seconds
    #[arg(long, allow_negative_numbers = true)] pub started_at: Option<i64>,
    /// How long it ran, in milliseconds
    #[arg(long, allow_negative_numbers = true)] pub duration_ms: Option<i64>,   // a negative value (clock stepped back) is stored as NULL
    /// Identifies the shell that ran it
    #[arg(long)] pub session: String,
    /// The command text, after `--`
    #[arg(last = true, required = true)] pub command: std::ffi::OsString,
}
```

Invocation form (the only one the hooks use):
`psychic [--data-dir DIR] track-command --cwd DIR --exit-code N [--started-at S] [--duration-ms MS] --session ID -- TEXT`.
`TEXT` is exactly one argv element; it may contain newlines, quotes, non-ASCII
and a leading `-`.

### 4.2 `psychic history`

```rust
#[derive(clap::Args, Debug)]
#[command(group(clap::ArgGroup::new("action").multiple(false)
    .args(["enable", "disable", "status", "import", "purge", "retrain"])))]
pub struct HistoryArgs {
    #[arg(long, value_name = "QUERY", default_value = "")] pub query: String,
    #[arg(long, value_name = "ID")] pub session: Option<String>,
    #[arg(long, value_name = "DIR")] pub cwd: Option<String>,
    #[arg(long)] pub enable: bool,
    #[arg(long, requires = "enable")] pub no_import: bool,
    #[arg(long)] pub disable: bool,
    #[arg(long)] pub status: bool,
    #[arg(long)] pub import: bool,
    #[arg(long)] pub purge: bool,
    #[arg(long, short)] pub yes: bool,
    #[arg(long)] pub retrain: bool,
    #[arg(long, requires = "retrain")] pub quiet: bool,
}
```

| Flag | Behaviour | Owner |
|---|---|---|
| (none of the action flags) | Open the picker | [02 section 4](02-browse.md#4-picker-invocation) |
| `--query`, `--session`, `--cwd` | Picker inputs | [02 section 4](02-browse.md#4-picker-invocation) |
| `--enable [--no-import]` | Section 13.1 | this document |
| `--disable` | Section 13.2 | this document |
| `--status` | Section 13.3 | this document |
| `--import` | Section 12 | this document |
| `--purge [--yes]` | Section 10.5 | this document |
| `--retrain [--quiet]` | Train the command model | [03 section 6](03-learn.md#6-fit-and-model-files) |

Every doc comment on these fields is one sentence; clap prints them in `--help`.

## 5. Schema

Same file `events.db`. Created idempotently, no `user_version`, like the
existing tables ([src/db.rs:219-294](../../src/db.rs)). All times are Unix
seconds (ADR-6). Foreign keys are not enforced (the connection never sets
`PRAGMA foreign_keys`); deletes are explicit (section 6, D1-D5).

### 5.1 `commands`: one row per exact command text

```sql
CREATE TABLE IF NOT EXISTS commands (
    id             INTEGER PRIMARY KEY,
    text           TEXT    NOT NULL UNIQUE,
    template       TEXT    NOT NULL,
    first_run_at   INTEGER NOT NULL,
    last_run_at    INTEGER NOT NULL,
    run_count      INTEGER NOT NULL DEFAULT 0,
    success_count  INTEGER NOT NULL DEFAULT 0,   -- runs with exit_code = 0
    fail_count     INTEGER NOT NULL DEFAULT 0,   -- runs with exit_code <> 0
    pick_count     INTEGER NOT NULL DEFAULT 0,   -- Ctrl-R picks, all time
    last_exit_code INTEGER                       -- exit code of the run with the greatest started_at; NULL = unknown
);
CREATE INDEX IF NOT EXISTS idx_commands_template ON commands(template);
```

`last_exit_code` is one column more than the brief listed; it exists so the
`last_exit_nonzero` feature and the row marker need no scan of `command_runs`.
Runs with `exit_code IS NULL` count in `run_count` and in neither of
`success_count`/`fail_count`.

### 5.2 `command_runs`: one row per execution

```sql
CREATE TABLE IF NOT EXISTS command_runs (
    id              INTEGER PRIMARY KEY,
    command_id      INTEGER NOT NULL,
    started_at      INTEGER NOT NULL,
    cwd             TEXT,                 -- logical $PWD the command was typed in; NULL = unknown
    repo_root       TEXT,                 -- section 8; NULL = not in a repository or unknown
    exit_code       INTEGER,              -- NULL = unknown
    duration_ms     INTEGER,              -- NULL = unknown
    shell_session   TEXT    NOT NULL,     -- section 11.1 for hooks, section 12.3 for imports
    prev_command_id INTEGER,              -- section 9, step 7; NULL = none
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
```

| Index | Serves |
|---|---|
| `idx_command_runs_cwd` | R2: runs in a given cwd (covering) |
| `idx_command_runs_repo` | R3: runs in a given repo root (covering) |
| `idx_command_runs_time` | R4: runs since a timestamp (covering) |
| `idx_command_runs_prev` | R6: runs following a given previous template (covering); D4 |
| `idx_command_runs_session` | S3, R5, R7: the tail and the set of one shell session, in `(started_at, rowid)` order |
| `idx_command_runs_command` | R9, R10, D3: one command's runs |
| `idx_command_runs_hook` | I1: earliest hook-recorded run |
| `idx_command_runs_import` | Import idempotence: an imported `(command, second)` exists once |

### 5.3 `command_events`, `command_imports`, `deleted_commands`

```sql
CREATE TABLE IF NOT EXISTS command_events (
    id              INTEGER PRIMARY KEY,
    timestamp       INTEGER NOT NULL,
    action          TEXT    NOT NULL CHECK (action IN ('impression', 'pick')),
    command_id      INTEGER NOT NULL,
    query           TEXT    NOT NULL,
    rank            INTEGER,              -- 1-based row position; set on impressions and on picks
    session_id      TEXT    NOT NULL,     -- one per `psychic history` process
    subsession_id   INTEGER NOT NULL,     -- the query_id the row was shown under
    episode_queries TEXT,                 -- JSON array of strings; picks only
    cwd             TEXT,                 -- picks only: the picker's cwd
    repo_root       TEXT,                 -- picks only
    shell_session   TEXT                  -- picks only: the --session value
);
CREATE INDEX IF NOT EXISTS idx_command_events_pick    ON command_events(timestamp, command_id) WHERE action = 'pick';
CREATE INDEX IF NOT EXISTS idx_command_events_command ON command_events(command_id);

CREATE TABLE IF NOT EXISTS command_imports (
    source_key     TEXT PRIMARY KEY,      -- section 12.1
    imported_at    INTEGER NOT NULL,
    runs           INTEGER NOT NULL       -- rows inserted by the most recent import of this source
);

CREATE TABLE IF NOT EXISTS deleted_commands (
    text_hash      INTEGER PRIMARY KEY,   -- fnv1a64(text) reinterpreted as i64 (section 10.5)
    deleted_at     INTEGER NOT NULL
);
```

Rows are written to `command_events` only by Phase B
([02 section 10](02-browse.md#10-event-logging)); the table is created here so
the schema lands in one commit.

### 5.4 Where the schema is created

```rust
// src/cmd/store.rs
pub fn create_schema(conn: &rusqlite::Connection) -> anyhow::Result<()>
```

1. Run `SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'deleted_commands'`.
   If a row comes back, return `Ok(())` (`deleted_commands` is the last object
   created, so its presence means all exist).
2. Otherwise run every statement of 5.1-5.3, in document order, as one
   `conn.execute_batch`.

It is called from `Database::prepare_schema` (section 2), so it runs once per
database file per process ([src/db.rs:154-155](../../src/db.rs)) and costs one
indexed lookup on every later process.

## 6. SQL statements

`src/cmd/store.rs` owns all of them. No other file contains SQL on these
tables. Every statement uses `prepare_cached`.

```rust
pub(super) struct CmdStore { db: crate::db::Database }
impl CmdStore {
    /// `create_dir_all(data_dir)`, then `Database::new(&Database::get_db_path(data_dir))`
    /// (pragmas, schema), then `privacy::tighten_permissions(data_dir)` (section 10.4).
    pub(super) fn open(data_dir: &Path) -> anyhow::Result<Self>;
    /// `open`, then `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` on this connection (section 9.1). `track-command` only.
    pub(super) fn open_for_recording(data_dir: &Path) -> anyhow::Result<Self>;
    /// Consumes the store: W1 when `events.db-wal` is longer than `limit` bytes, then close (section 9.1).
    pub(super) fn close_recording(self, limit: u64) -> anyhow::Result<LogBound>;
    #[cfg(test)] pub(super) fn in_memory() -> Self;   // Database::new(Path::new(":memory:"))
}
/// Within: log not over `limit`, left alone. Truncated: W1 returned busy = 0. Busy: W1 returned busy = 1.
pub(super) enum LogBound { Within, Truncated, Busy }
```

### 6.1 Write path (S)

```rust
impl CmdStore {
    /// BEGIN IMMEDIATE; `f`; COMMIT. Rolls back when `f` returns Err. Never nested. Built with
    /// `rusqlite::Transaction::new_unchecked(self.db.connection(), TransactionBehavior::Immediate)`,
    /// because `Database::connection()` hands out `&Connection`.
    pub(super) fn transaction<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> anyhow::Result<T>) -> anyhow::Result<T>;
    /// S1-S5 on an open transaction. No BEGIN of its own.
    pub(super) fn record_run_in(tx: &rusqlite::Transaction, run: &NewRun) -> anyhow::Result<RunOutcome>;
    /// `self.transaction(|tx| Self::record_run_in(tx, run))`: one run, one transaction.
    pub(super) fn record_run(&self, run: &NewRun) -> anyhow::Result<RunOutcome>;
}
pub(super) enum RunOutcome { Inserted { command_id: i64, run_id: i64 }, Duplicate }
```

`track-command` calls `record_run`. Bulk writers (backfill, the benchmark
corpora) call `transaction` once per batch and `record_run_in` per run.

```rust
pub(super) struct NewRun<'a> {
    pub text: &'a str, pub template: &'a str, pub started_at: i64,
    pub cwd: Option<&'a str>, pub repo_root: Option<&'a str>,
    pub exit_code: Option<i32>, pub duration_ms: Option<i64>,
    pub shell_session: &'a str, pub source: Source,          // enum Source { Hook, Import }
}
```

| ID | Statement | Expected plan contains |
|---|---|---|
| S1 | `INSERT INTO commands (text, template, first_run_at, last_run_at) VALUES (?1, ?2, ?3, ?3) ON CONFLICT(text) DO NOTHING` | - (SQLite prints no plan for a plain `INSERT`; the `ON CONFLICT` check uses `sqlite_autoindex_commands_1`) |
| S2 | `SELECT id FROM commands WHERE text = ?1` | `sqlite_autoindex_commands_1` |
| S3 | `SELECT command_id, started_at FROM command_runs WHERE shell_session = ?1 AND started_at <= ?2 ORDER BY started_at DESC, id DESC LIMIT 1` | `idx_command_runs_session` |
| S4 | `INSERT OR IGNORE INTO command_runs (command_id, started_at, cwd, repo_root, exit_code, duration_ms, shell_session, prev_command_id, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)` | - |
| S5 | `UPDATE commands SET run_count = run_count + 1, success_count = success_count + ?2, fail_count = fail_count + ?3, first_run_at = MIN(first_run_at, ?4), last_exit_code = CASE WHEN ?4 >= last_run_at THEN ?5 ELSE last_exit_code END, last_run_at = MAX(last_run_at, ?4) WHERE id = ?1` | `INTEGER PRIMARY KEY` |

Rules: `prev_command_id` = S3's `command_id` when S3 returns a row with
`started_at >= ?2 - CMD_SEQUENCE_MAX_GAP_SECS`, else NULL. S5 runs only when
`conn.changes() == 1` after S4 (an ignored duplicate import changes nothing and
returns `RunOutcome::Duplicate`). In S5, `?2` is 1 when `exit_code == Some(0)`
else 0; `?3` is 1 when `exit_code` is `Some(n)` with `n != 0` else 0. SQLite
evaluates every right-hand side of an `UPDATE ... SET` against the row as it
was before the update, so the order of assignments does not matter.

### 6.2 Read path at picker start (R), each run exactly once per process

`?now` is the picker's start time.

| ID | Statement | Expected plan contains |
|---|---|---|
| R1 ids | `SELECT (SELECT MIN(id) FROM commands), (SELECT MAX(id) FROM commands)` (two subqueries: SQLite finds a lone `MIN`/`MAX` of the rowid at one end of the tree, but scans for both in one `SELECT`) | `SEARCH commands` |
| R1 | `SELECT id, text, last_run_at, run_count, success_count, fail_count, pick_count, last_exit_code FROM commands WHERE id BETWEEN ?1 AND ?2` | `SEARCH commands USING INTEGER PRIMARY KEY` (together, the parts read every row: the one deliberate full read, split by id so several connections share it, [02 section 5.2](02-browse.md#52-load-picker-start)) |
| R2 | `SELECT command_id, COUNT(*), MAX(started_at) FROM command_runs WHERE cwd = ?1 GROUP BY command_id` | `USING COVERING INDEX idx_command_runs_cwd` |
| R3 | `SELECT command_id, COUNT(*) FROM command_runs WHERE repo_root = ?1 GROUP BY command_id` | `USING COVERING INDEX idx_command_runs_repo` |
| R4 | `SELECT command_id, started_at, exit_code FROM command_runs WHERE started_at > ?1 ORDER BY started_at` with `?1 = now - CMD_EVENT_WINDOW_SECS` | `USING COVERING INDEX idx_command_runs_time` |
| R5 | `SELECT c.template FROM command_runs r JOIN commands c ON c.id = r.command_id WHERE r.shell_session = ?1 ORDER BY r.started_at DESC, r.id DESC LIMIT 3` | `idx_command_runs_session`, no `TEMP B-TREE` |
| R6 | `SELECT r.command_id, p.template, COUNT(*) FROM commands p CROSS JOIN command_runs r ON r.prev_command_id = p.id WHERE p.template IN (?1[, ?2[, ?3]]) GROUP BY r.command_id, p.template` | `idx_commands_template` and `USING COVERING INDEX idx_command_runs_prev` |
| R7 | `SELECT DISTINCT command_id FROM command_runs WHERE shell_session = ?1` | `idx_command_runs_session` |
| R8 | `SELECT command_id, query, timestamp, episode_queries FROM command_events WHERE action = 'pick' AND timestamp > ?1 ORDER BY timestamp, id` with `?1 = now - CMD_EVENT_WINDOW_SECS` | `idx_command_events_pick` |

R2 is skipped when the cwd is unknown, R3 when there is no repo root, R5-R7
when `--session` is absent, R6 when R5 returned no row. R6 is built with exactly
as many placeholders as R5 returned distinct templates (1 to 3). `CROSS JOIN`
fixes the join order (SQLite never reorders a `CROSS JOIN`).

### 6.3 Read path on selection change (preview details), on the worker thread

| ID | Statement | Expected plan contains |
|---|---|---|
| R9 | `SELECT cwd, COUNT(*), MAX(started_at) FROM command_runs WHERE command_id = ?1 AND cwd IS NOT NULL GROUP BY cwd ORDER BY MAX(started_at) DESC LIMIT 5` | `idx_command_runs_command` |
| R10 | `SELECT p.template, COUNT(*) FROM command_runs r JOIN commands p ON p.id = r.prev_command_id WHERE r.command_id = ?1 GROUP BY p.template ORDER BY COUNT(*) DESC, p.template LIMIT 3` | `idx_command_runs_command` |

### 6.4 Events, delete, purge, import, status

| ID | Statement | Used by |
|---|---|---|
| E1 | `INSERT INTO command_events (timestamp, action, command_id, query, rank, session_id, subsession_id, episode_queries, cwd, repo_root, shell_session) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)` | [02 section 10](02-browse.md#10-event-logging) |
| E2 | `UPDATE commands SET pick_count = pick_count + 1 WHERE id = ?1` | pick |
| D1 | `INSERT OR REPLACE INTO deleted_commands (text_hash, deleted_at) VALUES (?1, ?2)` | delete one |
| D2 | `DELETE FROM command_events WHERE command_id = ?1` (plan: `idx_command_events_command`) | delete one |
| D3 | `DELETE FROM command_runs WHERE command_id = ?1` (plan: `idx_command_runs_command`) | delete one |
| D4 | `UPDATE command_runs SET prev_command_id = NULL WHERE prev_command_id = ?1` (plan: `idx_command_runs_prev`) | delete one |
| D5 | `DELETE FROM commands WHERE id = ?1` | delete one |
| P1 | `PRAGMA secure_delete = ON`; then in one transaction `DELETE FROM command_events; DELETE FROM command_runs; DELETE FROM commands; DELETE FROM command_imports;` (`deleted_commands` is kept, section 10.5); then `VACUUM`; then `PRAGMA wal_checkpoint(TRUNCATE)`, whose first column (busy) must be 0 (section 10.5) | purge |
| W1 | `PRAGMA busy_timeout` set to 0 on this connection; then `PRAGMA wal_checkpoint(TRUNCATE)`; busy (first column) 1 is not an error (section 9.1) | `track-command`, once `events.db-wal` is over `CMD_WAL_TRUNCATE_BYTES` |
| I1 | `SELECT MIN(started_at) FROM command_runs WHERE source = 'hook'` (plan: `idx_command_runs_hook`) | backfill |
| I2 | `SELECT 1 FROM command_imports WHERE source_key = ?1` / `INSERT OR REPLACE INTO command_imports (source_key, imported_at, runs) VALUES (?1, ?2, ?3)` | backfill |
| I3 | `SELECT 1 FROM deleted_commands WHERE text_hash = ?1` | backfill |
| I4 | `SELECT COUNT(*) FROM command_imports` | first enable |
| I5 | `SELECT 1 FROM commands c JOIN command_runs r ON r.command_id = c.id WHERE c.text = ?1 AND r.source = 'hook' LIMIT 1` | backfill |
| T1 | `SELECT COUNT(*) FROM commands` ; `SELECT source, COUNT(*) FROM command_runs GROUP BY source` ; `SELECT COUNT(*) FROM command_events WHERE action = 'pick'` | `--status` |
| T2 | `SELECT id, text, template FROM commands` | training, benchmark |
| T3 | `SELECT id, command_id, started_at, cwd, repo_root, exit_code, shell_session, prev_command_id, source FROM command_runs ORDER BY started_at, id` | training, benchmark |
| T4 | `SELECT id, timestamp, action, command_id, query, rank, session_id, subsession_id, episode_queries, cwd, repo_root, shell_session FROM command_events ORDER BY id` | training |

D1-D5 run in one transaction, in that order (`CmdStore::delete_command(id, text, now)`).
T2-T4 carry no plan requirement: they are full reads, off the interactive path.

### 6.5 `CmdStore` methods and row types

Every statement above is reached through exactly one method. All are
`pub(super)`, take `&self` unless shown, and return `anyhow::Result<..>`.
Row structs are plain `pub(super)` structs in `store.rs` with the listed fields.

| Method | Statements | Returns |
|---|---|---|
| `transaction`, `record_run_in`, `record_run` | S1-S5 | section 6.1 |
| `command_ids()` | R1 ids | `Option<IdRange { first: i64, last: i64 }>` (`None`: no command) |
| `load_commands_in<T>(range: IdRange, build: impl FnMut(CommandRow<&str>) -> T)` | R1 | `Vec<T>`, in id order, `build` called on each `CommandRow<Text = String> { id: i64, text: Text, last_run_at: i64, run_count: u32, success_count: u32, fail_count: u32, pick_count: u32, last_exit_code: Option<i32> }` while its row is current, the text borrowed from SQLite (no `String` per row); `load_commands()` (tests only) is the whole range as `Vec<CommandRow>` |
| `file()`, `open_file(path)`, `map_for_reading()` | - | The database file (`None` in memory); another connection to it, with `Database::new` only (no directory creation or permission change); `PRAGMA mmap_size = 268435456` on this connection. Used by the picker's start-up readers ([02 section 5.2](02-browse.md#52-load-picker-start)) |
| `runs_in_cwd(cwd: &str)` | R2 | `Vec<CwdRunsRow { command_id: i64, runs: u32, last_run_at: i64 }>` |
| `runs_in_repo(repo_root: &str)` | R3 | `Vec<CountRow { command_id: i64, runs: u32 }>` |
| `runs_since(cutoff: i64)` | R4 | `Vec<RecentRunRow { command_id: i64, started_at: i64, exit_code: Option<i32> }>` |
| `session_tail(shell_session: &str)` | R5 | `Vec<String>` (templates, newest first, at most 3) |
| `followers(templates: &[String])` (1 to 3 distinct) | R6 | `Vec<FollowerRow { command_id: i64, template: String, runs: u32 }>` |
| `session_commands(shell_session: &str)` | R7 | `Vec<i64>` |
| `picks_since(cutoff: i64)` | R8 | `Vec<PickRow { command_id: i64, query: String, timestamp: i64, episode_queries: Vec<String> }>` (unparseable JSON -> empty vector) |
| `command_dirs(command_id: i64)` | R9 | `Vec<DirUse { dir: String, runs: u32, last_run_at: i64 }>` |
| `command_predecessors(command_id: i64)` | R10 | `Vec<PrecededBy { template: String, runs: u32 }>` |
| `log_events(now: i64, events: &[NewEvent])` (one transaction) | E1 | `()`; `NewEvent` has one field per E1 parameter except `timestamp`, which the method takes as `now: i64`; `action` is `enum EventAction { Impression, Pick }`, `episode_queries` is `Option<&[String]>` and is written as a JSON array |
| `count_pick(command_id: i64)` | E2 | `()` |
| `delete_command(command_id: i64, text: &str, now: i64)` | D1-D5 | `()` |
| `open_for_recording(data_dir)`, `close_recording(self, limit: u64)` | W1 | `Self`; `LogBound` (section 6). The log's length is one `fs::metadata` of `events.db-wal` (missing = 0) |
| `purge()` | P1 | `PurgeCounts { commands: u64, runs: u64, log_cleared: bool }` (counts taken before deleting; `log_cleared` = the checkpoint's busy column was 0) |
| `first_hook_run()` | I1 | `Option<i64>` |
| `import_done(source_key: &str)`, `mark_import(source_key: &str, now: i64, runs: u64)` | I2 | `bool`, `()` |
| `is_tombstoned(text_hash: i64)` | I3 | `bool` |
| `import_count()` | I4 | `u64` |
| `has_hook_run(text: &str)` | I5 | `bool` |
| `counts()` | T1 | `StoreCounts { commands: u64, hook_runs: u64, import_runs: u64, picks: u64 }` |
| `all_commands()` | T2 | `Vec<CommandRecord { id: i64, text: String, template: String }>` |
| `all_runs()` | T3 | `Vec<RunRecord { id: i64, command_id: i64, started_at: i64, cwd: Option<String>, repo_root: Option<String>, exit_code: Option<i32>, shell_session: String, prev_command_id: Option<i64>, source: Source }>` |
| `all_events()` | T4 | `Vec<EventRecord { id: i64, timestamp: i64, is_pick: bool, command_id: i64, query: String, rank: Option<u32>, session_id: String, subsession_id: u64, episode_queries: Vec<String>, cwd: Option<String>, repo_root: Option<String>, shell_session: Option<String> }>` |

None of these structs derives `Debug` (section 10.6).

## 7. Templating

`src/cmd/template.rs`. `template` = the command text with path-like arguments
replaced by the literal `PATH`, so `cd foo` and `cd bar` share one template.
The upstream tokenizer ([src/import/shell_history.rs:211-289](../../src/import/shell_history.rs))
is **not** reused: it drops quotes and turns any word containing `$`, a glob or
a redirection into `Word(None)`, and a template must keep such words verbatim.

```rust
pub(super) fn raw_words(text: &str) -> Vec<RawWord<'_>>;   // RawWord { text: &str, kind: Word | Operator }
pub(super) fn templatize(text: &str) -> String;
pub(super) fn first_command_word(text: &str) -> Option<&str>;
pub(super) fn word_count(text: &str) -> usize;              // RawWord::Word count; used by 03 feature `argument_count`
```

`raw_words` algorithm (single pass over `char_indices`, slices of the input, no allocation per word):

| Input at word boundary or inside a word | Action |
|---|---|
| space, tab | End the current word |
| newline, `;`, `\|`, `&` outside quotes | End the current word; emit an `Operator` (`\|\|` and `&&` are one operator; a newline is emitted as the operator `;`). Exception: a `&` directly after `<` or `>` (as in `2>&1`) is part of the current word |
| `'` | Consume through the next `'` (or end of text); stays in the current word |
| `"` | Consume through the next unescaped `"` (a `\` skips one char); stays in the current word |
| `\` | Consume the next char; stays in the current word |
| anything else, including `<`, `>`, `$`, `*`, `(`, `)` | Part of the current word (so `$(date)` and `2>&1` are single words) |

`templatize` algorithm:

1. `words = raw_words(text.trim())`.
2. Walk the words keeping `position` (index within the current simple command;
   reset to 0 after each `Operator`) and `command_word` (the first `Word` at or
   after position 0 that does not match `^[A-Za-z_][A-Za-z0-9_]*=` and is not
   `command`, `builtin` or `sudo`).
3. A `Word` after the command word is **path-like** when its text, with one
   pair of enclosing `'` or `"` removed, does not start with `-`, is not a
   redirection word (it matches `^[0-9]*[<>]+&?[0-9-]*$`, e.g. `>`, `2>`, `>>`,
   `2>&1`) and satisfies any of:

   | Rule | Condition |
   |---|---|
   | P1 | `command_word` is in `CMD_PATH_ARG_COMMANDS` |
   | P2 | contains `/` |
   | P3 | starts with `~` |
   | P4 | equals `.` or `..` |
   | P5 | matches `^[A-Za-z0-9_.-]+\.[A-Za-z][A-Za-z0-9]{0,4}$` (a bare file name with an extension that starts with a letter) |

4. Output: words joined by one space; each path-like word becomes `PATH`;
   adjacent `PATH PATH` collapse to one `PATH`; operators are emitted as typed
   (`;` for a newline).

| Command text | Template |
|---|---|
| `cd foo` | `cd PATH` |
| `cd ~/src/psychic` | `cd PATH` |
| `git status` | `git status` |
| `git add src/main.rs Cargo.toml` | `git add PATH` |
| `vim -p a.rs b.rs` | `vim -p PATH` |
| `cargo test --test shell_hooks` | `cargo test --test shell_hooks` |
| `FOO=1 make -C build all` | `FOO=1 make -C build all` |
| `cat foo \| grep "a b" > out.txt` | `cat PATH \| grep "a b" > PATH` |
| `echo 'multi` newline `line'` | `echo 'multi` newline `line'` (one quoted word, kept verbatim) |
| `ls` newline `pwd` | `ls ; pwd` |
| `git push origin v1.2` | `git push origin v1.2` (P5 needs a letter after the dot) |
| `curl https://example.com/x` | `curl PATH` |
| `cat a > b` | `cat PATH > PATH` (the redirection word is kept and separates the two) |
| `make test 2>&1` | `make test 2>&1` |
| `echo $(date) done` | `echo $(date) done` |
| `sudo -u bob ls /x` | `sudo -u bob ls PATH` (the command word is `-u`; `/x` is path-like by P2) |

`first_command_word` returns the `command_word` of the first simple command.
A command is psychic's **own** when `first_command_word(text)` is in
`CMD_OWN_COMMANDS`.

## 8. Git repository root

```rust
// src/cmd/repo.rs
pub(super) fn find_repo_root(cwd: &Path, home: Option<&Path>) -> Option<PathBuf>
```

Return `None` unless `cwd.is_absolute()`. For `dir` in `cwd.ancestors()`, return
`Some(dir.to_path_buf())` at the first `dir.join(".git").exists()` (directory or
file, so worktrees and submodules count). When `cwd` is under `home` (`$HOME`,
compared by path component, so a trailing slash does not matter), the walk
stops before `home` itself: `$HOME/.git` (a dotfiles repository) or a `.git`
above `$HOME` would otherwise make every directory a repository with
`repo_root = $HOME`. A directory strictly below `home` with its own `.git`
still counts, and a `cwd` outside `home` (or `home` = `None`) is walked to `/`.
The comparison is textual, like everything else here (ADR-7): if `$HOME` and
`$PWD` spell the same directory differently (a symlinked home), the walk does
not see the boundary and finds the dotfiles repository again.
Callers pass `$HOME`: `track-command` reads it from the environment, the
importer uses the `home` it was given, the agent-transcript extractor the
`BenchEnv` home. No canonicalisation, no cache
(precedent: `IgnoreChecker::repo_of`, [src/import/mod.rs:338-349](../../src/import/mod.rs),
which stays private). Called once per `track-command`, once per picker start,
once per imported command with a known cwd. Never per keystroke.

## 9. `track-command`

`src/cmd/track.rs`, `pub(super) fn run(data_dir: Option<&Path>, args: &TrackCommandArgs, env: &TrackEnv) -> anyhow::Result<()>`
with `pub(super) struct TrackEnv<'a> { now: i64, ignore: Option<&'a str>, home: Option<&'a Path> }`;
`cmd::run_track_command` fills it with `jiff::Timestamp::now().as_second()`,
`std::env::var("PSYCHIC_HISTORY_IGNORE").ok()` and `std::env::var_os("HOME")`.

| Step | Action | On "skip" |
|---|---|---|
| 1 | `data_dir` is `None` -> `bail!("No data directory: pass --data-dir, or set HOME")` | - |
| 2 | `text = args.command.to_string_lossy()`, then `trim()` | empty -> return `Ok(())` |
| 3 | `text.len() > CMD_MAX_TEXT_BYTES` | return `Ok(())` |
| 4 | `template::first_command_word(text)` in `CMD_OWN_COMMANDS` | return `Ok(())` |
| 5 | `privacy::is_ignored(text, env_ignore)` (section 10.3) | return `Ok(())` |
| 6 | `started_at = args.started_at.unwrap_or(now - args.duration_ms.unwrap_or(0).max(0) / 1000)`; `cwd = Some(args.cwd)` unless empty; `repo_root = cwd.and_then(\|c\| find_repo_root(c, env.home))` on the path as given; then `cwd` and `repo_root` are stored with `to_string_lossy` (a non-UTF-8 byte becomes U+FFFD, as in the text, E-R-06); `template = templatize(text)` | - |
| 7 | `CmdStore::open_for_recording(data_dir)?` (creates the data dir if missing) `.record_run(&NewRun { source: Source::Hook, .. })` (section 6.1). A negative `--duration-ms` is passed as `None` | - |
| 8 | `store.close_recording(CMD_WAL_TRUNCATE_BYTES)?` (section 9.1); `LogBound::Busy` is not an error | - |

It does not check the opt-in marker: being called is the hook's decision
(section 10.1). `main` returns from M1 before its own `create_dir_all`
([src/main.rs:233](../../src/main.rs)), which is why `CmdStore::open` creates the directory. It writes nothing to stdout or stderr on success. Errors
propagate through `main` as `Err`; no error message contains the command text
(section 10.6). `log::` macros reached inside `Database::new` are no-ops because
no logger is installed (section 2, M1).

Budget: B4. Cost model: process start + clap parse, one `Database::new`
(first-open schema work is about 0.7 ms, [src/db.rs:151-153](../../src/db.rs)),
at most 7 `stat` calls for permissions, one `.git` walk, one 5-statement
transaction with `synchronous = NORMAL` under WAL (no fsync per commit), one
`stat` of the log, and no checkpoint on close (section 9.1).

### 9.1 The write-ahead log of `track-command`

By default the last connection to close checkpoints the log into
`events.db` and deletes it. For `track-command` that connection is nearly
always its own, and the checkpoint is about four `fsync`s, ~11 ms on an ext4
NVMe disk: p50 ~16 ms, p95 ~18 ms at the perf DB, against B4's 10 ms.
`open_for_recording` therefore sets `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`, and
the run stays in `events.db-wal`.

That alone lets the log grow without bound (12.9 MB after 310 runs, ~43 KB
per run at the perf DB). Cause: a connection that opens the database while
no other process has it open truncates `-shm` and rebuilds the log index
from the log (`walIndexRecover`), and the rebuilt index marks no frame as
checkpointed (`nBackfill = 0`). SQLite rewinds the log to its start only
when the index says every frame is checkpointed (`walRestartLog`), so across
one-shot processes it never rewinds; auto-checkpoint copies frames but
cannot reset the log either, and every new process re-reads the whole log
(about 0.25 ms per MiB). `SQLITE_FCNTL_PERSIST_WAL` grows the same way.

`close_recording` keeps the bound by hand: when `events.db-wal` is longer
than `CMD_WAL_TRUNCATE_BYTES` it runs W1, which copies the log into the
database and cuts it to zero bytes. W1 runs with `busy_timeout = 0`: if
another connection is writing, checkpointing or inside a read, it returns
busy at once (after a passive checkpoint), the log stays, and a later run
tries again. An idle open connection (a picker between reads) does not
block it. When the picker or file mode is the last connection to close, it
checkpoints and deletes the log as before.

| Property | Guarantee |
|---|---|
| Durability | A crash or power loss can lose the runs still only in the log (not synced under `synchronous = NORMAL`); the log's checksums drop a torn tail. The database is only written by checkpoints, which sync the log before and the database after, so it cannot be corrupted |
| Log size | At most `CMD_WAL_TRUNCATE_BYTES` plus one run's frames (one run past the bound truncates it), unless every run meets a busy log; measured 2,047 KiB max over 5,000 sequential runs and 2.1-2.7 MiB with 8 writers at once |
| Cost to other openers | The first connection to open the database alone re-reads up to 2 MiB of log: +0.4-0.6 ms on the picker's `db_open`, no measurable change in first render (two A/B runs of 30 starts each) |
| Purge | Unchanged: P1 truncates the log, wherever its frames came from (TC-R-27) |
| Rejected: `synchronous = OFF` on this connection | Fastest (p95 6.2 ms), but the close-time checkpoint then writes the database unsynced and deletes the log: a power loss can corrupt the database |

Measured on ext4 NVMe, the perf DB copied per series, 500 runs after 10
warm-up runs, two thirds repeating one of 50 texts and one third new
(`bench/history.py track` repeats 50 texts only): before p50 13.9-16.3 ms,
p95 17.7-18.3 ms, p99 19.0-20.5 ms; after p50 5.3-5.8 ms, p95 7.2-8.8 ms,
p99 13.7-17.3 ms (the p99 is the runs that truncate, about 1 in 48). With 8
writers at once (2,000 runs) p50 is ~3.1 ms either way, but p95 rises from
12.4-13.7 ms to ~21.5 ms: a run that truncates holds the write lock
through its `fsync`s while the other writers wait on `busy_timeout`.

## 10. Privacy

### 10.1 Opt-in

| State | Decided by |
|---|---|
| Recording is ON for a prompt | `PSYCHIC_HISTORY` is `1`, or (`PSYCHIC_HISTORY` is not `0` and the marker file `<data dir>/history-enabled` exists) |
| Data dir the hook looks in | `$PSYCHIC_DATA_DIR` if set, else `$HOME/.local/share/psychic` (same default as [src/cli.rs:253-259](../../src/cli.rs)) |
| Cost when OFF | One `[[ -e ... ]]` / `test -e` builtin per prompt; no process, no fork |
| Who decides whether a command is recorded | The shell hook only (section 11). `track-command` never re-checks |
| The same rule in Rust, for display only (`--status`, the picker's hint) | `privacy::recording_enabled(data_dir: &Path, env: Option<&str>) -> bool`, with `env` = the value of `PSYCHIC_HISTORY` |

`PSYCHIC_BIN` (default `psychic`) and `PSYCHIC_DATA_DIR` are read **only** by the
blocks added in section 11 and in [02 section 11](02-browse.md#11-shell-widgets).
They are how tests and benchmarks point the hooks at a build and a scratch data
dir. The existing `p`/`pd`/`pc`/Ctrl-T functions are not changed to read them.

### 10.2 What `psychic init` prints

In `add` ([src/shell_init.rs:328-331](../../src/shell_init.rs)), immediately
after the existing `println!("Done. Open a new shell, ...")`, add:

```rust
println!("Shell history search (Ctrl-R) is off. To turn it on: psychic history --enable");
```

Nothing else in `shell_init.rs` changes; `HINT` ([src/shell_init.rs:96](../../src/shell_init.rs)) is untouched.

### 10.3 What is never recorded

| Rule | Where enforced | Mechanism |
|---|---|---|
| Leading-space commands, zsh | hook | `[[ -o histignorespace && "$1" == ' '* ]]` |
| `HISTORY_IGNORE`, zsh | hook | `[[ -n "${HISTORY_IGNORE-}" && "$1" == ${~HISTORY_IGNORE} ]]` |
| `HIST_NO_STORE`, zsh | hook | `__psychic_history_no_store`, zsh's own rule repeated (section 11.2) |
| `zshaddhistory` hooks, zsh | **not honoured** | Calling the user's hooks a second time is not safe (section 11.2); `--enable` says so |
| `HISTCONTROL` (`ignorespace`, `ignoreboth`, `ignoredups`, `erasedups`) and `HISTIGNORE`, bash | by construction | The hook records only what bash itself added to its history list (section 11.3) |
| Leading-space commands and private mode, fish | hook | `string match -q -- ' *'` (only when `fish_should_add_to_history` is not defined), `set -q fish_private_mode` |
| `fish_should_add_to_history`, fish | hook | Called again with the command line; recorded only on status 0 (section 11.4) |
| psychic's own commands | `track.rs` step 4, `backfill.rs` | `CMD_OWN_COMMANDS` |
| `PSYCHIC_HISTORY_IGNORE` | `track.rs` step 5, `backfill.rs` | `privacy::is_ignored` |
| Commands over `CMD_MAX_TEXT_BYTES` | `track.rs` step 3, `backfill.rs` | length check |

`PSYCHIC_HISTORY_IGNORE` is a colon-separated list of glob patterns (the syntax
of bash `HISTIGNORE` without `&`). It must be **exported**, because the binary
reads it. A command is ignored when any pattern matches the **whole** trimmed
command text. Implemented without a dependency:

```rust
// src/cmd/privacy.rs
pub(super) fn is_ignored(text: &str, patterns: Option<&str>) -> bool;  // split on ':', skip empty patterns
fn glob_match(pattern: &str, text: &str) -> bool;                      // over chars, iterative with one backtrack point
```

| Pattern element | Matches |
|---|---|
| `*` | any run of characters, including none, including `/` and newlines |
| `?` | exactly one character |
| `[abc]`, `[a-z]`, `[!a-z]` | one character in (not in) the set; an unclosed `[` is a literal `[` |
| `\x` | the character `x` literally |
| anything else | itself, case-sensitive |

There is no way to write a literal `:` in a pattern; `?` matches it.
Example: `export PSYCHIC_HISTORY_IGNORE='*password*:export *TOKEN*:mysql *-p*'`.

### 10.4 File permissions

```rust
pub(super) fn tighten_permissions(data_dir: &Path) -> std::io::Result<()>
```

| Path | Required mode |
|---|---|
| `data_dir` | `0o700` |
| `events.db`, `events.db-wal`, `events.db-shm`, `history-enabled`, `cmd_model.txt`, `cmd_model_stats.json` (each only if it exists) | `0o600` |

For each path: `fs::metadata`, compare `mode() & 0o777`, call
`fs::set_permissions` only when it differs. Called from `CmdStore::open` (so
from `track-command`, every `psychic history` action, the picker, training and
backfill) and from `--enable`. This is where existing databases are tightened:
the first `--enable` or the first recorded command. SQLite creates `-wal` and
`-shm` with the mode of the main database file, so they stay `0600` once
`events.db` is; after a `track-command` they stay on disk between runs
(section 9.1), and truncating keeps the file and its mode. Upstream sets no mode anywhere (`Database::new`,
[src/db.rs:162-200](../../src/db.rs); `create_dir_all`, [src/main.rs:233](../../src/main.rs));
those call sites are not changed, so a user who never enables history keeps
today's modes.

### 10.5 Delete and purge

| Operation | Trigger | Effect |
|---|---|---|
| Delete one command | Picker key ([02 section 9](02-browse.md#9-keys)) | `CmdStore::delete_command`: D1-D5 in one transaction. The tombstone `fnv1a64(text)` stops a later import restoring it. Running the command again records it again |
| Purge | `psychic history --purge` | Without `--yes`: print `Delete all recorded commands (N commands, M runs)? [y/N] ` and read one line from stdin; anything but `y`/`Y` prints `Nothing deleted.` and returns. Then P1, remove `cmd_model.txt` and `cmd_model_stats.json` if present, print `Deleted N commands and M runs. Recording is still on|off.` If P1's checkpoint reported busy, then fail with E-R-23. The marker file is not touched. The tombstones in `deleted_commands` are kept; `command_imports` is emptied, so the next import (`--import`, or the first `--enable` after it) reads every source again, minus the tombstoned texts |

```rust
pub(super) fn fnv1a64(text: &str) -> i64  // offset 0xcbf29ce484222325, prime 0x100000001b3, over UTF-8 bytes, `as i64`
```

A tombstone is that hash and the second of the delete; it holds no text. It
outlives a purge: purge empties everything else, and a tombstone is the only
thing that stops the shell's own history file (which psychic does not edit)
from bringing a deleted command back on the next import, including the one a
`--disable`, `--purge`, `--enable` sequence runs. `command_imports` is emptied
by purge, because a source marked as imported would skip its untimestamped
entries on the next import (section 12.3 step 4). The hash is unsalted FNV-1a
of the raw text and not a cryptographic hash: someone holding `events.db` can
confirm a guess (hash a candidate command and look it up), so a short or
guessable secret is not hidden from a dictionary or brute-force search; a
long random one is. What a tombstone does not reveal is any text nobody guessed.

Purge leaves no command text in the database's files. `DELETE` alone leaves the freed pages'
bytes in `events.db`, and in WAL mode the old pages also sit in
`events.db-wal` until it is overwritten; when another connection is open (a
running picker, a `track-command` mid-flight) SQLite does not delete the log
when purge's connection closes, and the log `track-command` leaves behind
(section 9.1) holds the runs of up to the last 2 MiB of recording. So P1 turns on `secure_delete` (freed pages
are zeroed), `VACUUM`s (the file is rewritten), and runs
`wal_checkpoint(TRUNCATE)`, which copies the log into the database and cuts it
to zero bytes. It waits up to `busy_timeout` (5 s) for readers; an idle open
connection does not stop it, one in the middle of a read does, and then the
checkpoint reports busy and the text may still be in `-wal` (E-R-23: the
rows are deleted; running `--purge` again once the other process is done
clears it). A `VACUUM` that finds the database busy is reported the same way,
after the rows are gone. The `-shm` file holds only the log's index, never
page contents. Blocks the file system frees are not overwritten; that is
below SQLite.

Neither operation edits the shell's own history file (non-goal,
[00 section 7](00-blueprint.md#7-non-goals-not-built-do-not-add-hooks-flags-or-schema-for-them)).

### 10.6 No command text outside `events.db`

| Sink | Rule |
|---|---|
| `app.log` (`log::` macros) | No call in `src/cmd/` formats a command text, a template, a query, or an error value that embeds one. Log counts, ids and durations only |
| `TIMING` lines | Numbers only ([02 section 6.5](02-browse.md#65-timing-line)) |
| `training.log`, `cmd_model_stats.json` | Feature names and numbers only |
| Error messages returned from `main` | Built with `anyhow!`/`context` strings that name the operation and the path, never the text. SQL errors are wrapped: `.map_err(\|e\| anyhow!("recording a command failed: {}", e.sqlite_error_code_name()))` style, never `{e}` with bound values |
| Benchmark output | Aggregates only ([04 section 5.6](04-bench.md#56-output)) |
| Network | None anywhere in `src/cmd/` |

## 11. Shell hooks

Protocol: the pre-exec side only stashes shell variables; the pre-prompt side
makes **one** background `track-command` call. Conventions copied from the
existing scripts: `\command`/`\builtin` prefixes and `&!` in zsh
([shell/psychic.zsh:14-16](../../shell/psychic.zsh)), subshell-background in
bash ([shell/psychic.bash:21](../../shell/psychic.bash)), `disown` in fish
([shell/psychic.fish:14-17](../../shell/psychic.fish)), `__psychic_` prefix.

### 11.1 Session id

One per shell process, set when the script is sourced, not exported:
zsh/bash `"$$-${RANDOM}${RANDOM}"`, fish `"$fish_pid-"(random)`.

### 11.2 zsh (append to `shell/psychic.zsh`)

```zsh
# =============================================================================
# Command history: record each command for `psychic history` (Ctrl-R)
# =============================================================================

# Off until `psychic history --enable`. PSYCHIC_HISTORY=0 or 1 overrides.
\builtin zmodload zsh/datetime 2>/dev/null
\builtin typeset -g __psychic_session="$$-${RANDOM}${RANDOM}"
\builtin typeset -g __psychic_cmd='' __psychic_cmd_pwd='' __psychic_cmd_start=''

function __psychic_history_enabled() {
    case "${PSYCHIC_HISTORY-}" in
        0) \builtin return 1 ;;
        1) \builtin return 0 ;;
    esac
    [[ -e "${PSYCHIC_DATA_DIR:-${HOME}/.local/share/psychic}/history-enabled" ]]
}

# HIST_NO_STORE: whether zsh keeps the line `$1` out of its history, as
# should_ignore_line in zsh's hist.c decides it on the parsed line (aliases
# expanded, words joined by single spaces): `history`, `r` or `fc -l` (any
# option letters before the first non-letter include an `l`), after an
# optional `builtin`, and followed by a space or nothing (so `history;x` is
# kept, `history|x` is not); a function of that name runs instead unless
# `builtin` was given, and then the line is kept.
function __psychic_history_no_store() {
    \builtin emulate -L zsh
    \builtin local -a words
    words=(${(z)1})
    \builtin local forced='' depth=0
    while (( depth++ < 10 )); do
        if [[ "${words[1]-}" == builtin ]]; then
            forced=1
            \builtin shift words
        elif [[ -z "${forced}" && -n "${words[1]-}" && -n "${aliases[${words[1]}]-}" ]]; then
            words=(${(z)aliases[${words[1]}]} "${(@)words[2,-1]}")
        else
            break
        fi
    done
    [[ "${words[2]-}" == ';' ]] && \builtin return 1
    case "${words[1]-}" in
        history|r) ;;
        fc) [[ "${words[2]-}" == -* && "${${words[2]#-}%%[^[:alpha:]]*}" == *l* ]] || \builtin return 1 ;;
        *) \builtin return 1 ;;
    esac
    [[ -n "${forced}" ]] || ! \builtin functions "${words[1]}" >/dev/null 2>&1
}

# Before a command runs: remember it, where it was typed and when.
function __psychic_preexec() {
    __psychic_cmd=''
    __psychic_history_enabled || \builtin return 0
    [[ -n "$1" ]] || \builtin return 0
    [[ -o histignorespace && "$1" == ' '* ]] && \builtin return 0
    [[ -n "${HISTORY_IGNORE-}" && "$1" == ${~HISTORY_IGNORE} ]] && \builtin return 0
    [[ -o histnostore ]] && __psychic_history_no_store "$1" && \builtin return 0
    __psychic_cmd="$1"
    __psychic_cmd_pwd="${PWD}"
    __psychic_cmd_start="${EPOCHREALTIME-}"
}

# Before the next prompt: one background call with how it went.
function __psychic_precmd() {
    \builtin local __psychic_status=$?
    [[ -n "${__psychic_cmd}" ]] || \builtin return 0
    \builtin local __psychic_ms=''
    [[ -n "${__psychic_cmd_start}" ]] && (( __psychic_ms = (EPOCHREALTIME - __psychic_cmd_start) * 1000 ))
    \command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        track-command --cwd "${__psychic_cmd_pwd}" --exit-code "${__psychic_status}" \
        ${__psychic_cmd_start:+--started-at} ${__psychic_cmd_start:+"${__psychic_cmd_start%.*}"} \
        ${__psychic_ms:+--duration-ms} ${__psychic_ms:+"${__psychic_ms%.*}"} \
        --session "${__psychic_session}" -- "${__psychic_cmd}" >/dev/null 2>&1 &!
    __psychic_cmd=''
}

\builtin autoload -Uz add-zsh-hook
add-zsh-hook preexec __psychic_preexec
add-zsh-hook precmd __psychic_precmd
```

Facts this relies on: `preexec` receives the line as typed in `$1`, including a
leading space, and an empty string when history is off; `precmd` sees the last
command's `$?`; `add-zsh-hook` does not add a function twice, so re-sourcing is
idempotent. The functions do not call `emulate -L zsh`, because that would
reset the `histignorespace` option being tested (`__psychic_history_no_store`,
called only after `histnostore` is tested, does).

`HIST_NO_STORE`: `__psychic_history_no_store` repeats the test of
`should_ignore_line` in zsh's `hist.c` on the words `${(z)1}`, with aliases
in command position expanded (up to 10 levels; zsh judges the parsed line):
the line is dropped when its first word (after an optional `builtin`) is
`history` or `r`, or is `fc` followed by a word of `-` and letters among which
an `l` comes before the first non-letter, unless the next word is `;` (zsh's
parsed text then has no space after the name), and unless a function of that
name exists and `builtin` was not given. Checked against zsh 5.9 on a pty, by
comparing with what zsh kept in `fc -l`: `history`, `history|cat`,
`history&&echo y`, `history >/dev/null`, `  history 1`, `fc -l`, `fc -rl`,
`builtin history`, `history -n`, `h` with `alias h=history`, `hh` with
`alias hh='h -n'` are dropped by zsh and not recorded; `history;echo y`,
`fc -W f`, `echo history` and `r` with a function `r` are kept by zsh and
recorded.

`zshaddhistory` (the function and `zshaddhistory_functions`) is **not
honoured**, by decision: zsh calls every hook and keeps the first non-zero
status (1: not saved; 2: saved in the session only, not to `HISTFILE`; psychic
would treat both as "do not record", since it persists everything), but
neither outcome is observable afterwards: the rejected line stays in `$history`
and `fc -l` until the next line is read. The only way to learn the decision is
to call the user's hooks a second time, which is not safe: common
implementations have side effects (zsh-histdb inserts the line into its
database, per-directory-history appends it to a file). `--enable` says so, and
points to `PSYCHIC_HISTORY_IGNORE` instead (section 13.1). Likewise
`HIST_NO_FUNCTIONS` (function definitions) is not mirrored.

### 11.3 bash (append to `shell/psychic.bash`)

One edit outside the appended block: the upstream `__psychic_hook` (directory
tracking, which runs after the history hook) saves `$?` on entry and returns
it, so the entries after it see the user's command's status.

Decision (ADR-5): no DEBUG trap and no `PS0`. Two functions run from
`PROMPT_COMMAND`: `__psychic_history_hook` first (before anything can change
`$?` or load other shells' lines) and `__psychic_history_sync` last (after
every other entry has changed the history list, as `history -a; history -n`
does). The hook reads the newest entry of bash's own history list, and counts
it as the user's command only if bash added it after the previous prompt's
sync. Checked on a pty with bash 5.3 and, in the official `bash:X.Y` images,
3.2, 4.3, 4.4, 5.0 and 5.1.

```bash
# =============================================================================
# Command history: record each command for `psychic history` (Ctrl-R)
# =============================================================================

# Off until `psychic history --enable`. PSYCHIC_HISTORY=0 or 1 overrides.
#
# bash has no preexec hook (and a DEBUG trap or PS0 would collide with other
# tools), so this works from PROMPT_COMMAND alone. Two functions share the
# work: __psychic_history_hook runs first, before anything can change `$?` or
# load other shells' lines into the history list, and __psychic_history_sync
# runs last, after every other entry (Starship, direnv, `history -a;
# history -n`) has had its turn with the list. At the next prompt, the newest
# entry is the user's command only if the shell ran one and bash added it
# after the sync: one entry more than the sync counted, or (erasedups replaced
# an older copy) as many, but stamped after the sync. Lines other shells wrote
# to HISTFILE are loaded after the hook and before the sync, so they never
# count. docs/ctrl-r/01-record.md section 11.3.
#
# Kept when the script is sourced again (`source ~/.bashrc`), so the session
# and what the sync saw carry on.
: "${__psychic_session:=$$-${RANDOM}${RANDOM}}"
: "${__psychic_cmd_pwd:=${PWD}}"
: "${__psychic_hist_on=}"      # recording was on at this prompt
: "${__psychic_hist_cmdno=}"   # \# at the last prompt: how many commands the shell had run
: "${__psychic_hist_count=}"   # the history count when the sync ran; empty = nothing to compare with
: "${__psychic_hist_time=}"    # the time the sync ran, Unix seconds
: "${__psychic_hist_trusted=}" # the sync ran last in PROMPT_COMMAND
__psychic_hist_re='^[[:space:]]*([0-9]+)[*]?[[:space:]]+([0-9]+)[[:space:]](.*)$'
# bash 5.1 runs each element of a PROMPT_COMMAND array in turn, so the sync
# gets an element of its own: a `PROMPT_COMMAND="$PROMPT_COMMAND; ..."` later
# in ~/.bashrc then still runs before it. Older bash runs a string only. Also
# from 5.1, $HISTCMD inside PROMPT_COMMAND is the history count (before, it
# is always 1, and the count is the newest entry's number, at one fork).
__psychic_hist_new=''
((BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1))) && __psychic_hist_new=1
# \# in a prompt string counts the commands the shell has run; ${x@P} (bash
# 4.4) expands it without a fork. An empty line, Ctrl-C at the prompt and a
# syntax error leave it alone.
__psychic_hist_cmdno_format=''
((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4))) && __psychic_hist_cmdno_format='\#'

__psychic_history_enabled() {
    case "${PSYCHIC_HISTORY-}" in
        0) \builtin return 1 ;;
        1) \builtin return 0 ;;
    esac
    [[ -e "${PSYCHIC_DATA_DIR:-${HOME}/.local/share/psychic}/history-enabled" ]]
}

# The newest history entry, into __psychic_hist_{num,start,text}; fails when
# the list is empty.
__psychic_history_newest() {
    \builtin local __psychic_entry
    __psychic_entry="$(HISTTIMEFORMAT='%s ' \builtin history 1)"
    [[ "${__psychic_entry}" =~ ${__psychic_hist_re} ]] || \builtin return 1
    __psychic_hist_num="${BASH_REMATCH[1]}"
    __psychic_hist_start="${BASH_REMATCH[2]}"
    __psychic_hist_text="${BASH_REMATCH[3]}"
}

# PROMPT_COMMAND's first command, into __psychic_hist_word, and its last.
# bash-preexec keeps `__bp_precmd_invoke_cmd` first and `__bp_interactive_mode`
# last, putting them back at every prompt; neither touches the history list,
# so they are looked past, rather than fought over.
__psychic_history_ends() {
    \builtin local __psychic_all='' __psychic_i
    for __psychic_i in "${!PROMPT_COMMAND[@]}"; do
        # Only element 0 runs before bash 5.1.
        [[ -n "${__psychic_hist_new}" || "${__psychic_i}" == 0 ]] || continue
        __psychic_all+="${PROMPT_COMMAND[__psychic_i]}"$'\n'
    done
    # Commands are separated by `;` or a newline; strip blanks and separators.
    __psychic_all="${__psychic_all#"${__psychic_all%%[![:space:];]*}"}"
    __psychic_all="${__psychic_all%"${__psychic_all##*[![:space:];]}"}"
    __psychic_hist_first="${__psychic_all%%[[:space:];]*}"
    if [[ "${__psychic_hist_first}" == __bp_precmd_invoke_cmd ]]; then
        __psychic_hist_first="${__psychic_all#__bp_precmd_invoke_cmd}"
        __psychic_hist_first="${__psychic_hist_first#"${__psychic_hist_first%%[![:space:];]*}"}"
        __psychic_hist_first="${__psychic_hist_first%%[[:space:];]*}"
    fi
    __psychic_hist_last="${__psychic_all##*[[:space:];]}"
    if [[ "${__psychic_hist_last}" == __bp_interactive_mode ]]; then
        __psychic_hist_last="${__psychic_all%__bp_interactive_mode}"
        __psychic_hist_last="${__psychic_hist_last%"${__psychic_hist_last##*[![:space:];]}"}"
        __psychic_hist_last="${__psychic_hist_last##*[[:space:];]}"
    fi
}

# Move the hook to the front of PROMPT_COMMAND, from the next prompt on.
__psychic_history_place_hook() {
    \builtin local __psychic_i __psychic_first=''
    \builtin local __psychic_value
    for __psychic_i in "${!PROMPT_COMMAND[@]}"; do
        __psychic_value="${PROMPT_COMMAND[__psychic_i]//__psychic_history_hook;/}"
        __psychic_value="${__psychic_value//__psychic_history_hook/}"
        [[ "${__psychic_value}" == "${PROMPT_COMMAND[__psychic_i]}" ]] ||
            PROMPT_COMMAND[__psychic_i]="${__psychic_value}"
        [[ -n "${__psychic_first}" ]] || __psychic_first="${__psychic_i}"
    done
    [[ -n "${__psychic_hist_new}" ]] || __psychic_first=0
    PROMPT_COMMAND[${__psychic_first:-0}]="__psychic_history_hook;${PROMPT_COMMAND[${__psychic_first:-0}]-}"
}

# Move the sync to the end of PROMPT_COMMAND, from the next prompt on.
__psychic_history_place_sync() {
    if [[ -n "${__psychic_hist_new}" ]]; then
        \builtin local __psychic_i
        for __psychic_i in "${!PROMPT_COMMAND[@]}"; do
            [[ "${PROMPT_COMMAND[__psychic_i]}" == __psychic_history_sync ]] &&
                \builtin unset 'PROMPT_COMMAND[__psychic_i]'
        done
        PROMPT_COMMAND+=(__psychic_history_sync)
    else
        # A newline, not `;`: it ends any command, even one with a trailing
        # `&` or comment.
        PROMPT_COMMAND="${PROMPT_COMMAND//$'\n'__psychic_history_sync/}"$'\n'__psychic_history_sync
    fi
}

# The history count: $HISTCMD from bash 5.1, else the newest entry's number
# (one fork). Into __psychic_hist_now.
__psychic_history_count() {
    if [[ -n "${__psychic_hist_new}" ]]; then
        __psychic_hist_now="${HISTCMD-}"
    elif __psychic_history_newest; then
        __psychic_hist_now="${__psychic_hist_num}"
    else
        __psychic_hist_now=0
    fi
}

# First in PROMPT_COMMAND: if the user ran a command bash added to its
# history, record it.
__psychic_history_hook() {
    \builtin local __psychic_status=$?
    \builtin local __psychic_pwd="${__psychic_cmd_pwd}"
    __psychic_cmd_pwd="${PWD}"
    if ! __psychic_history_enabled; then
        __psychic_hist_on='' __psychic_hist_count=''
        \builtin return "${__psychic_status}"
    fi
    \builtin local __psychic_cmdno=''
    [[ -n "${__psychic_hist_cmdno_format}" ]] && __psychic_cmdno="${__psychic_hist_cmdno_format@P}"
    \builtin local __psychic_count="${__psychic_hist_count}" __psychic_since="${__psychic_hist_time}"
    \builtin local __psychic_trusted="${__psychic_hist_trusted}" __psychic_ran=1 __psychic_first=1
    [[ -n "${__psychic_cmdno}" && "${__psychic_cmdno}" == "${__psychic_hist_cmdno}" ]] && __psychic_ran=''
    __psychic_hist_on=1 __psychic_hist_count='' __psychic_hist_time='' __psychic_hist_cmdno="${__psychic_cmdno}"
    # Where the two functions stand, judged now: for this prompt (the hook)
    # and for the sync of this prompt. Something run before the hook may
    # have loaded other shells' lines since the last sync: nothing is
    # recorded at this prompt, and the hook goes back to the front.
    \builtin local __psychic_hist_first __psychic_hist_last
    __psychic_history_ends
    if [[ "${__psychic_hist_first}" != __psychic_history_hook ]]; then
        __psychic_first=''
        __psychic_history_place_hook
    fi
    if [[ "${__psychic_hist_last}" == __psychic_history_sync ]]; then
        __psychic_hist_trusted=1
    else
        __psychic_hist_trusted=''
        __psychic_history_place_sync
    fi

    # Nothing to compare with (first prompt, recording just turned on), the
    # shell ran nothing (empty line, Ctrl-C, syntax error), or the hook was
    # not first.
    [[ -n "${__psychic_count}" && -n "${__psychic_ran}" && -n "${__psychic_first}" ]] ||
        \builtin return "${__psychic_status}"

    # Did bash add the command to its history? With a trusted count: one more
    # entry is a yes; as many is a no, or erasedups dropped an older copy, and
    # only the entry's time tells; anything else (the command itself ran
    # `history -c`, say) is a no. Without one, only the time tells.
    \builtin local __psychic_appended='' __psychic_hist_num='' __psychic_hist_start='' __psychic_hist_text='' __psychic_hist_now=''
    if [[ -n "${__psychic_trusted}" ]]; then
        __psychic_history_count
        if [[ "${__psychic_hist_now}" == "$((__psychic_count + 1))" ]]; then
            __psychic_appended=1
        elif [[ "${__psychic_hist_now}" != "${__psychic_count}" ]]; then
            \builtin return "${__psychic_status}"
        fi
    fi
    [[ -n "${__psychic_appended}" || -n "${__psychic_since}" ]] || \builtin return "${__psychic_status}"
    [[ -n "${__psychic_hist_num}" ]] || __psychic_history_newest || \builtin return "${__psychic_status}"
    [[ -n "${__psychic_appended}" ]] || ((__psychic_hist_start > __psychic_since)) || \builtin return "${__psychic_status}"

    \builtin local __psychic_start="${__psychic_hist_start}" __psychic_now=''
    \builtin printf -v __psychic_now '%(%s)T' -1 2>/dev/null || __psychic_now=''
    # In a subshell, so the shell prints no job number or "Done" line.
    (\command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        track-command --cwd "${__psychic_pwd}" --exit-code "${__psychic_status}" \
        --started-at "${__psychic_start}" \
        ${__psychic_now:+--duration-ms} ${__psychic_now:+"$(((__psychic_now - __psychic_start) * 1000))"} \
        --session "${__psychic_session}" -- "${__psychic_hist_text}" >/dev/null 2>&1 &)
    \builtin return "${__psychic_status}"
}

# Last in PROMPT_COMMAND: note where the history list stands once every other
# entry is done with it.
__psychic_history_sync() {
    \builtin local __psychic_status=$?
    if [[ -n "${__psychic_hist_on}" ]]; then
        \builtin local __psychic_hist_now='' __psychic_hist_num='' __psychic_hist_start='' __psychic_hist_text=''
        __psychic_history_count
        __psychic_hist_count="${__psychic_hist_now}"
        \builtin printf -v __psychic_hist_time '%(%s)T' -1 2>/dev/null || __psychic_hist_time=''
    fi
    \builtin return "${__psychic_status}"
}

if [[ ";${PROMPT_COMMAND:-};" != *";__psychic_history_hook;"* ]]; then
    PROMPT_COMMAND="__psychic_history_hook${PROMPT_COMMAND:+;${PROMPT_COMMAND}}"
fi
__psychic_history_ends
[[ "${__psychic_hist_last}" == __psychic_history_sync ]] || __psychic_history_place_sync
\builtin unset __psychic_hist_first __psychic_hist_last
```

| Property | How it is obtained |
|---|---|
| Command text | Group 3 of `history 1`; multi-line entries arrive whole (POSIX `.` matches a newline) |
| Start time | Group 2: the history entry's own timestamp, forced to epoch seconds by a one-command `HISTTIMEFORMAT='%s '` |
| "This shell ran a command" | `\#` (the prompt escape: commands the shell has executed), expanded with no fork by `${var@P}` (bash >= 4.4), moved since the last prompt. An empty line, Ctrl-C at the prompt and a syntax error leave it alone, so they record nothing. On bash < 4.4 this check is skipped and the next rows decide alone |
| The history count | bash >= 5.1: `$HISTCMD` (the number the next entry gets). Before 5.1, `$HISTCMD` is always 1 inside `PROMPT_COMMAND`, so the count is the newest entry's number from `history 1` (one `$(...)` fork in the sync and one in the hook per prompt) |
| "bash added it to its history" | The count against the one the sync took after the previous prompt's `PROMPT_COMMAND`: one more is a yes; the same is a yes only if the newest entry is stamped after the sync (under `erasedups` the new entry replaced an older copy, so the count did not move); anything else is a no. Entries another shell wrote to `HISTFILE` are loaded by `history -n`/`history -r` after the hook and before the sync, so they are never taken for this shell's |
| Hook is first, sync is last | Checked by the hook at every prompt on the first and last command word of `PROMPT_COMMAND` (all elements joined, from bash 5.1; element 0 before), looking past bash-preexec's `__bp_precmd_invoke_cmd` (first) and `__bp_interactive_mode` (last), which bash-preexec puts back at every prompt and which do not touch the history list. bash >= 5.1: the sync is its own array element, so `PROMPT_COMMAND="$PROMPT_COMMAND; ..."` (which edits element 0) still runs before it; older bash: the last line of the string, after a newline (which ends any command, even one ending in `&` or a comment). If the hook is not first, something before it may have loaded other shells' lines since the sync: nothing is recorded at that prompt, and the hook is moved back to the front of the first element. If the sync is not last, it is moved back to the end, and the next prompt judges by time only (the newest entry must be stamped after the sync) |
| First prompt | Only primes: there is no count to compare with until a sync has run. So is the first prompt after recording is turned on |
| Sourced again | The session id and the hook state are set only when unset (`: "${var:=...}"`), so `source ~/.bashrc` keeps the session and the next command is recorded |
| cwd the command was typed in | `$PWD` saved at the end of the previous invocation |
| Exit status | `$?` on entry to the function |
| Duration | Whole seconds x 1000 (bash >= 4.2 for `printf '%(%s)T'`; omitted on 3.2-4.1, where the sync's time is unknown too, so only an appended entry counts) |
| `HISTCONTROL`, `HISTIGNORE`, `set +o history` | A command bash did not add to its history leaves the count where the sync found it and the newest entry stamped before the sync, so it is not recorded. No pattern is re-implemented |
| Coexistence | The hook is prepended to `PROMPT_COMMAND` (string, or element 0 of the array form), the sync appended; both, and the upstream `__psychic_hook` (which runs after the hook), return the status they received, so later entries (Starship, direnv, the user's own) see the user's command's `$?`. No trap and no `PS0` are touched |
| Cost when enabled | bash >= 5.1: builtins only when nothing was added to the history (empty line, ignored command), one `$(...)` fork of the shell (no exec) plus the background binary per recorded command. Older bash: one fork in the sync at every prompt, and one in the hook when a command ran |

Known limitations, stated in `docs/how-it-works.md`:

- With `ignoredups` or `ignoreboth` an immediate repeat of the identical
  command is not added to bash's history and therefore not recorded (decided:
  the shell's own decision is the definition of the opt-out).
- Under `erasedups`, a command that replaces an older copy of itself is
  recorded only if bash stamps it in a later second than the sync of the
  prompt it was typed at; one typed and run within the same second is lost.
- When the hook finds itself not first (something was prepended to
  `PROMPT_COMMAND`, as `PROMPT_COMMAND="history -n; $PROMPT_COMMAND"` does),
  the command run just before that prompt is not recorded. Usually that is the
  line that did the prepending, or the first prompt of the shell, which records
  nothing anyway.
- When the sync is not last (something appended behind it: a new array
  element on any version, or a string append on bash < 5.1), the next command
  is judged by time only and is lost if run within the second its prompt
  appeared; a `history -n` that loaded another shell's line in the very second
  after the sync could then be taken for the user's command.
- bash-preexec's two entries are trusted not to touch the history list; a
  `precmd_functions` entry (run inside `__bp_precmd_invoke_cmd`, before the
  hook) that runs `history -n` is not detected.
- A command that itself rewrites the history list (`history -c`, `history -r`,
  `history -n`, `history -s`, `history -d`) is recorded only when the count
  ends up exactly one past the sync's, and then the newest entry is what is
  recorded. With `shopt -u cmdhist`, a multi-line command adds one entry per
  line, so it is not recorded.
- An entry another shell wrote with a timestamp in the future (clock skew
  between machines sharing `HISTFILE`) and loaded when this shell ran an
  ignored command can be taken for the user's.
- `unset HISTCMD` (bash >= 5.1) stops recording, silently.
- Re-sourcing the script on bash < 5.1 prepends `__psychic_hook` a second time
  (its registration looks for `;__psychic_hook;`, and the sync's newline
  follows it); the second call is a no-op.
- If another tool is prepended to `PROMPT_COMMAND` after psychic and does not
  preserve `$?`, the recorded exit code is that tool's; loading psychic last
  in `~/.bashrc` avoids it.

### 11.4 fish (append to `shell/psychic.fish`)

```fish
# =============================================================================
# Command history: record each command for `psychic history` (Ctrl-R)
# =============================================================================

# Off until `psychic history --enable`. PSYCHIC_HISTORY=0 or 1 overrides.
set -g __psychic_session "$fish_pid-"(random)
set -g __psychic_cmd_pwd $PWD

function __psychic_history_enabled
    switch "$PSYCHIC_HISTORY"
        case 0
            return 1
        case 1
            return 0
    end
    set -l dir $HOME/.local/share/psychic
    set -q PSYCHIC_DATA_DIR; and set dir $PSYCHIC_DATA_DIR
    test -e $dir/history-enabled
end

# Before a command runs: remember where it was typed.
function __psychic_preexec --on-event fish_preexec
    set -g __psychic_cmd_pwd $PWD
end

# After it: one background call with how it went. Disowned, so fish prints no
# "Job has ended" line.
function __psychic_postexec --on-event fish_postexec
    set -l __psychic_status $status
    __psychic_history_enabled; or return
    set -q fish_private_mode; and return
    test -n "$argv[1]"; or return
    # fish keeps a command out of its history when it starts with a space,
    # or, if the user defined fish_should_add_to_history, when that says so
    # (it then decides alone, leading space included).
    if functions -q fish_should_add_to_history
        fish_should_add_to_history "$argv[1]" >/dev/null 2>&1; or return
    else
        string match -q -- ' *' "$argv[1]"; and return
    end
    set -l __psychic_bin psychic
    set -q PSYCHIC_BIN; and set __psychic_bin $PSYCHIC_BIN
    # Not installed (or not on PATH): fish would print "command not found",
    # and the command line with it, at every prompt.
    command -q $__psychic_bin; or return
    set -l __psychic_dir
    set -q PSYCHIC_DATA_DIR; and set __psychic_dir --data-dir $PSYCHIC_DATA_DIR
    command $__psychic_bin $__psychic_dir track-command --cwd "$__psychic_cmd_pwd" \
        --exit-code $__psychic_status --duration-ms "$CMD_DURATION" \
        --session "$__psychic_session" -- "$argv[1]" >/dev/null 2>&1 &
    disown 2>/dev/null
end
```

fish >= 4 also sets `fish_private_mode` for `fish --no-config`, so such a shell records nothing.

`fish_should_add_to_history` (fish >= 4): when the user defined it, fish calls
it with the command line and adds the command to its history only when it
returns 0; it then decides alone, leading space included (fish's own
documentation). The hook calls it once more with the same `$argv[1]` (checked
on fish 4.2.1: fish passes `fish_postexec` the same string) and records only on
0, instead of the leading-space rule. Calling it again is safe because fish
documents it as a predicate run before the command, which cannot see `$status`.

`command -q $__psychic_bin; or return` keeps a missing binary silent: without
it fish prints `<bin>: command not found` and the function's arguments, the
command text, at every prompt (E-R-01).

No `--started-at`: `track.rs` step 6 derives it from `--duration-ms`
(`$CMD_DURATION` is milliseconds). `$status` is read on the first line, before
anything can overwrite it.

## 12. Backfill

`src/cmd/backfill.rs`:
`pub(super) fn run(store: &CmdStore, locations: &Locations, home: &Path, now: i64, env_ignore: Option<&str>) -> anyhow::Result<BackfillReport>`,
with `pub(super) fn Locations::discover(home: &Path, xdg_data_home: Option<&Path>, zdotdir: Option<&Path>) -> Locations { atuin, zsh: Vec<PathBuf>, bash, fish }`
(the paths of 12.1). The caller (`mod.rs`) reads `$XDG_DATA_HOME` and `$ZDOTDIR`; the importer
never reads the process environment, so tests can point it at a scratch home without the
real `$XDG_DATA_HOME` leaking in. A `--enable` that needs to import with `HOME` unset fails
with `HOME is not set, so psychic can't find your shell history` before the marker is written.
Runs synchronously (a) inside `--enable` under the condition of section 13.1,
and (b) on `psychic history --import`. Idempotent: running it twice
inserts nothing the second time.

### 12.1 Sources, in this order

| Order | Kind | Path | `source_key` |
|---|---|---|---|
| 1 | Atuin | `$XDG_DATA_HOME/atuin/history.db`, else `~/.local/share/atuin/history.db` | `atuin:<path>` |
| 2 | zsh | `~/.zsh_history`, then `$ZDOTDIR/.zsh_history` if different (as [src/import/sources.rs:67-73](../../src/import/sources.rs)) | `zsh:<path>` |
| 3 | bash | `~/.bash_history` ([src/import/sources.rs:89](../../src/import/sources.rs)) | `bash:<path>` |
| 4 | fish | `$XDG_DATA_HOME/fish/fish_history`, else `~/.local/share/fish/fish_history` ([src/import/sources.rs:74-77](../../src/import/sources.rs)) | `fish:<path>` |

Atuin goes first: it has true cwd and exit codes, and the unique index
`idx_command_runs_import` then drops the same `(command, second)` arriving again
from a shell file. A missing or unreadable file is an empty source. `$HISTFILE`
is not consulted (it is rarely exported).

### 12.2 Changes to `src/import/`

| Location | Change |
|---|---|
| [src/import/mod.rs:17](../../src/import/mod.rs) | `pub(crate) mod shell_history;` |
| `src/import/mod.rs`, after `is_off_limits` ([:188](../../src/import/mod.rs)) | Add `pub(crate) fn is_dir_within_limits(path: &Path, home: &Path) -> bool { path.is_absolute() && !is_off_limits(path, home) && resolve_safely(path, home).is_some_and(\|r\| r.kind == PathKind::Dir) }`. It goes through `resolve_safely` ([:255](../../src/import/mod.rs)), not `Path::is_dir`, so a symlink into a macOS protected folder is never followed |
| [src/import/shell_history.rs:26](../../src/import/shell_history.rs) | `pub(super) struct HistoryCommand` -> `pub(crate)` |
| [src/import/shell_history.rs:92](../../src/import/shell_history.rs), [:136](../../src/import/shell_history.rs), [:158](../../src/import/shell_history.rs) | `parse_zsh`, `parse_bash`, `parse_fish`: `pub(super)` -> `pub(crate)` |
| `src/import/shell_history.rs`, after `parse_bash` | Add `pub(crate) fn parse_bash_entries(bytes: &[u8]) -> Vec<HistoryCommand>`: the file as bash reads it back. If its first line is a timestamp line (bash's `HIST_TIMESTAMP_START`: `#` and a digit), each non-timestamp line that does not directly follow a timestamp line is appended, after a `\n`, to the entry before it, so a multi-line command is one entry; the timestamp is the last timestamp line before the entry's first line. Otherwise it is `parse_bash` (bash itself reads such a file one entry per line, checked with bash 5.3 `history -r`). `parse_bash` and the path import (`replay`, [src/import/sources.rs:89](../../src/import/sources.rs)) are unchanged: they look at one line at a time |
| [src/import/shell_history.rs:372-383](../../src/import/shell_history.rs) | Extract into `fn command_words(words: Vec<Option<String>>) -> Option<NamedCommand>` with `struct NamedCommand { name: String, arguments: Vec<Option<String>> }` (a struct, not a tuple, per [llm.md](../../llm.md)) (skips `NAME=value`, `builtin`, `command`; returns name and arguments) |
| [src/import/shell_history.rs:385-404](../../src/import/shell_history.rs) | Extract the decision into `cwd_step` (below); `replay` calls it. Behaviour of `replay` is unchanged and its tests ([:507-581](../../src/import/shell_history.rs)) pass unmodified |
| `src/import/shell_history.rs`, after `replay` | Add `replay_cwds` (below) |

```rust
/// What one simple command does to the directory the replay believes it is in.
enum CwdStep {
    /// `cd`/`pushd`: the target before it is checked on disk; `None` = unknowable.
    Cd(Option<PathBuf>),
    /// A command in CWD_LOSING_COMMANDS.
    Lost,
    Unchanged,
}
fn cwd_step(name: &str, arguments: &[Option<String>], cwd: Option<&Path>,
            previous: Option<&Path>, home: &Path) -> CwdStep;

/// For each command, the directory it was typed in, as far as a replay from
/// `$HOME` can tell: the state *before* the command's own `cd`s apply.
pub(crate) fn replay_cwds(commands: &[HistoryCommand], home: &Path,
                          is_dir: &mut dyn FnMut(&Path) -> bool) -> Vec<Option<PathBuf>>;
```

`replay_cwds`: `cwd = Some(home)`, `previous = None`; for each command push
`cwd.clone()` to the output, then for each `simple_commands(&command.text)` ->
`command_words` -> `cwd_step`: `Cd(t)` sets `target = t.filter(|p| is_dir(p))`
and `previous = replace(&mut cwd, target)`; `Lost` sets `previous = cwd.take()`;
`Unchanged` does nothing. Output length equals input length.

### 12.3 Shell-file algorithm (per file)

1. `commands = parse_zsh|parse_bash_entries|parse_fish(bytes)`;
   `cwds = replay_cwds(&commands, home, &mut |p| crate::import::is_dir_within_limits(p, home))`.
2. Assign timestamps by one backward pass,
   `fn assign_times(timestamps: &[Option<i64>], now: i64, ceiling: i64) -> Vec<AssignedTime { at: i64, synthetic: bool }>`
   with `ceiling = min(now, mtime of the file in seconds + 1)`. `next = ceiling`.
   For `i` from last to first: if `timestamps[i] == Some(t)` and `t <= now`,
   then `at = t`, `synthetic = false`; else `at = next - 1`, `synthetic = true`.
   Then `next = at`. A real timestamp is kept even when the file is not in time
   order (several terminals write one zsh file). An untimestamped entry sits
   one second before the entry after it; a fully untimestamped file is spaced
   1 second apart ending at the file's mtime, so old plain bash history does not
   pose as "run just now".
3. `already = I2-select(source_key)`; `hook_start = I1` (NULL -> `i64::MAX`).
4. For the entries sorted by `(at, file index)`, skip the entry when any holds:
   text (trimmed) empty or over `CMD_MAX_TEXT_BYTES`; own command;
   `privacy::is_ignored`; I3 finds a tombstone; `!synthetic && at >= hook_start`
   (the hook has it); `synthetic && hook_start != i64::MAX &&` I5 finds a hook run of this text
   (an untimestamped entry cannot be compared by time, so a command the hook has
   already recorded is not imported a second time); `synthetic && already`
   (synthetic times are not reproducible, so a file's untimestamped entries are
   imported once only).
5. Otherwise `record_run_in` with `source = Import`, `exit_code = None`,
   `duration_ms = None`, `started_at = at[i]`, `cwd = cwds[i]`,
   `repo_root = cwd.and_then(|c| find_repo_root(c, Some(home)))`, `shell_session =
   "import:<kind>:<h>"` for real timestamps and `"import-synth:<kind>:<h>"` for
   synthetic ones, where `<h>` is `format!("{:016x}", fnv1a64(path) as u64)[..8]`.
   A `RunOutcome::Duplicate` counts as skipped.
6. After the file: I2-insert `(source_key, now, inserted)`.

All entries of one file go in one `CmdStore::transaction`, in ascending
`(at, file index)` order, so S3 finds each entry's predecessor.

### 12.4 Atuin

Opened read-only and never written:
`Connection::open_with_flags(path, SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX)`
followed by `PRAGMA query_only = ON` and `PRAGMA busy_timeout = 2000`.

Schema, from Atuin's public client migrations. **Assumptions A1-A4 are from
memory of that source and are checked at run time** by reading
`SELECT name FROM pragma_table_info('history')`; if `command`, `timestamp`,
`cwd`, `exit`, `duration` or `session` is missing the source is skipped with
`unavailable: Some("unexpected schema")` on its `SourceReport`.

| Atuin column | Assumption | Maps to |
|---|---|---|
| `command TEXT` | - | `text` |
| `timestamp INTEGER` | A1: nanoseconds since the epoch | `started_at` = value scaled to seconds by magnitude: `>= 10^17` / 10^9; `>= 10^14` / 10^6; `>= 10^11` / 10^3; else as is |
| `duration INTEGER` | A2: nanoseconds, `-1` = unknown | `duration_ms = value / 1_000_000` when `value >= 0`, else NULL |
| `exit INTEGER` | A3: `-1` = unknown | `exit_code`, NULL when negative |
| `cwd TEXT` | A4: the literal `unknown` when Atuin itself imported the row | `cwd`, NULL when empty, `unknown`, or not starting with `/` |
| `session TEXT` | - | `shell_session = "atuin:<session>"` |
| `deleted_at` | present only in newer schemas | Add `WHERE deleted_at IS NULL` only when the column exists |

Query: `SELECT command, timestamp, duration, exit, cwd, session FROM history
[WHERE deleted_at IS NULL] ORDER BY timestamp, id`. Each row then follows steps
4-5 of 12.3 with `synthetic = false` and `source = Import`.

`BackfillReport { sources: Vec<SourceReport { kind, key, found, inserted, skipped, unavailable: Option<&'static str> }> }`
(`unavailable` says why Atuin could not be read: `Some("locked")` when SQLite reported `SQLITE_BUSY` or
`SQLITE_LOCKED` past the 2 s busy timeout, `Some("unreadable")` for any other open or read failure (not a
database, damaged, no permission), `Some("unexpected schema")` when a column is missing; a missing
file is `None` with `found 0`). A source's whole file is one transaction that also writes its
`command_imports` row, so a failed file leaves no trace (E-R-14). The filter of 12.3 step 4 that
also applies to `track-command` (empty, too long, own, ignored) is one function, `track::is_recordable`.
with `summary()` = `Imported <total> commands (atuin <n>, zsh <n>, bash <n>, fish <n>).`
where each number is `inserted` with `,` as thousands separator, zsh sums its
files, and a skipped Atuin source prints `atuin skipped (<reason>)` in place of
`atuin <n>`. Example: `Imported 18,402 commands (atuin 12,001, zsh 6,401, bash 0, fish 0).`

## 13. `psychic history` actions built in this phase

### 13.1 `--enable [--no-import]`

1. `create_dir_all(data_dir)`; `CmdStore::open` (tightens permissions).
2. `first = !marker.exists() && I4 == 0`.
3. Write the marker file: contents `"<now>\n"`, then `tighten_permissions`.
4. If `first && !no_import`: run backfill and print its `summary()`.
5. Print exactly:

```
Shell history search is on.
  Recording: every command you run from now on, with its directory, exit code and duration.
  Not recorded: what your shell's own settings keep out of its history (bash: HISTCONTROL,
    HISTIGNORE; zsh: HIST_IGNORE_SPACE, HISTORY_IGNORE, HIST_NO_STORE; fish: a leading space,
    or fish_should_add_to_history), psychic's own commands, and anything matching
    PSYCHIC_HISTORY_IGNORE (colon-separated patterns; it must be exported).
  Recorded anyway: lines a zsh zshaddhistory hook rejects; add them to PSYCHIC_HISTORY_IGNORE.
  Stored in: <data dir>/events.db (readable only by you). Nothing leaves this machine.
  Ctrl-R: open a new shell to get the binding.
  Undo: psychic history --disable   Delete everything: psychic history --purge
```

### 13.2 `--disable`

Remove the marker file if present. Print
`Shell history search is off. Recorded commands are kept; delete them with: psychic history --purge`.

### 13.3 `--status`

Prints, one per line: `recording: on|off` (marker exists; then
` (PSYCHIC_HISTORY=<v> overrides)` when the variable is `0` or `1`),
`data dir: <path>`, `commands: N`, `runs: N (hook H, import I)`, `picks: N`,
`model: none` (Phase C replaces this line, [03 section 6.4](03-learn.md#64-status-line)).

## 14. Anti-patterns (DO NOT)

| Don't | Do instead | Why |
|---|---|---|
| Interpolate the command into a shell string (`psychic track-command "... $cmd ..."` built by `eval`, or `--command="$1"`) | Pass it as one argv element after `--` | Quotes, newlines and a leading `-` must survive; `--` ends option parsing |
| Call `psychic` from the pre-exec side, or twice per command | Stash in shell variables; one background call before the prompt | Two spawns per command double the cost and race each other |
| Run the hook's `psychic` in the foreground or wait for it | `&!` (zsh), `( ... &)` (bash), `& disown` (fish), output to `/dev/null` | The prompt must never block on psychic |
| Spawn anything when recording is off | The builtin file test of `__psychic_history_enabled` | Requirement: no process per command while disabled |
| Install or replace a bash `DEBUG` trap, or assign `PS0` | `PROMPT_COMMAND` + `history 1` (section 11.3) | bash-preexec owns the DEBUG trap; Starship uses `PS0` |
| Set up fern or open `app.log` in `track-command` | Early dispatch M1 | B4, and section 10.6 |
| Skip the close-time checkpoint without bounding the log, or rely on `wal_autocheckpoint`/`journal_size_limit` for it | `open_for_recording` + `close_recording` (section 9.1) | A lone process rebuilds the log index with nothing checkpointed, so SQLite never rewinds the log: it only grows |
| `PRAGMA synchronous = OFF` (or `journal_mode = MEMORY/OFF`) to make `track-command` fast | Section 9.1 | A checkpoint without `fsync` followed by deleting the log can corrupt the database on power loss; only losing the last runs is acceptable |
| Let W1 wait on `busy_timeout` | `busy_timeout = 0` before W1; a busy result is left for a later run | A picker mid-read would hold the shell's background process for up to 5 s |
| `log::info!("recorded {}", text)` or `{:?}` of a `NewRun` anywhere | Log ids and counts; do not derive `Debug` on `NewRun` | Section 10.6 |
| Write SQL on the command tables outside `src/cmd/store.rs` | Add a method to `CmdStore` and a row to section 6 | One place to test plans and to audit |
| Recompute `run_count` etc. with `COUNT(*)` at read time | Maintain the aggregates in S5 | Picker start must not scan 500,000 runs |
| Bump aggregates before knowing the run was inserted | S4, then S5 only when `changes() == 1` | A duplicate import would inflate `run_count` |
| Canonicalise `cwd` or resolve symlinks | Store `$PWD` as given (ADR-7) | Hook and widget must agree with zero syscalls |
| Re-implement `HISTIGNORE`/`HISTCONTROL` matching for bash | Rely on bash's own history list | The shell's own decision is the definition of the opt-out |
| Add the `regex` crate for `PSYCHIC_HISTORY_IGNORE` | `privacy::glob_match` | Upstream dropped the dependency in `7b99679` |
| Edit `~/.zsh_history` or other shell history files | Tombstone in `deleted_commands` | Non-goal; another process owns those files |
| chmod inside `Database::new` | `privacy::tighten_permissions` from `CmdStore::open` | Keeps `src/db.rs` to a one-line edit and leaves file-only users untouched |
| Rename or reuse `src/history.rs`, `history_mode`, `ToggleHistoryMode` | `cmd`/`command` identifiers (ADR-1) | Those are directory back/forward |
| `eprintln!` from `src/cmd/` | Return `Err` from the subcommand; `println!` for user-facing action output | [llm.md:212](../../llm.md) |

## 15. Test case specifications

Conventions: inline `#[cfg(test)] mod tests` per file, sentence-style names,
expected values written out ([llm.md:216-235](../../llm.md)); `CmdStore::in_memory()`
or `std::env::temp_dir().join(format!("psychic-<tag>-{}", std::process::id()))`.
No test writes `~/.local/share/psychic` or reads the real `$HOME` history
(the only reads of the real data dir are the two guards TC-N-09 and IT-N-05 of
[04 section 11](04-bench.md#11-test-case-specifications), which `stat` it).
Every test uses its own `<tag>` and never removes and recreates a data dir
within one test: `Database`'s schema registry is keyed by path
([src/db.rs:154-155](../../src/db.rs)) and would not re-create the tables.

### 15.1 Unit tests

| ID | Component | Input | Expected | Edge cases covered |
|---|---|---|---|---|
| TC-R-01 | `templatize` | Each row of the table in section 7 | The template in that row | quotes, pipes, env assignment, newline, version number, URL |
| TC-R-02 | `raw_words` | `a\|\|b && c;d` | words `a`,`b`,`c`,`d` with operators `\|\|`,`&&`,`;` between | adjacent operators, no spaces |
| TC-R-03 | `first_command_word` | `FOO=1 command psychic zsh`; `sudo pd x`; `echo psychic` | `psychic`; `pd`; `echo` | assignment, `command`, `sudo` prefixes; own name as an argument is not own |
| TC-R-04 | `glob_match` | (`*password*`, `mysql -ppassword1`), (`git ?`, `git s`), (`git ?`, `git st`), (`[a-c]x`, `bx`), (`[!a-c]x`, `bx`), (`a\*b`, `a*b`), (`a\*b`, `aXb`), (`[x`, `[x`) | true, true, false, true, false, true, false, true | whole-text anchoring, escape, unclosed bracket |
| TC-R-05 | `is_ignored` | text `export TOKEN=1`; patterns `None`, `""`, `ls:export *`, `:` | false, false, true, false | empty list elements |
| TC-R-06 | `fnv1a64` | `""`, `"a"` | `0xcbf29ce484222325 as i64`, `0xaf63dc4c8601ec8c as i64` | published FNV-1a vectors |
| TC-R-07 | `find_repo_root` | temp tree with its own `home`: `home/r/.git/`, `home/r/a/b/`, `home/plain/`; home = `home`; inputs `home/r/a/b`, `home/r`, `home/plain`, `home`, `relative/x` | `Some(r)`, `Some(r)`, `None`, `None`, `None` | `.git` as a file (`home/r2/.git` file) -> `Some(r2)`. Second tree with `home/.git/`: `home/notes/2024` -> `None` (also with home given as `home/`), `home/src/app/src` with `home/src/app/.git` -> `Some(home/src/app)`, `home/notes/2024` with home `None` -> `Some(home)`. Third: `srv/repo/.git`, input `srv/repo/x/y` outside home -> `Some(srv/repo)`. No answer depends on what lies above the temp dir |
| TC-R-08 | `record_run` aggregates | same text 3 times: exit 0 at t=100, exit 1 at t=200, exit NULL at t=150 | `run_count 3, success 1, fail 1, first_run_at 100, last_run_at 200, last_exit_code 1` | out-of-order arrival does not overwrite `last_exit_code` |
| TC-R-09 | `record_run` predecessor | session `s`: A at 100, B at 160, C at 160+1801; session `t`: D at 170 | B.prev = A, C.prev = NULL (gap), D.prev = NULL (other session) | `CMD_SEQUENCE_MAX_GAP_SECS` boundary: gap exactly 1800 -> linked |
| TC-R-10 | import uniqueness | `record_run` Import twice with same text and `started_at` | second returns `Duplicate`; `run_count == 1` | Hook rows with the same pair are both inserted |
| TC-R-11 | every plan | `EXPLAIN QUERY PLAN` of S2, S3, R1 ids, R1-R8, R9, R10, D2-D4, I1 with a `plan()` helper shaped like [src/db.rs:1424-1433](../../src/db.rs) | Each contains the string in its "Expected plan contains" cell; R5 does not contain `TEMP B-TREE` | R6 with 1, 2 and 3 placeholders |
| TC-R-12 | `create_schema` idempotence | call 3 times on `:memory:` | no error; `SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'idx_command%'` = 11 | - |
| TC-R-13 | `assign_times` | timestamps `[None, Some(50), None, None]`, now 100, ceiling 100 | `at = [49, 50, 98, 99]`, `synthetic = [true, false, true, true]` | `[Some(500)]` -> `at = [99]`, synthetic (future timestamp); `[Some(30), Some(20), Some(10), Some(5)]` -> all kept, none synthetic (out-of-order file); ceiling 60 with `[None]` -> `at = [59]` |
| TC-R-14 | `replay_cwds` | home `/h`, dirs `/h/a`, `/h/a/b`; commands `cd a`, `ls`, `cd b && make`, `z foo`, `ls`, `cd /h/a`, `pwd` | `[/h, /h/a, /h/a, /h/a/b, None, None, /h/a]` | the command's own `cd` does not apply to itself; lost and regained cwd |
| TC-R-15 | `track::run` skips | texts `"   "`, 8193 x `a`, `p`, `pd foo`, `psychic history`, `export TOKEN=1` with ignore `export *` | 0 rows in `commands` after each | - |
| TC-R-16 | `track::run` derives the start | now 1000, `--duration-ms 2500`, no `--started-at` | `started_at == 998` | `--started-at 990` wins; no duration -> 1000 |
| TC-R-17 | `tighten_permissions` | temp dir 0755 with `events.db` 0644 | dir `0o700`, file `0o600`; a second call performs no `set_permissions` (modes already right) | missing `-wal`/`-shm` are not an error |
| TC-R-18 | `delete_command` | A (3 runs, 2 events), B with `prev = A` | A gone from all three tables; B.prev NULL; `deleted_commands` has `fnv1a64(A.text)` | - |
| TC-R-19 | clap surface | `["psychic","track-command","--cwd","/x","--session","s","--","-rf --weird\nline"]` | `command == "-rf --weird\nline"` | missing `--` -> parse error; `history --enable --purge` -> parse error (group); `--cwd` with the bytes `/tmp/caf\xe9` parses |
| TC-R-20 | `parse_bash_entries` | `#1700000000`, `echo one`, `#1700000001`, `echo "multi`, `line"`, `#1700000002`, `#1700000003`, `ls`, `#1700000004` | `echo one`@..000, `echo "multi\nline"`@..001, `ls`@..003 | two timestamp lines in a row (the last wins); a trailing timestamp line; a file not starting with a timestamp line is one entry per line, as bash reads it (`old plain`, `#1700000001`, `echo "multi`, `line"` -> 3 entries) |
| TC-R-21 | `purge` on disk | a file data dir with 200 runs of a sentinel text | after `purge`: `log_cleared`; neither `events.db` nor `events.db-wal` holds the sentinel's bytes, before and after the store is dropped | the same with a second, idle connection open; a second connection inside a read transaction -> `log_cleared == false`, rows still deleted; the `--purge` action then prints its line and fails with E-R-23 |
| TC-R-22 | Atuin failures | `history.db` of 4096 bytes of `Z`; a real Atuin database under another connection's `BEGIN EXCLUSIVE` | `unavailable` `Some("unreadable")`, summary `atuin skipped (unreadable)`; `Some("locked")` | - |
| TC-R-23 | `track::run` cwd | `--cwd` with the bytes `/tmp/caf\xe9`; a cwd `H/notes` with `H/.git` and home `H` | cwd stored as `/tmp/caf\u{FFFD}`; `repo_root` NULL | - |
| TC-R-25 | recording log, unbounded vs bounded | a file data dir; 20 one-shot `open_for_recording` + record + drop; then a fresh dir with 60 one-shot runs closed by `close_recording(64 KiB)` | without the bound the log grows on every run (> 256 KiB after 20); with it the log never exceeds 128 KiB, `Truncated` at least 3 times, the log is 0 bytes after each, all 60 runs readable | a run closed with `close_recording(u64::MAX)` leaves the log non-empty and the run readable by a new connection |
| TC-R-26 | recording log, busy | log over 64 KiB; another connection inside a read transaction | `close_recording` returns `Busy` in under 1 s and the log stays; after the reader commits (still open, idle) the next one returns `Truncated` and the log is 0 bytes | - |
| TC-R-27 | purge after recording connections | 30 one-shot runs of a sentinel text closed with `close_recording(u64::MAX)` (text in `-wal`) | `purge` from a normal `open`: `log_cleared`; neither file holds the sentinel, before and after drop. `events.db`, `-wal`, `-shm` are `0600` after a recording run | - |
| TC-R-24 | `backfill::run`, bash | `.bash_history` `#1700000000`, `for f in *; do`, `  echo "$f"`, `done`, `#1700000005`, `ls` | 2 runs: the loop as one text with its newlines at 1700000000, `ls` at 1700000005 | - |

### 15.2 Integration tests

| ID | Flow | Setup | Verification | Teardown |
|---|---|---|---|---|
| IT-R-01 | Older database gains the command tables (shape of [src/db.rs:1053](../../src/db.rs)) | Temp dir; create `events.db` by hand as an older database holding only an `events` and a `sessions` table (current upstream column lists), one row each | `Database::new` succeeds; the old rows are intact; `commands`, `command_runs`, `command_events`, `command_imports`, `deleted_commands` exist | `remove_dir_all` |
| IT-R-02 | Backfill is idempotent | Temp home with `.zsh_history` (extended format, 4 commands: `p`, and three different commands of which two share one second) and `.bash_history` (3 lines, no timestamps, mtime set to `M`); temp data dir | First `backfill::run`: zsh 3 inserted, bash 3 inserted with `shell_session LIKE 'import-synth:bash:%'` and `started_at` = M-2, M-1, M. Second run: 0 inserted, row counts unchanged | `remove_dir_all` |
| IT-R-03 | Import stops at the first hook run | As IT-R-02 plus one `Source::Hook` run at t=1,700,000,050 before importing a zsh file with entries at ...000 and ...100 | Only the ...000 entry is imported | `remove_dir_all` |
| IT-R-04 | Atuin import | Build a temp `history.db` with the A1-A4 schema and 4 rows with nanosecond timestamps: (exit 0, cwd `/x`), (exit 1, cwd `unknown`), (exit -1, cwd `/y`), and a fourth with `deleted_at` set | 3 rows imported; `started_at` in seconds; `exit_code` 0, 1, NULL; `cwd` `/x`, NULL, `/y`; `duration_ms` NULL where Atuin's is -1; the Atuin file's mtime and size are unchanged | `remove_dir_all` |
| IT-R-05 | Tombstone blocks re-import | Import, `delete_command` one, import again | The deleted text is absent after the second import | `remove_dir_all` |
| IT-R-08 | Tombstones outlive purge | `.zsh_history` (timestamped) and `.bash_history` (untimestamped), each with a synthetic secret and a kept command; import, `delete_command` both secrets, `purge`, import again | After purge: `commands` and `command_imports` empty, `deleted_commands` 2 rows; after the second import both kept commands are back (the bash one only because `command_imports` was emptied) and neither secret is. Purge's unit test checks a tombstone survives it | `remove_dir_all` |
| IT-R-06 | Enable, disable, purge | Temp data dir; `--enable --no-import`, record 2 runs, `--status`, `--purge --yes`, `--disable` | Marker exists then not; modes 700/600; status lines as section 13.3; after purge all five tables are empty (nothing was tombstoned) and the marker is still present | `remove_dir_all` |
| IT-R-07 | No command text leaks (`tests/no_text_leaks.rs`, the built binary, so its real logger writes the real `app.log`) | Temp `HOME` (cleared environment) with `.bash_history`, `.zsh_history` and a non-database Atuin `history.db`, all holding `SENTINEL_c0ffee`; `--data-dir` in the temp dir. Run `history --enable` (imports), `track-command` with the sentinel (succeeding, and failing on a data dir that is a file), `history --import`, `--status`, `--purge --yes` | No invocation's stdout or stderr contains the sentinel; it is in `events.db*` before the purge (so it went through every path); `app.log` exists and holds `atuin skipped (unreadable)` (so a leak would show); no file in the data dir other than `events.db*`, recursively, contains the bytes; nothing was written to `HOME/.local/share/psychic`. Checked to fail when a `log::debug!` of the text is added to the importer | `remove_dir_all` |
| IT-S-zsh, IT-S-bash, IT-S-fish | Real shell, real hook (`tests/shell_hooks.rs`) | Section 15.3 | Section 15.3 | `remove_dir_all` |

### 15.3 Shell-level tests (`tests/shell_hooks.rs`)

One `#[test]` per shell for the session below, once with `PSYCHIC_HISTORY=1` and
once with the marker file instead (`psychic --data-dir T/data history --enable
--no-import`, run with `HOME=T/home`, and no `PSYCHIC_HISTORY`); then the
scenario tests after the table. Binary: `env!("CARGO_BIN_EXE_psychic")`.

| Step | Detail |
|---|---|
| Skip rule | If `Command::new(<shell>).arg("--version")` fails to spawn: `println!("skipped: <shell> not installed")` and return (pass) |
| Scratch | `T = temp_dir()/psychic-shell-<shell>-<tag>-<pid>`; `T/home`, `T/data`, `T/work/sub`; `T/hook` = stdout of `psychic --data-dir T/data <shell>` run with `HOME=T/home` |
| Database | Created before the shell starts (`psychic --data-dir T/data history --status`, or the `--enable` of the marker variant): otherwise the session's first two background `track-command`s both create it, and one can lose the race to switch it to WAL mode (`database is locked`) and drop a run |
| Environment (cleared, then) | `HOME=T/home`, `PATH` inherited, `TERM=dumb`, `PSYCHIC_BIN=<binary>`, `PSYCHIC_DATA_DIR=T/data`, `PSYCHIC_HISTORY=1`, `PSYCHIC_NO_RETRAIN=1`, `HISTFILE=T/home/hist`; bash also `HISTCONTROL=ignorespace`, `HISTIGNORE='secret*'`; zsh also `ZDOTDIR=T/home` |
| Launch | On a pty (`libc::openpty`; child via `std::process::Command` with the slave as stdin/stdout/stderr and `pre_exec` doing `setsid()` + `ioctl(0, TIOCSCTTY, 0)`; window 24x80). zsh: `zsh -f -i`; bash: `bash --norc --noprofile -i`; fish: `fish -i -C 'source T/hook'` with `XDG_CONFIG_HOME=T/home/.config` and `XDG_DATA_HOME=T/home/.local/share` (not `--no-config`: fish 4 sets `fish_private_mode` under `--no-config`, and the hook records nothing in private mode) (the hook is loaded before the first prompt, because `fish_postexec` would otherwise record the `source` line itself). cwd `T/work` |
| Drive | A reader thread drains the master. Write each line + `\n`, sleeping 500 ms before the first line and 150 ms between lines |
| Lines | (zsh only) `setopt histignorespace`, `HISTORY_IGNORE='secret*'`; then for all: (zsh, bash) `source T/hook`; then `true`, `false`, `cd sub`, `echo in-sub`, ` echo leading-space`, (zsh, bash) `secret-thing`, `echo 'multi` / `line'`; then the single byte `0x04` (Ctrl-D on an empty line ends all three shells without a command that could itself be recorded) |
| Wait | Child exit within 30 s (else kill and fail). The `0x04` is re-sent every 2 s until the shell exits: fish 4.2 sometimes drops a Ctrl-D that arrives while it is still finishing the previous command (seen with recording off too, after a `cd` whose upstream `track-visit` hook fails), and a second Ctrl-D on an empty line does nothing else. Then poll `SELECT COUNT(*) FROM command_runs` every 50 ms for up to 5 s until it equals the expected count |
| Expected rows, compared sorted (text / cwd / exit_code / source) | `true` / `T/work` / 0 / hook; `false` / `T/work` / 1 / hook; `cd sub` / `T/work` / 0 / hook; `echo in-sub` / `T/work/sub` / 0 / hook; the multi-line echo: text starts with `echo 'multi`, contains a newline or `;`, ends with `line'` / `T/work/sub` / 0 / hook. Exactly 5 rows; no text contains `leading-space` or `secret`; all 5 share one `shell_session`; `commands.template` of `cd sub` is `cd PATH`. Not by `started_at, id`: the runs land in the same second and their background `track-command`s take the write lock in any order |

Scenario tests (same scratch, launch and environment; rows compared as a
sorted list of `(text, exit_code)`):

| Test | Steps | Expected runs |
|---|---|---|
| bash, `history -a; history -n` | `source T/hook`; `PROMPT_COMMAND="$PROMPT_COMMAND; history -a; history -n"`; `echo one`; (another shell appends `echo from-another-shell` to `T/home/hist`); empty line; ` echo ignored`; (append `echo from-another-shell-2`); empty line; Ctrl-C; `false`; `echo two`; 300 ms between steps | the `PROMPT_COMMAND=...` line, `echo one`, `echo two` (0), `false` (1): nothing of the other shell, nothing for the empty lines, Ctrl-C or the ignored command, nothing twice |
| bash, `history -a; history -c; history -r` | as above with that `PROMPT_COMMAND` | as above |
| bash, `history -a; history -n` prepended | `PROMPT_COMMAND="history -a; history -n; $PROMPT_COMMAND"`; `echo one`; (append); ` echo ignored`; (append); `echo two`; `echo three` | `echo one`, `echo two`, `echo three`: the hook moves back to the front; the prepending line itself is not recorded (section 11.3) |
| bash, next to bash-preexec | `source T/bp`, a stand-in that re-pins `__bp_precmd_invoke_cmd` first and `__bp_interactive_mode` last (an array element) at every prompt, as bash-preexec does, without its DEBUG trap; `PROMPT_COMMAND="$PROMPT_COMMAND; history -a; history -n"`; `echo one`; (append); ` echo ignored`; `false`; `echo two` | the `source` and `PROMPT_COMMAND=...` lines, `echo one`, `false` (1), `echo two`: no fight over the ends of `PROMPT_COMMAND` |
| bash, `HISTCONTROL=ignoredups` | `echo same`, `echo same`, `echo other` | `echo same` once, `echo other` (the decision of section 11.3) |
| bash, `$?` after the hooks | `PROMPT_COMMAND="$PROMPT_COMMAND; echo \$? >> T/seen"`; `false`; `true`; `(exit 3)` | `T/seen` ends with `1`, `0`, `3` |
| zsh, `HIST_NO_STORE` | `setopt histnostore` and `alias h=history` before `source T/hook`; `history`, `fc -rl`, `builtin history 1`, `h >/dev/null`, `fc -W /dev/null`, `echo history`, `history;echo y` | `echo history`, `fc -W /dev/null`, `history;echo y` |
| fish, `fish_should_add_to_history` | `T/fish-init` defines it to reject `secret*`, sourced after the hook by `-C`; `echo kept`, `secret-thing`, ` echo spaced` | `echo kept`, `echo spaced` (the function decides alone, leading space included) |
| fish, binary missing | `PSYCHIC_BIN=T/no-such-psychic`; `echo first`, `echo second` | The terminal shows neither the missing path nor `__psychic_postexec` |

Each scenario test except the `ignoredups` one fails against the hooks of
commit `4c37b59`. Old bash versions are not in the suite (one bash per
machine); they were checked by hand in the `bash:3.2`, `4.3`, `4.4`, `5.0` and
`5.1` images: no line of another shell, no ignored command and nothing twice
was recorded, and every command at 0.3 s spacing was, except the documented
ones (section 11.3).

The pty helper (about 60 lines) lives in `tests/support/mod.rs`, declared with
`mod support;` in `tests/shell_hooks.rs`; Phase B's `tests/picker.rs` uses the
same module. It is not shared with `src/`.

## 16. Error handling matrix

| # | Failure | Detection | Response | Fallback | Logging |
|---|---|---|---|---|---|
| E-R-01 | `psychic` not on PATH in a hook | Shell cannot exec; fish checks first with `command -q` (section 11.4) | Nothing: background job fails, output discarded; fish returns before calling it | Command not recorded | None |
| E-R-02 | No data dir (`HOME` unset, no `--data-dir`) | `data_dir.is_none()` | `Err("No data directory: pass --data-dir, or set HOME")`, exit 1 | Not recorded | None (stderr discarded by the hook) |
| E-R-03 | Database locked more than 5 s | `SQLITE_BUSY` after `busy_timeout = 5000` ([src/db.rs:209](../../src/db.rs)) | `Err("recording a command failed: database busy")`, exit 1 | That run is lost; no retry | None |
| E-R-04 | Disk full / read-only data dir | rusqlite or io error | `Err` naming the operation and path, exit 1 | Not recorded | None |
| E-R-05 | Clap rejects the hook's arguments (non-numeric `--exit-code`, missing `--`) | clap error | clap's message on stderr (discarded), exit 2 | Not recorded | None |
| E-R-06 | Command text or `--cwd` is not UTF-8 | `to_string_lossy` replaces bytes (`--cwd` is a `PathBuf`, so clap accepts any bytes) | Recorded with U+FFFD; the repository is looked up on the real path first | - | None |
| E-R-07 | Command over `CMD_MAX_TEXT_BYTES`, empty, own, or ignored | Section 9 steps 2-5 | `Ok(())`, exit 0 | Not recorded by design | None |
| E-R-08 | `chmod` fails (file owned by another user) | `tighten_permissions` returns `Err` | `CmdStore::open` returns `Err("could not make <path> private")` | `track-command`: not recorded. `--enable`: prints the error, marker not written | Picker/actions: `log::error!` with the path |
| E-R-09 | Marker file cannot be written | io error in `--enable` | `Err("could not write <marker path>")`, exit 1 | Recording stays off | stderr via `main` |
| E-R-10 | History file missing or unreadable | `fs::read` error | Source reports `found 0` | Other sources continue | `log::info!("import: <kind> history not readable")` |
| E-R-11 | History file is not valid UTF-8 | - | `from_utf8_lossy` (upstream parsers, [src/import/shell_history.rs:93](../../src/import/shell_history.rs)) | - | None |
| E-R-12 | Atuin DB missing | `!path.exists()` | Source skipped, `found 0` | - | None |
| E-R-13 | Atuin DB locked, unreadable, or unexpected schema | open/query error with `SQLITE_BUSY`/`SQLITE_LOCKED` -> `locked`; any other open/query error (not a database, damaged, permission) -> `unreadable`; `pragma_table_info` check fails -> `unexpected schema` | `unavailable: Some(<reason>)` in the report; summary prints `atuin skipped (<reason>)` | Shell files still import | `log::warn!` with the reason, no row data |
| E-R-14 | Import transaction fails mid-file | rusqlite error | Roll back that file's transaction; `command_imports` not updated; continue with the next source | A later `--import` retries the file (synthetic entries included, since I2 has no row) | `log::error!("import of <source_key> failed: <sqlite code>")` |
| E-R-15 | `--purge` without `--yes` and stdin is not a terminal | `!std::io::stdin().is_terminal()` | `bail!("stdin is not a terminal, so psychic can't ask before deleting; pass --yes")` (wording as [src/shell_init.rs:269-271](../../src/shell_init.rs)) | Nothing deleted | stderr via `main` |
| E-R-16 | zsh without `zsh/datetime` | `EPOCHREALTIME` empty | Hook omits `--started-at` and `--duration-ms` | `track.rs` uses `now`; duration NULL | None |
| E-R-17 | bash older than 4.2 | `printf '%(%s)T'` fails | Hook omits `--duration-ms` | duration NULL | None |
| E-R-18 | bash history disabled or command suppressed by `HISTCONTROL`/`HISTIGNORE` | `$HISTCMD` where the sync left it, newest entry stamped before the sync | Not recorded | By design | None |
| E-R-19 | Two `track-command` processes at once | SQLite write lock | Second waits on `busy_timeout` | - | None |
| E-R-24 | `track-command` finds the log over `CMD_WAL_TRUNCATE_BYTES` while another connection writes, checkpoints or reads | W1 returns busy = 1 (`busy_timeout` 0) | `LogBound::Busy`, `Ok(())`, exit 0: the run is recorded | The log stays over the bound until a later run (or the last connection closing) truncates it | None |
| E-R-20 | Clock goes backwards between runs | `started_at` smaller than the session's last | S3 (`started_at <= ?2`) finds an older predecessor or none; S5 keeps `last_run_at` as the max | - | None |
| E-R-22 | `--import`, or a first `--enable` that imports, with `HOME` unset | `env.home()` is `None` | `Err("HOME is not set, so psychic can't find your shell history")`, exit 1 | `--enable`: marker not written | stderr via `main` |
| E-R-21 | A shell needed by a shell test is not installed | `--version` spawn fails | Test prints `skipped: ...` and passes | - | test stdout |
| E-R-23 | `--purge` while another connection is inside a read (a running picker, say) | `PRAGMA wal_checkpoint(TRUNCATE)` returns busy = 1 after `busy_timeout`, or `VACUUM` fails with `SQLITE_BUSY`/`SQLITE_LOCKED` | The rows are deleted and `Deleted ...` is printed; then `Err("another psychic process has <db> open, so the deleted commands can still be on disk in its files; close running psychic instances and run psychic history --purge again")`, exit 1 | Rerun once the other process is gone; the next checkpoint clears the log | stderr via `main` |

## 17. References

| Topic | Location |
|---|---|
| Targets B4, B9; ADR-4..ADR-9, ADR-18, ADR-20 | [00-blueprint.md section 2](00-blueprint.md#2-success-metrics), [section 8](00-blueprint.md#8-architecture-decision-records) |
| Complete list of upstream edits | [00-blueprint.md section 4.2](00-blueprint.md#42-edits-to-upstream-files-the-complete-list-anything-else-is-a-spec-violation) |
| Picker flags and behaviour | [02-browse.md section 4](02-browse.md#4-picker-invocation) |
| Who writes `command_events` | [02-browse.md section 10](02-browse.md#10-event-logging) |
| Who reads T2-T4 | [03-learn.md section 5](03-learn.md#5-training-set-replay) |
| Connection pragmas | [src/db.rs:207-216](../../src/db.rs) |
| Schema creation and migration pattern | [src/db.rs:219-294](../../src/db.rs), [src/db.rs:303-402](../../src/db.rs) |
| Once-per-file schema registry | [src/db.rs:154-155](../../src/db.rs), [src/db.rs:179-188](../../src/db.rs) |
| Migration test to imitate | [src/db.rs:1053-1100](../../src/db.rs) |
| Plan-test helper to imitate | [src/db.rs:1415-1452](../../src/db.rs) |
| Logger set-up that `track-command` must precede | [src/main.rs:229-265](../../src/main.rs) |
| Subcommand dispatch and the `TrackVisit` precedent | [src/main.rs:268-273](../../src/main.rs), [src/main.rs:457-499](../../src/main.rs) |
| History parsers and the cd replay | [src/import/shell_history.rs:92-193](../../src/import/shell_history.rs), [src/import/shell_history.rs:352-420](../../src/import/shell_history.rs) |
| Existing shell hook conventions | [shell/psychic.zsh:14-24](../../shell/psychic.zsh), [shell/psychic.bash:16-27](../../shell/psychic.bash), [shell/psychic.fish:14-17](../../shell/psychic.fish) |
| Help screen derives CLI rows from clap | [src/help.rs:121-134](../../src/help.rs) |
| Test style rules | [llm.md:214-243](../../llm.md) |
