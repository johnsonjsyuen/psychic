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
`tests/shell_hooks.rs`, `tests/support/mod.rs`.

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
| `shell/psychic.zsh`, `shell/psychic.bash`, `shell/psychic.fish` | Append the blocks of section 11 at end of file |
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

## 4. CLI surface

`src/cmd/cli.rs`. This table is the single definition of every flag of both
subcommands; the Owner column says which document specifies the behaviour.

### 4.1 `psychic track-command`

```rust
#[derive(clap::Args, Debug)]
pub struct TrackCommandArgs {
    /// Directory the command was typed in
    #[arg(long)] pub cwd: String,
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
    #[cfg(test)] pub(super) fn in_memory() -> Self;   // Database::new(Path::new(":memory:"))
}
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
| S1 | `INSERT INTO commands (text, template, first_run_at, last_run_at) VALUES (?1, ?2, ?3, ?3) ON CONFLICT(text) DO NOTHING` | `sqlite_autoindex_commands_1` |
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
| R1 | `SELECT id, text, last_run_at, run_count, success_count, fail_count, pick_count, last_exit_code FROM commands` | `SCAN commands` (the one deliberate full scan) |
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
| P1 | `DELETE FROM command_events; DELETE FROM command_runs; DELETE FROM commands; DELETE FROM command_imports; DELETE FROM deleted_commands;` then, outside the transaction, `VACUUM` | purge |
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
| `load_commands()` | R1 | `Vec<CommandRow { id: i64, text: String, last_run_at: i64, run_count: u32, success_count: u32, fail_count: u32, pick_count: u32, last_exit_code: Option<i32> }>` |
| `runs_in_cwd(cwd: &str)` | R2 | `Vec<CwdRunsRow { command_id: i64, runs: u32, last_run_at: i64 }>` |
| `runs_in_repo(repo_root: &str)` | R3 | `Vec<CountRow { command_id: i64, runs: u32 }>` |
| `runs_since(cutoff: i64)` | R4 | `Vec<RecentRunRow { command_id: i64, started_at: i64, exit_code: Option<i32> }>` |
| `session_tail(shell_session: &str)` | R5 | `Vec<String>` (templates, newest first, at most 3) |
| `followers(templates: &[String])` (1 to 3 distinct) | R6 | `Vec<FollowerRow { command_id: i64, template: String, runs: u32 }>` |
| `session_commands(shell_session: &str)` | R7 | `Vec<i64>` |
| `picks_since(cutoff: i64)` | R8 | `Vec<PickRow { command_id: i64, query: String, timestamp: i64, episode_queries: Vec<String> }>` (unparseable JSON -> empty vector) |
| `command_dirs(command_id: i64)` | R9 | `Vec<DirUse { dir: String, runs: u32, last_run_at: i64 }>` |
| `command_predecessors(command_id: i64)` | R10 | `Vec<PrecededBy { template: String, runs: u32 }>` |
| `log_events(now: i64, events: &[NewEvent])` (one transaction) | E1 | `()`; `NewEvent` has one field per E1 parameter except `timestamp`, which the method takes as `now: i64` |
| `count_pick(command_id: i64)` | E2 | `()` |
| `delete_command(command_id: i64, text: &str, now: i64)` | D1-D5 | `()` |
| `purge()` | P1 | `PurgeCounts { commands: u64, runs: u64 }` (counted before deleting) |
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
pub(super) fn find_repo_root(cwd: &Path) -> Option<PathBuf>
```

Return `None` unless `cwd.is_absolute()`. For `dir` in `cwd.ancestors()`, return
`Some(dir.to_path_buf())` at the first `dir.join(".git").exists()` (directory or
file, so worktrees and submodules count). No canonicalisation, no cache
(precedent: `IgnoreChecker::repo_of`, [src/import/mod.rs:338-349](../../src/import/mod.rs),
which stays private). Called once per `track-command`, once per picker start,
once per imported command with a known cwd. Never per keystroke.

## 9. `track-command`

`src/cmd/track.rs`, `pub(super) fn run(data_dir: Option<&Path>, args: &TrackCommandArgs, now: i64, env_ignore: Option<&str>) -> anyhow::Result<()>`;
`cmd::run_track_command` passes `jiff::Timestamp::now().as_second()` and
`std::env::var("PSYCHIC_HISTORY_IGNORE").ok()`.

| Step | Action | On "skip" |
|---|---|---|
| 1 | `data_dir` is `None` -> `bail!("No data directory: pass --data-dir, or set HOME")` | - |
| 2 | `text = args.command.to_string_lossy()`, then `trim()` | empty -> return `Ok(())` |
| 3 | `text.len() > CMD_MAX_TEXT_BYTES` | return `Ok(())` |
| 4 | `template::first_command_word(text)` in `CMD_OWN_COMMANDS` | return `Ok(())` |
| 5 | `privacy::is_ignored(text, env_ignore)` (section 10.3) | return `Ok(())` |
| 6 | `started_at = args.started_at.unwrap_or(now - args.duration_ms.unwrap_or(0).max(0) / 1000)`; `cwd = Some(args.cwd)` unless empty; `repo_root = cwd.and_then(find_repo_root)`; `template = templatize(text)` | - |
| 7 | `CmdStore::open(data_dir)?` (creates the data dir if missing) `.record_run(&NewRun { source: Source::Hook, .. })` (section 6.1). A negative `--duration-ms` is passed as `None` | - |

It does not check the opt-in marker: being called is the hook's decision
(section 10.1). `main` returns from M1 before its own `create_dir_all`
([src/main.rs:233](../../src/main.rs)), which is why `CmdStore::open` creates the directory. It writes nothing to stdout or stderr on success. Errors
propagate through `main` as `Err`; no error message contains the command text
(section 10.6). `log::` macros reached inside `Database::new` are no-ops because
no logger is installed (section 2, M1).

Budget: B4. Cost model: process start + clap parse, one `Database::new`
(first-open schema work is about 0.7 ms, [src/db.rs:151-153](../../src/db.rs)),
at most 7 `stat` calls for permissions, one `.git` walk, one 5-statement
transaction with `synchronous = NORMAL` under WAL (no fsync per commit).

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
| `HISTCONTROL` (`ignorespace`, `ignoreboth`, `ignoredups`, `erasedups`) and `HISTIGNORE`, bash | by construction | The hook records only what bash itself added to its history list (section 11.3) |
| Leading-space commands and private mode, fish | hook | `string match -q -- ' *'`, `set -q fish_private_mode` |
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
`events.db` is. Upstream sets no mode anywhere (`Database::new`,
[src/db.rs:162-200](../../src/db.rs); `create_dir_all`, [src/main.rs:233](../../src/main.rs));
those call sites are not changed, so a user who never enables history keeps
today's modes.

### 10.5 Delete and purge

| Operation | Trigger | Effect |
|---|---|---|
| Delete one command | Picker key ([02 section 9](02-browse.md#9-keys)) | `CmdStore::delete_command`: D1-D5 in one transaction. The tombstone `fnv1a64(text)` stops a later import restoring it. Running the command again records it again |
| Purge | `psychic history --purge` | Without `--yes`: print `Delete all recorded commands (N commands, M runs)? [y/N] ` and read one line from stdin; anything but `y`/`Y` prints `Nothing deleted.` and returns. Then P1, remove `cmd_model.txt` and `cmd_model_stats.json` if present, print `Deleted N commands and M runs. Recording is still on|off.` The marker file is not touched |

```rust
pub(super) fn fnv1a64(text: &str) -> i64  // offset 0xcbf29ce484222325, prime 0x100000001b3, over UTF-8 bytes, `as i64`
```

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

# Before a command runs: remember it, where it was typed and when.
function __psychic_preexec() {
    __psychic_cmd=''
    __psychic_history_enabled || \builtin return 0
    [[ -n "$1" ]] || \builtin return 0
    [[ -o histignorespace && "$1" == ' '* ]] && \builtin return 0
    [[ -n "${HISTORY_IGNORE-}" && "$1" == ${~HISTORY_IGNORE} ]] && \builtin return 0
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
reset the `histignorespace` option being tested.

### 11.3 bash (append to `shell/psychic.bash`)

Decision (ADR-5): no DEBUG trap and no `PS0`. The hook runs from
`PROMPT_COMMAND` and reads the newest entry of bash's own history list.

```bash
# =============================================================================
# Command history: record each command for `psychic history` (Ctrl-R)
# =============================================================================

# Off until `psychic history --enable`. PSYCHIC_HISTORY=0 or 1 overrides.
__psychic_session="$$-${RANDOM}${RANDOM}"
__psychic_hist_primed=''
__psychic_hist_seen=''
__psychic_cmd_pwd="${PWD}"
__psychic_hist_re='^[[:space:]]*([0-9]+)[*]?[[:space:]]+([0-9]+)[[:space:]](.*)$'

__psychic_history_enabled() {
    case "${PSYCHIC_HISTORY-}" in
        0) \builtin return 1 ;;
        1) \builtin return 0 ;;
    esac
    [[ -e "${PSYCHIC_DATA_DIR:-${HOME}/.local/share/psychic}/history-enabled" ]]
}

# Before each prompt: if bash added a new entry to its history, record it.
__psychic_history_hook() {
    \builtin local __psychic_status=$?
    \builtin local __psychic_pwd="${__psychic_cmd_pwd}"
    __psychic_cmd_pwd="${PWD}"
    __psychic_history_enabled || \builtin return "${__psychic_status}"

    \builtin local __psychic_entry __psychic_key=''
    __psychic_entry="$(HISTTIMEFORMAT='%s ' \builtin history 1)"
    if [[ "${__psychic_entry}" =~ ${__psychic_hist_re} ]]; then
        __psychic_key="${BASH_REMATCH[1]} ${BASH_REMATCH[2]} ${BASH_REMATCH[3]}"
    fi
    if [[ -z "${__psychic_hist_primed}" ]]; then
        __psychic_hist_primed=1
        __psychic_hist_seen="${__psychic_key}"
        \builtin return "${__psychic_status}"
    fi
    if [[ -z "${__psychic_key}" || "${__psychic_key}" == "${__psychic_hist_seen}" ]]; then
        \builtin return "${__psychic_status}"
    fi
    __psychic_hist_seen="${__psychic_key}"

    \builtin local __psychic_start="${BASH_REMATCH[2]}" __psychic_text="${BASH_REMATCH[3]}" __psychic_now=''
    \builtin printf -v __psychic_now '%(%s)T' -1 2>/dev/null || __psychic_now=''
    # In a subshell, so the shell prints no job number or "Done" line.
    (\command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        track-command --cwd "${__psychic_pwd}" --exit-code "${__psychic_status}" \
        --started-at "${__psychic_start}" \
        ${__psychic_now:+--duration-ms} ${__psychic_now:+"$(( (__psychic_now - __psychic_start) * 1000 ))"} \
        --session "${__psychic_session}" -- "${__psychic_text}" >/dev/null 2>&1 &)
    \builtin return "${__psychic_status}"
}

if [[ ";${PROMPT_COMMAND:-};" != *";__psychic_history_hook;"* ]]; then
    PROMPT_COMMAND="__psychic_history_hook${PROMPT_COMMAND:+;${PROMPT_COMMAND}}"
fi
```

| Property | How it is obtained |
|---|---|
| Command text | Group 3 of `history 1`; multi-line entries arrive whole (POSIX `.` matches a newline) |
| Start time | Group 2: the history entry's own timestamp, forced to epoch seconds by a one-command `HISTTIMEFORMAT='%s '` |
| "A new command ran" | The key (history number, timestamp, text) differs from the last one seen (the text is in the key because `erasedups` can leave number and second unchanged). The first invocation only primes the key, so the entry loaded from `HISTFILE` at shell start is never recorded |
| cwd the command was typed in | `$PWD` saved at the end of the previous invocation |
| Exit status | `$?` on entry to the function |
| Duration | Whole seconds x 1000 (bash >= 4.2 for `printf '%(%s)T'`; omitted on 4.0/4.1) |
| `HISTCONTROL`, `HISTIGNORE`, `set +o history` | A command bash did not add to its history produces no new key, so it is not recorded. No pattern is re-implemented |
| Coexistence | The function is prepended to `PROMPT_COMMAND` (string or element 0 of the array form) and returns the status it received, so later entries (Starship, direnv, `__psychic_hook`) see the same `$?`. No trap and no `PS0` are touched |
| Cost when enabled | One `$(...)` fork of the shell per prompt (no exec) plus the background binary |

Known limitation, stated in `docs/how-it-works.md`: with `ignoredups` or
`ignoreboth` an immediate repeat of the identical command is not added to
bash's history and therefore not recorded. Two identical commands entered within the same second under `erasedups`
are recorded once. If another tool is prepended to `PROMPT_COMMAND` after
psychic and does not preserve `$?`, the recorded exit code is that tool's;
loading psychic last in `~/.bashrc` avoids it.

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
    string match -q -- ' *' "$argv[1]"; and return
    set -l __psychic_bin psychic
    set -q PSYCHIC_BIN; and set __psychic_bin $PSYCHIC_BIN
    set -l __psychic_dir
    set -q PSYCHIC_DATA_DIR; and set __psychic_dir --data-dir $PSYCHIC_DATA_DIR
    command $__psychic_bin $__psychic_dir track-command --cwd "$__psychic_cmd_pwd" \
        --exit-code $__psychic_status --duration-ms "$CMD_DURATION" \
        --session "$__psychic_session" -- "$argv[1]" >/dev/null 2>&1 &
    disown 2>/dev/null
end
```

No `--started-at`: `track.rs` step 6 derives it from `--duration-ms`
(`$CMD_DURATION` is milliseconds). `$status` is read on the first line, before
anything can overwrite it.

## 12. Backfill

`src/cmd/backfill.rs`:
`pub(super) fn run(store: &CmdStore, home: &Path, now: i64, env_ignore: Option<&str>) -> anyhow::Result<BackfillReport>`.
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
| [src/import/shell_history.rs:372-383](../../src/import/shell_history.rs) | Extract into `fn command_words(words: Vec<Option<String>>) -> Option<(String, Vec<Option<String>>)>` (skips `NAME=value`, `builtin`, `command`; returns name and arguments) |
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

1. `commands = parse_zsh|parse_bash|parse_fish(bytes)`;
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
   `repo_root = cwd.and_then(find_repo_root)`, `shell_session =
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
`BackfillReport.atuin = Skipped("unexpected schema")`.

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

`BackfillReport { sources: Vec<SourceReport { key, found, inserted, skipped }> }`
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
  Not recorded: commands your shell keeps out of its own history (leading space,
    HISTIGNORE / HISTORY_IGNORE), anything matching $PSYCHIC_HISTORY_IGNORE, psychic's own commands.
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
| TC-R-07 | `find_repo_root` | temp tree `r/.git/`, `r/a/b/`; inputs `r/a/b`, `r`, temp root, `relative/x` | `Some(r)`, `Some(r)`, `None`, `None` | `.git` as a file (`r2/.git` file) -> `Some(r2)` |
| TC-R-08 | `record_run` aggregates | same text 3 times: exit 0 at t=100, exit 1 at t=200, exit NULL at t=150 | `run_count 3, success 1, fail 1, first_run_at 100, last_run_at 200, last_exit_code 1` | out-of-order arrival does not overwrite `last_exit_code` |
| TC-R-09 | `record_run` predecessor | session `s`: A at 100, B at 160, C at 160+1801; session `t`: D at 170 | B.prev = A, C.prev = NULL (gap), D.prev = NULL (other session) | `CMD_SEQUENCE_MAX_GAP_SECS` boundary: gap exactly 1800 -> linked |
| TC-R-10 | import uniqueness | `record_run` Import twice with same text and `started_at` | second returns `Duplicate`; `run_count == 1` | Hook rows with the same pair are both inserted |
| TC-R-11 | every plan | `EXPLAIN QUERY PLAN` of S2, S3, R2-R8, R9, R10, D2-D4, I1 with a `plan()` helper shaped like [src/db.rs:1424-1433](../../src/db.rs) | Each contains the string in its "Expected plan contains" cell; R5 does not contain `TEMP B-TREE` | R6 with 1, 2 and 3 placeholders |
| TC-R-12 | `create_schema` idempotence | call 3 times on `:memory:` | no error; `SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'idx_command%'` = 11 | - |
| TC-R-13 | `assign_times` | timestamps `[None, Some(50), None, None]`, now 100, ceiling 100 | `at = [49, 50, 98, 99]`, `synthetic = [true, false, true, true]` | `[Some(500)]` -> `at = [99]`, synthetic (future timestamp); `[Some(30), Some(20), Some(10), Some(5)]` -> all kept, none synthetic (out-of-order file); ceiling 60 with `[None]` -> `at = [59]` |
| TC-R-14 | `replay_cwds` | home `/h`, dirs `/h/a`, `/h/a/b`; commands `cd a`, `ls`, `cd b && make`, `z foo`, `ls`, `cd /h/a`, `pwd` | `[/h, /h/a, /h/a, /h/a/b, None, None, /h/a]` | the command's own `cd` does not apply to itself; lost and regained cwd |
| TC-R-15 | `track::run` skips | texts `"   "`, 8193 x `a`, `p`, `pd foo`, `psychic history`, `export TOKEN=1` with ignore `export *` | 0 rows in `commands` after each | - |
| TC-R-16 | `track::run` derives the start | now 1000, `--duration-ms 2500`, no `--started-at` | `started_at == 998` | `--started-at 990` wins; no duration -> 1000 |
| TC-R-17 | `tighten_permissions` | temp dir 0755 with `events.db` 0644 | dir `0o700`, file `0o600`; a second call performs no `set_permissions` (modes already right) | missing `-wal`/`-shm` are not an error |
| TC-R-18 | `delete_command` | A (3 runs, 2 events), B with `prev = A` | A gone from all three tables; B.prev NULL; `deleted_commands` has `fnv1a64(A.text)` | - |
| TC-R-19 | clap surface | `["psychic","track-command","--cwd","/x","--session","s","--","-rf --weird\nline"]` | `command == "-rf --weird\nline"` | missing `--` -> parse error; `history --enable --purge` -> parse error (group) |

### 15.2 Integration tests

| ID | Flow | Setup | Verification | Teardown |
|---|---|---|---|---|
| IT-R-01 | Older database gains the command tables (shape of [src/db.rs:1053](../../src/db.rs)) | Temp dir; create `events.db` by hand as an older database holding only an `events` and a `sessions` table (current upstream column lists), one row each | `Database::new` succeeds; the old rows are intact; `commands`, `command_runs`, `command_events`, `command_imports`, `deleted_commands` exist | `remove_dir_all` |
| IT-R-02 | Backfill is idempotent | Temp home with `.zsh_history` (extended format, 4 commands: `p`, and three different commands of which two share one second) and `.bash_history` (3 lines, no timestamps, mtime set to `M`); temp data dir | First `backfill::run`: zsh 3 inserted, bash 3 inserted with `shell_session LIKE 'import-synth:bash:%'` and `started_at` = M-2, M-1, M. Second run: 0 inserted, row counts unchanged | `remove_dir_all` |
| IT-R-03 | Import stops at the first hook run | As IT-R-02 plus one `Source::Hook` run at t=1,700,000,050 before importing a zsh file with entries at ...000 and ...100 | Only the ...000 entry is imported | `remove_dir_all` |
| IT-R-04 | Atuin import | Build a temp `history.db` with the A1-A4 schema and 4 rows with nanosecond timestamps: (exit 0, cwd `/x`), (exit 1, cwd `unknown`), (exit -1, cwd `/y`), and a fourth with `deleted_at` set | 3 rows imported; `started_at` in seconds; `exit_code` 0, 1, NULL; `cwd` `/x`, NULL, `/y`; `duration_ms` NULL where Atuin's is -1; the Atuin file's mtime and size are unchanged | `remove_dir_all` |
| IT-R-05 | Tombstone blocks re-import | Import, `delete_command` one, import again | The deleted text is absent after the second import | `remove_dir_all` |
| IT-R-06 | Enable, disable, purge | Temp data dir; `--enable --no-import`, record 2 runs, `--status`, `--purge --yes`, `--disable` | Marker exists then not; modes 700/600; status lines as section 13.3; after purge all five tables are empty and the marker is still present | `remove_dir_all` |
| IT-R-07 | No command text leaks | Temp data dir; run `track::run` and `backfill::run` with texts containing `SENTINEL_c0ffee` | No file in the data dir other than `events.db*` contains the bytes `SENTINEL_c0ffee` | `remove_dir_all` |
| IT-S-zsh, IT-S-bash, IT-S-fish | Real shell, real hook (`tests/shell_hooks.rs`) | Section 15.3 | Section 15.3 | `remove_dir_all` |

### 15.3 Shell-level tests (`tests/shell_hooks.rs`)

One `#[test]` per shell. Binary: `env!("CARGO_BIN_EXE_psychic")`.

| Step | Detail |
|---|---|
| Skip rule | If `Command::new(<shell>).arg("--version")` fails to spawn: `println!("skipped: <shell> not installed")` and return (pass) |
| Scratch | `T = temp_dir()/psychic-shell-<shell>-<pid>`; `T/home`, `T/data`, `T/work/sub`; `T/hook` = stdout of `psychic <shell>` |
| Environment (cleared, then) | `HOME=T/home`, `PATH` inherited, `TERM=dumb`, `PSYCHIC_BIN=<binary>`, `PSYCHIC_DATA_DIR=T/data`, `PSYCHIC_HISTORY=1`, `PSYCHIC_NO_RETRAIN=1`, `HISTFILE=T/home/hist`; bash also `HISTCONTROL=ignorespace`, `HISTIGNORE='secret*'`; zsh also `ZDOTDIR=T/home` |
| Launch | On a pty (`libc::openpty`; child via `std::process::Command` with the slave as stdin/stdout/stderr and `pre_exec` doing `setsid()` + `ioctl(0, TIOCSCTTY, 0)`; window 24x80). zsh: `zsh -f -i`; bash: `bash --norc --noprofile -i`; fish: `fish --no-config -i -C 'source T/hook'` (the hook is loaded before the first prompt, because `fish_postexec` would otherwise record the `source` line itself). cwd `T/work` |
| Drive | A reader thread drains the master. Write each line + `\n`, sleeping 150 ms between lines |
| Lines | (zsh only) `setopt histignorespace`, `HISTORY_IGNORE='secret*'`; then for all: (zsh, bash) `source T/hook`; then `true`, `false`, `cd sub`, `echo in-sub`, ` echo leading-space`, (zsh, bash) `secret-thing`, `echo 'multi` / `line'`; then the single byte `0x04` (Ctrl-D on an empty line ends all three shells without a command that could itself be recorded) |
| Wait | Child exit within 30 s (else kill and fail). Then poll `SELECT COUNT(*) FROM command_runs` every 50 ms for up to 5 s until it equals the expected count |
| Expected rows, ordered by `started_at, id` (text / cwd / exit_code / source) | `true` / `T/work` / 0 / hook; `false` / `T/work` / 1 / hook; `cd sub` / `T/work` / 0 / hook; `echo in-sub` / `T/work/sub` / 0 / hook; the multi-line echo: text starts with `echo 'multi`, contains a newline or `;`, ends with `line'` / `T/work/sub` / 0 / hook. Exactly 5 rows; no text contains `leading-space` or `secret`; all 5 share one `shell_session`; `commands.template` of `cd sub` is `cd PATH` |

The pty helper (about 60 lines) lives in `tests/support/mod.rs`, declared with
`mod support;` in `tests/shell_hooks.rs`; Phase B's `tests/picker.rs` uses the
same module. It is not shared with `src/`.

## 16. Error handling matrix

| # | Failure | Detection | Response | Fallback | Logging |
|---|---|---|---|---|---|
| E-R-01 | `psychic` not on PATH in a hook | Shell cannot exec | Nothing: background job fails, output discarded | Command not recorded | None |
| E-R-02 | No data dir (`HOME` unset, no `--data-dir`) | `data_dir.is_none()` | `Err("No data directory: pass --data-dir, or set HOME")`, exit 1 | Not recorded | None (stderr discarded by the hook) |
| E-R-03 | Database locked more than 5 s | `SQLITE_BUSY` after `busy_timeout = 5000` ([src/db.rs:209](../../src/db.rs)) | `Err("recording a command failed: database busy")`, exit 1 | That run is lost; no retry | None |
| E-R-04 | Disk full / read-only data dir | rusqlite or io error | `Err` naming the operation and path, exit 1 | Not recorded | None |
| E-R-05 | Clap rejects the hook's arguments (non-numeric `--exit-code`, missing `--`) | clap error | clap's message on stderr (discarded), exit 2 | Not recorded | None |
| E-R-06 | Command text is not UTF-8 | `to_string_lossy` replaces bytes | Recorded with U+FFFD | - | None |
| E-R-07 | Command over `CMD_MAX_TEXT_BYTES`, empty, own, or ignored | Section 9 steps 2-5 | `Ok(())`, exit 0 | Not recorded by design | None |
| E-R-08 | `chmod` fails (file owned by another user) | `tighten_permissions` returns `Err` | `CmdStore::open` returns `Err("could not make <path> private")` | `track-command`: not recorded. `--enable`: prints the error, marker not written | Picker/actions: `log::error!` with the path |
| E-R-09 | Marker file cannot be written | io error in `--enable` | `Err("could not write <marker path>")`, exit 1 | Recording stays off | stderr via `main` |
| E-R-10 | History file missing or unreadable | `fs::read` error | Source reports `found 0` | Other sources continue | `log::info!("import: <kind> history not readable")` |
| E-R-11 | History file is not valid UTF-8 | - | `from_utf8_lossy` (upstream parsers, [src/import/shell_history.rs:93](../../src/import/shell_history.rs)) | - | None |
| E-R-12 | Atuin DB missing | `!path.exists()` | Source skipped, `found 0` | - | None |
| E-R-13 | Atuin DB locked or unexpected schema | open/query error, or `pragma_table_info` check fails | `Skipped("locked")` / `Skipped("unexpected schema")` in the report; summary prints `atuin skipped (<reason>)` | Shell files still import | `log::warn!` with the reason, no row data |
| E-R-14 | Import transaction fails mid-file | rusqlite error | Roll back that file's transaction; `command_imports` not updated; continue with the next source | A later `--import` retries the file (synthetic entries included, since I2 has no row) | `log::error!("import of <source_key> failed: <sqlite code>")` |
| E-R-15 | `--purge` without `--yes` and stdin is not a terminal | `!std::io::stdin().is_terminal()` | `bail!("stdin is not a terminal, so psychic can't ask before deleting; pass --yes")` (wording as [src/shell_init.rs:269-271](../../src/shell_init.rs)) | Nothing deleted | stderr via `main` |
| E-R-16 | zsh without `zsh/datetime` | `EPOCHREALTIME` empty | Hook omits `--started-at` and `--duration-ms` | `track.rs` uses `now`; duration NULL | None |
| E-R-17 | bash older than 4.2 | `printf '%(%s)T'` fails | Hook omits `--duration-ms` | duration NULL | None |
| E-R-18 | bash history disabled or command suppressed by `HISTCONTROL`/`HISTIGNORE` | No new history key | Not recorded | By design | None |
| E-R-19 | Two `track-command` processes at once | SQLite write lock | Second waits on `busy_timeout` | - | None |
| E-R-20 | Clock goes backwards between runs | `started_at` smaller than the session's last | S3 (`started_at <= ?2`) finds an older predecessor or none; S5 keeps `last_run_at` as the max | - | None |
| E-R-21 | A shell needed by a shell test is not installed | `--version` spawn fails | Test prints `skipped: ...` and passes | - | test stdout |

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
