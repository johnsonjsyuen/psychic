# Phase B: Browse (Implementation)

Document type: **Implementation** (HOW). Strategy and targets:
[00-blueprint.md](00-blueprint.md). This document is the single source for the
`psychic history` picker: invocation, the in-memory `CommandIndex`, the worker,
heuristic ranking, the UI, keys, event logging and the Ctrl-R shell widgets.

## 1. Scope, inputs, done when

| | |
|---|---|
| Builds | The picker with heuristic ranking only, scope filter, preview, debug pane, help, delete, impression/pick logging, Ctrl-R widgets |
| Inputs (must exist) | Phase A: tables and `CmdStore` ([01 section 5](01-record.md#5-schema), [section 6](01-record.md#6-sql-statements)), `template.rs`, `repo.rs`, `privacy.rs`, `HistoryArgs` ([01 section 4.2](01-record.md#42-psychic-history)) |
| Leaves for Phase C | `CmdRow.features` is empty, `ml_score` is `None`, `ml_weight` is `0.0`; the debug pane prints `model: none` |

Done when all of these pass:

```bash
just build && cargo test && cargo clippy          # clean
cargo test cmd::                                  # unit tests of src/cmd/
cargo test --test picker -- --nocapture           # pty-driven picker tests (section 14.2)
cargo test --test shell_hooks -- --nocapture      # Phase A tests still pass with the widget blocks appended
./bench/run.py setup HEAD~1 && ./bench/run.py startup 6 && ./bench/run.py keystroke 30   # file mode unchanged; table in the commit message
```

The last line is for a human: agents never run `bench/run.py` (it writes to
the real data directory). The Phase B commit carries instead a rough pty
measurement of the picker on the perf DB (section 12, "Measured at Phase B").

Metrics B1-B3 and B5 ([00 section 2](00-blueprint.md#2-success-metrics)) are
first measured in Phase D ([04 section 6](04-bench.md#6-performance-benchmarks));
this phase is built to the design in section 12 so that they pass there.

## 2. Files touched

New files: `src/cmd/{index,rank,worker,fuzzy,events,keys,app,render}.rs`,
`tests/picker.rs`. Extended: `src/cmd/mod.rs` (the no-action branch of
`run_history`), `src/cmd/tuning.rs` (section 3), `src/cmd/store.rs` (R1 split
by id and the start-up reader methods, [01 section 6.5](01-record.md#65-cmdstore-methods-and-row-types)),
`tests/support/mod.rs` (the scratch home and shell set-up that
`tests/shell_hooks.rs` had, so `tests/picker.rs` drives the same shells;
`Pty::spawn_with_stdout`, `wait_quiet`, `wait_for`).

| Upstream file | Exact edit |
|---|---|
| `shell/psychic.zsh`, `shell/psychic.bash`, `shell/psychic.fish` | Append the widget blocks of section 11 after the Phase A recording blocks |
| `docs/how-it-works.md` | Extend the "Shell history search" section with the picker |

Reused as they are, with no edit:

| Item | Location | Used for |
|---|---|---|
| `tty_input::attach_stdin_to_terminal`, `tty_input::spawn`, `TtyInput::shutdown` | [src/tty_input.rs:267](../../src/tty_input.rs), [:187](../../src/tty_input.rs), [:159](../../src/tty_input.rs) | Terminal attach, input thread |
| `input::leave_tui` | [src/input.rs:706-716](../../src/input.rs) | Restoring the terminal |
| `path_display::{printable, truncate_to_width, display_width, get_time_ago}` | [src/path_display.rs:59](../../src/path_display.rs), [:21](../../src/path_display.rs), [:12](../../src/path_display.rs), [:263](../../src/path_display.rs) | Row and preview text |
| `keymap::{Chord, Trigger, describe_trigger}` | [src/keymap.rs:83](../../src/keymap.rs), [:91](../../src/keymap.rs), [:513](../../src/keymap.rs) | Key table types and key names |
| `fuzzy_matcher::skim::SkimMatcherV2` | as [src/search_worker.rs:808](../../src/search_worker.rs) | Filter |
| `crate::PROCESS_START` | [src/main.rs:99](../../src/main.rs) | Zero point of every startup timing (`crate::PROCESS_START.elapsed()`) |
| The fern logger installed by `main` | [src/main.rs:229-265](../../src/main.rs) | `app.log`, TIMING lines |

## 3. Constants (Phase B)

Appended to `src/cmd/tuning.rs`. The heuristic constants are starting values;
they are the first tuning candidates of [04 section 9](04-bench.md#9-tuning-candidates-in-priority-order).

| Constant | Type | Value | Meaning |
|---|---|---|---|
| `CMD_H_W_RECENCY` | `f64` | `1.0` | Heuristic weight of recency |
| `CMD_H_W_DIR` | `f64` | `0.5` | Heuristic weight of "run in this directory" |
| `CMD_H_W_REPO` | `f64` | `0.2` | Heuristic weight of "run in this repository" |
| `CMD_H_W_FUZZY` | `f64` | `0.6` | Heuristic weight of the normalised fuzzy score |
| `CMD_H_RECENCY_SCALE_SECS` | `f64` | `3600.0` | Age at which the recency term is 0.5 |
| `CMD_H_DIR_SATURATION` | `f64` | `10.0` | Runs in a directory at which the directory term stops growing |
| `CMD_LOAD_READERS` | `usize` | `4` | Connections that read R1 at picker start, each a slice of the id range (section 5.2) |
| `CMD_PAR_FILTER_MIN` | `usize` | `2000` | Base-set size from which the filter (with a non-empty query) and the heuristic use rayon |
| `CMD_PAGE_ROWS` | `usize` | `200` | Rows per `QueryUpdated` / `Rows` response |
| `CMD_PAGE_PREFETCH` | `usize` | `50` | Request the next page when the selection is this close to the last loaded row |
| `CMD_IMPRESSION_AGE_MS` | `u64` | `200` | A query on screen this long counts as seen (same value as [src/analytics.rs:22](../../src/analytics.rs)) |
| `CMD_TICK_MS` | `u64` | `1000` | Tick thread sleep; each tick wakes the event loop once (file mode's idle redraw rate, [src/main.rs:840-848](../../src/main.rs)) |
| `CMD_STACK_BELOW_COLS` | `u16` | `120` | Narrower terminals stack list over preview (as [src/render.rs:401](../../src/render.rs)) |

## 4. Picker invocation

`psychic [--data-dir DIR] history [--query Q] [--session ID] [--cwd DIR]` with
no action flag runs `cmd::app::run`.

| Input | Resolution |
|---|---|
| `cwd` | `--cwd` if given; else `$PWD` if set and absolute; else `std::env::current_dir()` as a string. Never canonicalised (ADR-7). Resolved once |
| `repo_root` | `repo::find_repo_root(cwd, $HOME)`, once |
| `shell_session` | `--session`; absent means no session scope and no sequence context |
| `now` | `jiff::Timestamp::now().as_second()`, once; all ranking in this process uses it |
| `tui_session_id` | `rand::random::<u64>().to_string()` (as [src/main.rs:205-207](../../src/main.rs)) |
| `home` | `$HOME`, read once, for `~` abbreviation in the preview |

| Outcome | stdout | Exit code |
|---|---|---|
| Enter on a row | The command text followed by `\n` (`println!`), written after the terminal is restored | 0 |
| Esc / Ctrl-C / Ctrl-D, or Enter with an empty list | nothing | 0 |
| Start-up failure (no tty, database unreadable) | nothing; the error on stderr through `main` | 1 |

A multi-line command is printed with its newlines; the widgets of section 11
keep them. The picker opens even when no command is recorded; section 8.3 says
what it shows then.

Start-up order in `cmd::app::run` (the worker is spawned first so the index
loads while the terminal is being set up):

1. Resolve the table above. Create `mpsc::channel::<CmdEvent>()`.
2. `worker::spawn(data_dir, LoadParams { now, cwd, repo_root, shell_session }, event_tx.clone())`.
3. Send `CmdRequest::UpdateQuery { query: args.query, query_id: 1, scope: Scope::All }`.
4. `tty_input::attach_stdin_to_terminal()`; `tty_input::spawn(event_tx.clone())`.
5. Open `/dev/tty` read+write, `enable_raw_mode`, `EnterAlternateScreen`,
   `EnableMouseCapture`, `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`,
   `Terminal::new(CrosstermBackend::new(tty))` - the sequence of
   [src/main.rs:765-781](../../src/main.rs), so `input::leave_tui` undoes it exactly.
6. Open the UI thread's own `CmdStore` for event logging; spawn the tick thread.
7. Event loop (section 8.2).
8. On exit: `input.shutdown()`, `input::leave_tui(&mut terminal)`, print the
   pick if there is one (and flush); then send `CmdRequest::Shutdown` and join
   the worker only if it has already finished. The worker may be busy (a load,
   a `Delete` waiting on another process's lock for up to `busy_timeout`); the
   terminal and the shell must not wait for it, and the process exits next.
   (The error paths of steps 4-5 still stop and join it.)

## 5. `CommandIndex`

`src/cmd/index.rs`. All ranking statistics in memory. It has two fill paths and
one read path (ADR-12): `load` (SQL aggregates, this phase) and the mutators
(replay, [03 section 5](03-learn.md#5-training-set-replay)). Heuristic and
features read only through the accessors below.

```rust
pub(super) struct Interner { /* FxHashMap<Box<str>, u32> + Vec<Box<str>> */ }
impl Interner { pub fn intern(&mut self, s: &str) -> u32; pub fn get(&self, s: &str) -> Option<u32>; pub fn name(&self, id: u32) -> &str; }

pub(super) struct CommandEntry {
    pub db_id: i64,
    pub text: std::sync::Arc<str>,
    pub text_lower: Box<str>,          // text.to_lowercase(), computed once
    pub chars: u32,                    // text.chars().count()
    pub words: u32,                    // template::word_count(text)
    pub classes: u64,                  // fuzzy::text_classes(text): one bit per byte class present (section 6.3)
    pub last_run_at: i64,
    pub run_count: u32,
    pub success_count: u32,
    pub fail_count: u32,
    pub pick_count: u32,               // all-time Ctrl-R picks (R1); display only, no feature reads it
    pub last_exit_code: Option<i32>,
    pub last_fail_at: i64,             // newest run with a non-zero exit code; 0 = none known
    pub run_times: Vec<i64>,           // ascending; every run newer than CMD_EVENT_WINDOW_SECS is present
    pub deleted: bool,
}
pub(super) struct DirStat { pub runs: u32, pub last_run_at: i64 }

pub(super) struct CommandIndex {
    pub entries: Vec<CommandEntry>,                         // position = "idx" (u32) used everywhere below
    filter_classes: Vec<u64>,                               // per idx: entry.classes | fuzzy::LIVE, LIVE cleared by remove; read by the filter only
    by_db_id: FxHashMap<i64, u32>,
    pub dirs: Interner, pub repos: Interner, pub templates: Interner,
    dir_stats: FxHashMap<(u32, u32), DirStat>,              // (idx, dir id)
    repo_runs: FxHashMap<(u32, u32), u32>,                  // (idx, repo id)
    followers: FxHashMap<(u32, u32), u32>,                  // (idx, template id of the previous command) -> runs
    picks: FxHashMap<u32, Vec<i64>>,                        // idx -> pick times, ascending
    picks_by_query: FxHashMap<Box<str>, FxHashMap<u32, Vec<i64>>>,
    episode_picks_by_query: FxHashMap<Box<str>, FxHashMap<u32, Vec<i64>>>,
    pub session_commands: FxHashSet<u32>,                   // serving only: commands run in --session
}

/// The moment and place being ranked for.
pub(super) struct RankContext {
    pub now: i64,
    pub dir: Option<u32>,                 // id in `dirs` of the cwd; None = unknown
    pub repo: Option<u32>,                // id in `repos` of the repo root; None = not in a repository
    pub prev_templates: [Option<u32>; 3], // [0] = template id of the most recent command in the shell session
}
```

### 5.1 Mutators (the only writers)

| Method | Effect |
|---|---|
| `add_command(db_id, text) -> u32` | Push an entry with zero counts; fill `text_lower`, `chars`, `words`; register in `by_db_id`. Asserts `db_id` is new |
| `record_run(idx, at, dir: Option<u32>, repo: Option<u32>, exit: Option<i32>, prev_template: Option<u32>)` | `run_count += 1`; `success_count`/`fail_count` by `exit`; if `at >= last_run_at` then `last_exit_code = exit`; `last_run_at = max`; non-zero `exit` -> `last_fail_at = max(last_fail_at, at)`; insert `at` into `run_times` at `partition_point(\|&t\| t <= at)`; for a `Some` dir: `dir_stats[(idx, dir)].runs += 1` and its `last_run_at = max(last_run_at, at)`; for a `Some` repo: `repo_runs += 1`; for a `Some` prev_template: `followers += 1` |
| `record_pick(idx, at, query: &str, episode_queries: &[String])` | Push `at` to `picks[idx]`, to `picks_by_query[query][idx]`, and to `episode_picks_by_query[q][idx]` for each distinct `q` in `episode_queries` |
| `remove(idx)` | `entries[idx].deleted = true`; clear `fuzzy::LIVE` in `filter_classes[idx]` |

### 5.2 `load` (picker start)

```rust
pub(super) struct LoadParams { pub now: i64, pub cwd: Option<String>, pub repo_root: Option<String>, pub shell_session: Option<String> }
impl CommandIndex { pub(super) fn load(store: &CmdStore, p: &LoadParams) -> anyhow::Result<(CommandIndex, RankContext)>; }
```

Runs R1-R8 of [01 section 6.2](01-record.md#62-read-path-at-picker-start-r-each-run-exactly-once-per-process),
each once, and fills the fields directly. The reads run side by side, because
one connection reading them in order took 40-50 ms of the 50 ms B3 budget at
50,000 commands (R1 alone 25-30 ms, R6 several):

| Thread | Connection | Reads |
|---|---|---|
| the worker | `store` | R1 ids, then R1 over the first of `CMD_LOAD_READERS` equal slices of that id range, building its `CommandEntry`s as rows arrive |
| `CMD_LOAD_READERS - 1` scoped threads | one `CmdStore::open_file` each | R1 over one more slice each, building its entries |
| one scoped thread | `CmdStore::open_file` | R2-R8, in order, into plain row vectors |

Every one of these connections first sets `map_for_reading` (`PRAGMA
mmap_size`): R1 then reads pages from the page cache without copying them,
about 6 of 17 ms cold. The entries are pushed in id order, so `idx` order is
the same as one full scan's; then the R2-R8 rows are applied. An in-memory
store (tests), which another connection would see empty, reads everything on
`store`, in order. Rows of R2-R8 whose command is missing from R1 (recorded
or deleted between the reads) are skipped. The worker starts the rayon pool
and warms the matcher (section 6.2, step 2) on another thread while the load
runs.

| Query | Fills |
|---|---|
| R1 | One entry per row, built from the borrowed row on the reader's thread: `db_id`, `text`, `last_run_at`, the four counts, `last_exit_code`; `run_times` empty, `last_fail_at = 0` |
| R2 | `ctx.dir = Some(dirs.intern(cwd))`; `dir_stats[(idx, dir)] = DirStat { runs, last_run_at }` |
| R3 | `ctx.repo = Some(repos.intern(repo_root))`; `repo_runs[(idx, repo)] = count` |
| R4 | Push `started_at` to `run_times` (rows arrive ascending); non-zero `exit_code` -> `last_fail_at = max` |
| R5 | `ctx.prev_templates[i] = Some(templates.intern(template))` for row `i` |
| R6 | `followers[(idx, templates.intern(template))] = count` |
| R7 | `session_commands` |
| R8 | `record_pick(idx, timestamp, query, episode_queries)` in the order returned (ascending time); rows whose `command_id` is unknown are skipped |

A row whose `command_id` is not in `by_db_id` (deleted concurrently) is skipped
in every query. `ctx.now = p.now`.

### 5.3 Accessors (the only readers)

| Accessor | Returns |
|---|---|
| `dir_stat(idx, dir: Option<u32>) -> Option<&DirStat>` | `None` when `dir` is `None` or no entry |
| `runs_in_repo(idx, repo: Option<u32>) -> u32` | 0 when absent |
| `follower_runs(idx, template: Option<u32>) -> u32` | 0 when absent |
| `runs_in_window(idx, now, secs) -> u32` | Count of `run_times` in `(now - secs, now]` by two `partition_point`s; requires `secs <= CMD_EVENT_WINDOW_SECS` (assert) |
| `picks_in_window(idx, now, secs) -> u32` | Same over `picks[idx]` |
| `picks_for_query(query, idx, now, secs) -> u32`, `episode_picks_for_query(...)` | Same over the per-query maps |

## 6. Worker

`src/cmd/worker.rs`. One thread; owns the `CommandIndex`, the `RankContext`, one
`SkimMatcherV2`, a `CmdStore` connection, and (Phase C) the model.

### 6.1 Protocol

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope { All, Directory, Repo, Session }   // next(): All->Directory->Repo->Session->All; prev(): the reverse

pub(super) enum CmdRequest {
    UpdateQuery { query: String, query_id: u64, scope: Scope },
    GetRows { query_id: u64, start: usize },             // CMD_PAGE_ROWS rows from `start`
    Details { command_id: i64 },
    Delete { command_id: i64, query_id: u64 },           // delete, then answer like UpdateQuery for the current query
    Shutdown,
}
pub(super) enum CmdResponse {
    Loaded { commands: usize, load_ms: f64, model: Option<CmdModelStats> },   // model: Phase C (03 section 7.4)
    LoadFailed { message: String },                      // never contains command text
    QueryUpdated { query_id: u64, query: String, total_matched: usize, total_commands: usize, rows: Vec<CmdRow>, rank_ms: f64 },
    Rows { query_id: u64, start: usize, rows: Vec<CmdRow> },
    Details(CmdDetails),
    DeleteFailed,                                        // E-B-07; the QueryUpdated that follows still lists the command
    ModelDropped,                                        // Phase C, E-L-07: sent once, before that query's QueryUpdated
}
pub(super) struct CmdRow {
    pub command_id: i64, pub text: Arc<str>, pub last_run_at: i64,
    pub run_count: u32, pub pick_count: u32, pub runs_here: u32, pub last_exit_code: Option<i32>,
    pub score: f64, pub heuristic: f64, pub heuristic_parts: HeuristicParts,
    pub ml_score: Option<f64>, pub ml_weight: f64, pub fuzzy_score: i64,
    pub features: Vec<f64>,                              // registry order; empty when no model scored the row
}
pub(super) struct CmdDetails { pub command_id: i64, pub dirs: Vec<DirUse>, pub preceded_by: Vec<PrecededBy> }
// DirUse (R9) and PrecededBy (R10) are the row types of store.rs (01 section 6.5).

pub(super) fn spawn<T>(data_dir: &Path, params: LoadParams, event_tx: Sender<T>)
    -> (Sender<CmdRequest>, JoinHandle<()>) where T: From<CmdResponse> + Send + 'static;
```

### 6.2 Thread body

1. `CmdStore::open`, `CommandIndex::load`. On error send `LoadFailed`, then
   answer every later `UpdateQuery` (and `Delete`) with an empty `QueryUpdated`,
   and `Details` with empty vectors, until `Shutdown`.
1a. (Phase C) The index load of step 1 runs on a scoped thread while this
   thread loads the command model ([03 section 7.1](03-learn.md#71-loading-the-model)).
2. On a scoped thread, while step 1 runs:
   `rayon::broadcast(|_| { let _ = matcher.fuzzy_match("warm up", "wu"); })` - the
   rayon pool starts, and every rayon thread creates its matcher scratch space,
   off the load's critical path and not inside the first query.
3. Send `Loaded`. (`CommandIndex::load` has logged `TIMING {"op":"cmd_index_stages","readers":N,"context_ms":..,"read_ms":..,"pushed_ms":..,"applied_ms":..}`:
   when R2-R8, all reads, the entries' move into the index and the
   application of R2-R8 ended, in ms from the load's start.) Log `TIMING {"op":"cmd_index_load","ms":..,"commands":N}` and
   `TIMING {"op":"cmd_worker_ready","ms":<since PROCESS_START>}`.
4. Loop: `rx.recv()`, then drain with `try_recv` into a queue applying the
   supersession rule; process the queue in order. While seeds (section 6.3)
   remain to be computed, `try_recv` instead, computing one seed whenever
   nothing is waiting.

Supersession rule (same as `drain_requests`, [src/search_worker.rs:1508-1534](../../src/search_worker.rs)):
when the request being queued is an `UpdateQuery` and the last request already
in the queue is an `UpdateQuery`, remove that older one. When the request being
queued is a `Details`, remove every `Details` already in the queue: holding Down
asks for one per row, each is two queries (R9, R10; about 1 ms, up to 25 ms at
50,000 commands) on this thread, and they must not pile up ahead of the next
keystroke. The UI's half of the rule is in section 8.2.

### 6.3 Filter

The matching itself is a free function, so the benchmark can call it without a worker thread:

```rust
/// `base`: the idx list to test, ascending; `None` = every entry.
pub(super) fn filter_candidates(index: &CommandIndex, ctx: &RankContext, matcher: &SkimMatcherV2,
                                query: &str, scope: Scope, base: Option<&[u32]>) -> Vec<CommandCandidate>;
```

The worker wraps it in `fn filter(&mut self, query: &str, scope: Scope) -> Filtered`
(the candidates, the base-set size, `narrowed`, `reused`), which owns the
narrowing state, a *trail*: the matches (`Vec<CommandCandidate>`) of the
non-empty query being typed and of its prefixes typed before it, each entry's
query a prefix of the next one's, all in one scope. It decides the base set with
`pub(super) fn narrowing_applies(query: &str, scope: Scope, last_query: &str, last_scope: Scope) -> bool`
(the "Base set" rule below; also used by the benchmark's `latency`), and holds,
separately, `current_query`, `current_scope`, `current_query_id` (what `Delete`
and `GetRows` refer to):

| Step | Rule |
|---|---|
| Empty query | Every idx; the trail is neither used nor changed (typing the same letters again after clearing reuses it) |
| Trail | Drop entries from its end until the last one satisfies `narrowing_applies(query, scope, entry.query, entry.scope)`, that is `!query.is_empty() && !entry.query.is_empty() && query.starts_with(&entry.query) && scope == entry.scope` |
| Reuse | If the trail's last entry has exactly this query: its candidates are the result (`reused = true`, base-set size 0, no matching). Deleting back to a query typed before costs no filter |
| Base set | Otherwise the last entry's idx list (`narrowed = true`), or every idx `0..entries.len()` when the trail is empty. (A text that matches a longer query matches its prefix, so narrowing loses nothing) |
| Per idx | Skip when `filter_classes[idx]` lacks `LIVE` (deleted). Skip when out of scope (below). Empty query: keep with `fuzzy_score = 0`. Otherwise the prefilter, then the scorer (`src/cmd/fuzzy.rs`, below): rejected or `None` drops the row, `Some(s)` keeps it with `fuzzy_score = s` |
| Parallelism | When the query is non-empty and the base set has at least `CMD_PAR_FILTER_MIN` entries, the base set is cut into chunks of `FILTER_CHUNK` (1,024, `worker.rs`), one rayon task each with its own output vector, joined in order; a plain loop otherwise (with no query there is nothing to match, and a pass over 50,000 entries costs less than waking the pool). Output order is ascending idx in both cases |
| Seeds | While no request waits, the worker computes, one per loop turn, the matches in scope `All` of each single character of `SEED_CHARS` (`a`-`z`; an upper-case letter is a case-sensitive query of its own, and a digit or punctuation rarely starts a search; at 50,000 commands the 26 seeds hold about 530,000 candidates, 8.5 MB). When the trail is empty and the scope is `All`, a query starting with a seeded character starts the trail from that seed: typing the first letter is a reuse and the second narrows from it. `Delete` clears the seeds (they are computed again). One slice is one filter pass over every command, so a request that arrives meanwhile waits at most that |

The per-command test (`src/cmd/fuzzy.rs`, `Pattern`): a text can match only
if it holds every byte of the query in order (ASCII case folded unless the
query has an ASCII upper-case letter, the matcher's smart case). It is
checked in two steps: `filter_classes[idx]` must hold every class bit of the
query (64 bits: `a`-`z` case-folded, `0`-`9`, common punctuation, one bit for
the rest, one for any non-ASCII byte), which rejects most texts without
reading them; then the bytes are scanned in order. Only then is the scorer
called: `SkimMatcherV2::fuzzy_match`, unchanged. This holds for any matcher
that pairs each query character with an equal text character in order, so
the prefilter only ever rejects what the scorer would; the candidates and
scores are exactly those of matching every command (test
`the_filter_returns_exactly_what_matching_every_command_does`, and digests
of the 50,000-command key cycle, 04 section 6.6 `--dump`).
| Afterwards | A non-empty query that was not reused is pushed onto the trail with its result. `Delete` clears the trail (not `current_query`/`current_scope`) |
| Empty-query cache | The worker keeps the `CommandRanking` of the empty query per scope (`empty_rankings`, shared `Arc`s): asking for it again skips filter and ranking (`reused = true`, stage times 0). `Delete` clears it. When a page needs the rest sorted, the cache's handle is taken out first so `sort_rest` runs in place, and the sorted ranking is cached |

| Scope | Row is in scope when |
|---|---|
| `All` | always |
| `Directory` | `index.dir_stat(idx, ctx.dir).is_some()` |
| `Repo` | `index.runs_in_repo(idx, ctx.repo) > 0` |
| `Session` | `index.session_commands.contains(&idx)` |

The closure captures `&CommandIndex`, `&RankContext`, `&SkimMatcherV2` and the
prepared query: nothing lazily initialised, no lock, no allocation other than the output
([docs/how-it-works.md:1205-1207](../how-it-works.md)).

### 6.4 Handling each request

| Request | Action | Response |
|---|---|---|
| `UpdateQuery` | Set `current_query`, `current_scope`, `current_query_id`; `filter`, then `rank::rank_commands` (section 7) with `head = CMD_PAGE_ROWS`; keep the `CommandRanking` as `current` | `QueryUpdated` (echoing `query`) with rows `0..CMD_PAGE_ROWS`; then the TIMING line (6.5) |
| `GetRows` | Ignore unless `query_id == current_query_id`. If the page reaches past `current.sorted`, `current.sort_rest(index)` first (once per query, off the keystroke path; see the empty-query cache in 6.3) | `Rows` with rows `start..start + CMD_PAGE_ROWS` (clamped) |
| `Details` | R9 and R10 | `Details` |
| `Delete` | `store.delete_command(id, text, now)`; `index.remove(idx)`; clear the trail; re-run `current_query`/`current_scope` under the given `query_id`. On a database error: `DeleteFailed` first, the index untouched (E-B-07) | `QueryUpdated` |
| `Shutdown` | Return from the thread | - |

Building a `CmdRow` from a `CommandScore` clones the `Arc<str>` (no text copy),
reads `runs_here` from `dir_stat(idx, ctx.dir)`, and copies the row's
`features_of(rank)` (Phase C).

### 6.5 TIMING line

One per `UpdateQuery`/`Delete`, written **after** the response is sent
(convention: [docs/how-it-works.md:73-90](../how-it-works.md)):

```
TIMING {"op":"cmd_query","filter_ms":0.412,"heuristic_ms":0.201,"features_ms":null,
        "predict_ms":null,"blend_ms":null,"total_ms":0.655,"candidates":50000,
        "matched":1840,"stage2":0,"query_len":2,"narrowed":true,"reused":false}
```

Built by `CmdQueryTimings { filter_ms: f64, rank: CmdRankTimings, total_ms: f64, candidates: usize, matched: usize, query_len: usize, narrowed: bool, reused: bool }::to_json() -> String`
(`query_len` in characters). Milliseconds rounded to 3 decimals. `candidates` is the base-set size
(0 when `reused`: deleting back to a query typed before, or the cached empty query),
`stage2` the number of rows the model scored (0 and `null` stage times in this
phase). The line holds numbers and two booleans: no query text, no command text.
The op is `cmd_query`, not `query`, so `bench/harness.py` `parse_timings`
([bench/harness.py:165-173](../../bench/harness.py)) does not mistake it for a
file-mode line.

## 7. Heuristic ranking

`src/cmd/rank.rs`. In this phase it is the whole ranking; Phase C adds a model
stage on top ([03 section 7](03-learn.md#7-serving-two-stage-rank-and-blend)).

```rust
pub(super) struct CommandCandidate { pub idx: u32, pub fuzzy_score: i64 }
pub(super) struct HeuristicParts { pub recency: f64, pub dir: f64, pub repo: f64, pub fuzzy: f64 }
pub(super) struct CommandScore {
    pub idx: u32, pub score: f64, pub heuristic: f64, pub parts: HeuristicParts,
    pub ml_score: Option<f64>, pub ml_weight: f64, pub fuzzy_score: i64,
}
pub(super) struct CmdRankTimings { pub heuristic_ms: f64, pub features_ms: Option<f64>, pub predict_ms: Option<f64>, pub blend_ms: Option<f64>, pub stage2: usize }
pub(super) struct CommandRanking {
    pub scores: Vec<CommandScore>,   // scores[..sorted] in rank order; the rest after them, unordered
    pub sorted: usize,
    pub timings: CmdRankTimings,
    pub model_failed: bool,          // Phase C (03 section 7.2)
    pub features: Vec<f64>,          // Phase C: the model-scored rows' features, row-major
    pub feature_rows: Vec<u32>,      // Phase C: rank -> row of `features`
}
impl CommandRanking {
    pub(super) fn sort_rest(&mut self, index: &CommandIndex);   // sorted = scores.len() afterwards
    pub(super) fn features_of(&self, rank: usize) -> &[f64];    // registry order; empty when not model-scored
}

pub(super) fn heuristic(index: &CommandIndex, ctx: &RankContext, c: &CommandCandidate, max_fuzzy: i64) -> HeuristicParts;
pub(super) fn rank_commands(query: &str, candidates: &[CommandCandidate], index: &CommandIndex, ctx: &RankContext,
                            head: usize) -> CommandRanking;
pub(super) fn rank_order(index: &CommandIndex, a: &CommandScore, b: &CommandScore) -> std::cmp::Ordering;   // the total order below
```

For candidate `c` with entry `e` (`ln` is the natural logarithm):

| Term | Formula | Range |
|---|---|---|
| `recency` | `1 / (1 + max(0, now - e.last_run_at) / CMD_H_RECENCY_SCALE_SECS)` | (0, 1] |
| `dir` | `(d.runs / e.run_count) * min(1, ln(1 + d.runs) / ln(1 + CMD_H_DIR_SATURATION))` with `d = dir_stat(idx, ctx.dir)`; 0 when `None` or `run_count == 0` | [0, 1] |
| `repo` | Same with `r = runs_in_repo(idx, ctx.repo)` in place of `d.runs` | [0, 1] |
| `fuzzy` | `c.fuzzy_score / max_fuzzy` when the query is non-empty and `max_fuzzy > 0`, else 0; `max_fuzzy` = the largest `fuzzy_score` among the candidates of this call | [0, 1] for non-negative scores |
| `heuristic` | `(W_RECENCY*recency + W_DIR*dir + W_REPO*repo + W_FUZZY*fuzzy) / (W_RECENCY + W_DIR + W_REPO + W_FUZZY)` | [0, 1] |

`rank_commands` in this phase: compute `max_fuzzy`; compute `heuristic` for
every candidate (rayon `par_iter` when there are at least `CMD_PAR_FILTER_MIN`);
`score = heuristic`, `ml_score = None`, `ml_weight = 0.0`, `features` empty;
order by the total order **`score` descending (`f64::total_cmp`), then
`last_run_at` descending, then `db_id` descending** - exactly for the first
`head` rows, `sorted = min(head, len)`: `select_nth_unstable_by(sorted - 1)`,
then sort `[..sorted]`; with `head >= len` the whole list (`par_sort_unstable_by`
from `CMD_PAR_FILTER_MIN`). What is selected and sorted is a 32-byte key per
candidate (`score`, `last_run_at`, `db_id`, position); the `CommandScore`s are
built once, in key order, afterwards. The worker passes `head = CMD_PAGE_ROWS`:
a screen shows a few dozen rows, and sorting 50,000 to show them was most of
an empty query's cost. A caller that needs the full order (the benchmark's
relevance mode) passes `usize::MAX`, or calls `sort_rest`.

Consequences of the starting constants, with an empty query (`fuzzy = 0`):

| Statement | Derivation |
|---|---|
| Row 1 is the most recently run command `M` unless some command `D` satisfies `0.5*(dir_D - dir_M) + 0.2*(repo_D - repo_M) > recency_M - recency_D` | Definition of `heuristic` |
| If `M` ran within the last 300 seconds, no command last run more than 4 hours ago displaces it | The left side is at most 0.7; `recency_M >= 0.923` and `recency_D <= 0.2`, a gap of at least 0.723 |
| A command with all of its >= 10 runs in this directory (`dir = 1`, `repo = 0`) and last run long ago displaces an `M` never run here once `M` is older than 3,600 s | `recency_M < 0.5` |

With a non-empty query the ordering is recency plus directory plus `0.6 *`
relative match quality; metric R3 holds it to at least the newest-first baseline.

## 8. UI thread

### 8.1 State (`src/cmd/app.rs`)

```rust
pub(super) enum CmdEvent { Input(crossterm::event::Event), Worker(CmdResponse), Tick }   // + From<Event>, From<CmdResponse>

pub(super) struct CmdApp {
    pub query: String, pub scope: Scope,
    pub rows: Vec<CmdRow>,               // contiguous from rank 0
    pub total_matched: usize, pub total_commands: usize,
    pub selected: usize, pub scroll: usize, pub visible_rows: usize,
    pub shown_query_id: u64,             // id of the QueryUpdated the rows came from
    pub next_query_id: u64,              // starts at 2 (1 is the initial query)
    pub rows_requested_to: usize,        // highest `start` already asked for with GetRows
    pub confirm_delete: Option<i64>,     // command_id awaiting the one-key confirm
    pub help_visible: bool, pub debug_visible: bool,
    pub details: FxHashMap<i64, CmdDetails>, pub details_requested: FxHashSet<i64>,
    pub list_clock: i64,                 // "now" for time-ago; steps once a minute on Tick
    pub status: Option<String>,
    pub loaded: Option<LoadInfo>,        // LoadInfo { commands: usize, load_ms: f64 }, from CmdResponse::Loaded
    pub recording_on: bool,              // privacy marker / PSYCHIC_HISTORY, read once at start
    pub timings: CmdTimings,             // CmdTimings { first_results_ms, last_rank_ms, last_round_trip_ms: Option<f64> }
    pub home: Option<String>, pub cwd: String,
    pub events: events::CmdEvents,
    worker_tx: Sender<CmdRequest>, query_sent_at: Option<Instant>,
}
```

### 8.2 Event loop

Each iteration, in this order: `if visible_rows > 0 && events.wants_impressions(false) { events.log_impressions(&shown) }`
(section 10; `shown` = the rows on screen); `terminal.draw(|f| layout = render::draw(f, &app))`; store
`layout.visible_rows`; request details and the next page (below); block on
`event_rx.recv()`; handle the event.

| Event | Handling |
|---|---|
| `Worker(QueryUpdated)` | Accept only when `query_id >= shown_query_id` (same test as [src/main.rs:1045](../../src/main.rs)). Replace `rows`, totals; `shown_query_id = query_id`; `selected = 0`, `scroll = 0`; `rows_requested_to = 0`; `confirm_delete = None` (the selection moved, so `y` would delete another row); `events.new_subsession(query_id, <the response's query>)` (not `app.query`, which may already be longer); record `last_rank_ms` and the round trip since `query_sent_at` |
| `Worker(Rows)` | Accept only when `query_id == shown_query_id && start == rows.len()`; append |
| `Worker(Details)` | Insert into `details`; clear `details_requested` (the worker answers only the newest of the `Details` queued together, so a row selected again must be able to ask again) |
| `Worker(Loaded)` / `LoadFailed` | Store; `LoadFailed` sets `status = Some("Could not read history: <message>")` |
| `Tick` (every `CMD_TICK_MS`) | `list_clock` advances to the current minute boundary; the loop iteration that follows runs the impression check and redraws |
| `Input(Key)` | Section 9 |
| `Input(Mouse ScrollUp/ScrollDown)` | As `MoveUp` / `MoveDown`; ignored while `confirm_delete` or `help_visible` is set |
| `Input(Resize)` | Redraw (next iteration) |

After each draw: if the selected row's `command_id` is in neither `details` nor
`details_requested`, send `Details`. If `selected + CMD_PAGE_PREFETCH >= rows.len()`,
`rows.len() < total_matched` and `rows.len() > rows_requested_to`, send
`GetRows { query_id: shown_query_id, start: rows.len() }` and set `rows_requested_to`.

Changing the query or the scope: assign `query_id = next_query_id++`, send
`UpdateQuery`, set `query_sent_at`. The list on screen stays as it is until the
answer arrives. Nothing else reorders the list: there is no timer-driven
refresh and no model reload (ADR-11).

Scrolling: rank 1 is the top row. After every change of `selected`:
`if selected < scroll { scroll = selected }`;
`if visible_rows > 0 && selected >= scroll + visible_rows { scroll = selected + 1 - visible_rows }`.

TIMING lines written by the UI thread, each once per process, of the form
`TIMING {"op":"<name>","ms":<milliseconds since PROCESS_START>}`: `cmd_first_render` (end of the first draw),
`cmd_first_results` (first accepted `QueryUpdated`), `cmd_first_full_render`
(end of the first draw that had at least one row, or that followed a
`QueryUpdated` with zero rows). `cmd_first_full_render` is what B3 measures.
One more, per accepted `QueryUpdated` (Phase D): `TIMING
{"op":"cmd_key_to_results","ms":<ms>,"draw_ms":<ms>}` after the draw that follows it, `ms`
counted from when the UI thread sent the query (the key handler) to the end
of that draw, `draw_ms` that draw alone: the key-to-results time `bench/history.py` reports beside B2
([04 section 6.3](04-bench.md#63-keystroke-n200---pause-s---b2-and-b1)).

### 8.3 Layout and rows

`src/cmd/render.rs`: `pub(super) fn draw(f: &mut Frame, app: &CmdApp) -> CmdLayout`
(`CmdLayout { visible_rows: usize }`). It takes `&CmdApp`, performs no I/O, reads
no clock and no environment variable (B5).

| Terminal width | Debug off | Debug on |
|---|---|---|
| `>= CMD_STACK_BELOW_COLS` | Horizontal: list 50%, preview 50% | Horizontal: list 35%, preview 30%, debug 35% |
| `< CMD_STACK_BELOW_COLS` | Vertical: list 60%, preview 40% | Vertical: list 40%, debug 60% (preview hidden) |

Below them, full width, a 3-row bordered input box. Its title:
`` history - <scope label> - <total_matched>/<total_commands> `` where the scope
label is `all`, `this directory`, `this repo` or `this session`; the right end
of the bottom border shows `Ctrl-G help`. The box content is `> ` + query. When
`confirm_delete` is set the box content is instead
`Delete this command from psychic's history? y = delete, any other key = keep`.
`status`, when set, replaces the title until the next key press.

List row, left to right:

| Part | Width | Content |
|---|---|---|
| Rank | 4 | 1-based rank, right-aligned, dim |
| Fail marker | 1 | `!` in red when `last_exit_code` is `Some(n)` with `n != 0`, else a space |
| Here marker | 2 | `*` + space, dim, when `runs_here > 0`, else two spaces |
| Command | remaining | `text` with every `\n` replaced by `↵ ` (U+21B5 and a space), then `path_display::printable`, then `truncate_to_width` |
| Time | 16, right-aligned | `get_time_ago(Some(last_run_at), app.list_clock)` |

The selected row is drawn reversed. Matched characters are not highlighted.
When `rows` is empty the list area shows one centred line: `Could not read
history.` after `LoadFailed` (the status that said so is gone after a key),
else `No commands recorded yet.` if `total_commands == 0`, else `No match.`; and a second line `Recording
is off. Turn it on with: psychic history --enable` when `!recording_on` (not after a
failed load). Until
the first `QueryUpdated` is accepted it shows nothing (the index is still
loading; `No commands recorded yet.` would be a lie for those milliseconds).

Preview pane for the selected row, top to bottom:

| Block | Content |
|---|---|
| Command | The full text, one screen line per text line, each through `printable`, wrapped at the pane width |
| blank line | |
| `Last run` | `get_time_ago` |
| `Runs` | `<runs_here> here / <run_count> total` |
| `Last exit` | `0`, `<n> (failed)` or `unknown` |
| `Picked` | `<pick_count> times with Ctrl-R`; omitted when 0 |
| blank line, `Last directories` | Up to 5 `DirUse`: path (leading `home` shown as `~`, truncated), `<runs> runs`, time-ago. While the details are not in `app.details`: `...` |
| blank line, `Usually after` | Up to 3 `PrecededBy`: template, `<runs> times`. Omitted when empty |

Debug pane (Ctrl-O), for the selected row: `score`, `heuristic`, the four
`heuristic_parts`, `ml_score`, `ml_weight`, `fuzzy_score`; then one line per
feature `name = value` when `features` is non-empty (names from
[03 section 4](03-learn.md#4-features)), else `model: none`; then model stats
(Phase C, [03 section 7.4](03-learn.md#74-what-the-debug-pane-shows)); then
`here <cwd>` (`~`-abbreviated), `load <ms>, <n> commands`, `first results <ms>`,
`rank <ms>`, `round trip <ms>`, `matched <n> of <n>`.

Help overlay (Ctrl-G / F1): a centred bordered box listing every `CMD_KEYMAP`
row as `keys  description`, generated from the table (section 9), followed by
the line `Ctrl-R in your shell opens this; Enter puts the command on your prompt.`

## 9. Keys

`src/cmd/keys.rs`. A separate table (ADR-3); `src/keymap.rs` and `src/help.rs`
are not edited.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum_macros::EnumIter)]
pub(super) enum CmdAction { AppendToQuery, DeleteFromQuery, ClearQuery, MoveUp, MoveDown, Accept,
    ScopeForward, ScopeBackward, DeleteSelected, ToggleDebug, ToggleHelp, Cancel }
pub(super) struct CmdBinding { pub action: CmdAction, pub triggers: &'static [Trigger], pub description: &'static str }
pub(super) static CMD_KEYMAP: &[CmdBinding];
pub(super) fn lookup(code: KeyCode, modifiers: KeyModifiers) -> Option<CmdAction>;  // explicit chords first, then AnyChar for a plain char
pub(super) fn lookup_mouse(kind: MouseEventKind) -> Option<CmdAction>;
```

| Action | Triggers (in declaration order) | Description (help text) | Effect |
|---|---|---|---|
| `AppendToQuery` | `AnyChar` (a `KeyCode::Char` with neither CONTROL nor ALT; SHIFT is allowed) | type to search | Append the char; new query |
| `DeleteFromQuery` | Backspace | delete the last character | Pop one char; new query if it changed |
| `ClearQuery` | Ctrl-U | clear the query | Empty the query; new query if it changed |
| `MoveUp` | Up, Ctrl-P, wheel up | previous command | `selected -= 1` (stops at 0) |
| `MoveDown` | Down, Ctrl-N, wheel down | next command | `selected += 1` (stops at `total_matched - 1`, and at the last loaded row) |
| `Accept` | Enter | put the command on your prompt | Section 10 pick, then exit with the text; with no rows: exit with nothing |
| `ScopeForward` | Tab | next scope: all, this directory, this repo, this session | `scope.next()`; new query |
| `ScopeBackward` | Shift-Tab (`BackTab`) | previous scope | `scope.prev()`; new query |
| `DeleteSelected` | Delete, Ctrl-X | delete this command from psychic's history | Set `confirm_delete` to the selected `command_id` (no-op with no rows) |
| `ToggleDebug` | Ctrl-O | show why this command ranks here | Toggle `debug_visible` |
| `ToggleHelp` | Ctrl-G, F1 | show this help | Toggle `help_visible` |
| `Cancel` | Esc, Ctrl-C, Ctrl-D | leave without choosing | Exit with nothing |

Modal rules, applied before `lookup`, on key **press** events only:

| State | Rule |
|---|---|
| `confirm_delete = Some(id)` | Plain `y` or `Y`: send `Delete { command_id: id, query_id: next_query_id++ }`. Any other key: nothing. Either way clear `confirm_delete` and consume the key. An accepted `QueryUpdated` also clears it, and the wheel does nothing meanwhile (section 8.2) |
| `help_visible` | Any key closes help and is consumed (Esc does not cancel the picker here) |
| `status.is_some()` | Clear `status`; the key is then handled normally |

The helpers `key`, `ctrl_key`, `chr`, `ctrl` ([src/keymap.rs:204-232](../../src/keymap.rs)),
`chord_matches` ([:440-444](../../src/keymap.rs)) and `is_plain_char`
([:447-451](../../src/keymap.rs)) are private; `keys.rs` has its own copies
(`Chord`'s fields are `pub`, so `Trigger::Key(Chord { code, ctrl, alt })` can
be built in a `const fn`).

## 10. Event logging

`src/cmd/events.rs`, mirroring `Analytics` ([src/analytics.rs:37-193](../../src/analytics.rs)).
Rows go to `command_events` through E1/E2 of [01 section 6.4](01-record.md#64-events-delete-purge-import-status)
on the UI thread's own connection.

```rust
pub(super) struct CmdEvents { /* session_id, cwd, repo_root, shell_session, store,
    current: Option<Subsession { id, query, created_at: Instant, logged: bool, logged_ids: FxHashSet<i64> /* command ids, filled by log_impressions */ }>,
    episode_queries: Vec<String> */ }
impl CmdEvents {
    pub fn new_subsession(&mut self, query_id: u64, query: String);
    pub fn wants_impressions(&mut self, force: bool) -> bool;
    pub fn log_impressions(&mut self, shown: &[Shown]) -> anyhow::Result<()>;   // Shown { command_id: i64, rank: usize }
    pub fn log_pick(&mut self, picked: Shown) -> anyhow::Result<()>;
}
// `store` is `Option<CmdStore>`: `None` when the UI thread could not open it (E-B-19); every method is then a no-op.
```

| Rule | Definition |
|---|---|
| Subsession | Starts when a `QueryUpdated` is accepted; id = its `query_id`; query = the `query` field of that `QueryUpdated` |
| `wants_impressions(force)` | Push the subsession's query into `episode_queries` if absent (always). Return `!logged && (force \|\| created_at.elapsed() >= CMD_IMPRESSION_AGE_MS)` - the logic of [src/analytics.rs:94-111](../../src/analytics.rs) |
| Impressions | The rows on screen: ranks `scroll + 1 ..= scroll + visible_rows` that exist. One transaction, one E1 per row with `action = 'impression'`, `rank`, `query`, `session_id`, `subsession_id`; `episode_queries`, `cwd`, `repo_root`, `shell_session` NULL. Then `logged = true`. Nothing is logged before the first draw (`visible_rows == 0`) |
| When checked | Top of every loop iteration with `force = false` (so within one tick, `CMD_TICK_MS`, of the query settling); with `force = true` immediately before a pick |
| Pick | After the forced impressions: if the picked `command_id` is not in the current subsession's `logged_ids` (the user scrolled after the impressions were logged), first one more E1 `impression` for it (same query, its rank), so every pick has a label-1 impression. Then one E1 with `action = 'pick'`, the row's `rank`, the query, `episode_queries` = JSON array of `episode_queries`, and `cwd`, `repo_root`, `shell_session`; then E2. Then `episode_queries.clear()` |
| Cancel | Nothing more is written; the impressions already logged stay and train nothing (no positive) |
| Failure | Section 15, E-B-06: logged, never shown, never blocks the pick |

## 11. Shell widgets

Appended to each script after the Phase A block. Each replaces the **whole**
prompt buffer with the pick and passes the current buffer as the initial query
(what fzf and Atuin do). Bound only when all hold at the time the script is
sourced: recording is enabled (`__psychic_history_enabled`,
[01 section 11](01-record.md#11-shell-hooks)), `PSYCHIC_NO_CTRL_R` is unset, and
Ctrl-R still has the shell's default binding. A user who enables history in a
running shell gets the binding in the next shell (`--enable` says so,
[01 section 13.1](01-record.md#131---enable---no-import)).

### 11.1 zsh

```zsh
# =============================================================================
# Ctrl-R: search command history in psychic
# =============================================================================

# Enter on a command replaces the line being edited with it; Escape leaves the
# line alone. What is already typed becomes the search.
__psychic_history_widget() {
    \builtin local result
    result="$(\command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        history --session="${__psychic_session}" --cwd="${PWD}" --query="${BUFFER}")"
    if [[ -n "${result}" ]]; then
        BUFFER="${result}"
        CURSOR=${#BUFFER}
    fi
    \builtin zle reset-prompt
}
\builtin zle -N __psychic_history_widget

# Bound to Ctrl-R only where history search is on and Ctrl-R still does what
# zsh does by default. Set PSYCHIC_NO_CTRL_R before loading this to skip it;
# bind the widget yourself with, e.g.,
#   bindkey '^X^R' __psychic_history_widget
if [[ -z "${PSYCHIC_NO_CTRL_R-}" ]] && __psychic_history_enabled; then
    for __psychic_keymap in emacs viins; do
        case "$(\builtin bindkey -M "${__psychic_keymap}" '^R' 2>/dev/null)" in
            *history-incremental-search-backward|*redisplay|*undefined-key|'')
                \builtin bindkey -M "${__psychic_keymap}" '^R' __psychic_history_widget ;;
        esac
    done
    \builtin unset __psychic_keymap
fi
```

zsh defaults: `^R` is `history-incremental-search-backward` in `emacs` and
`redisplay` in `viins`.

### 11.2 bash

```bash
# =============================================================================
# Ctrl-R: search command history in psychic
# =============================================================================

# Enter on a command replaces the line being edited with it; Escape leaves the
# line alone. Needs bash 4 or newer, for READLINE_LINE.
__psychic_history_widget() {
    \builtin local result
    result="$(\command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        history --session="${__psychic_session}" --cwd="${PWD}" --query="${READLINE_LINE}")" || return
    [[ -n "${result}" ]] || return
    READLINE_LINE="${result}"
    READLINE_POINT=${#READLINE_LINE}
}

# Bound only in an interactive bash 4+, where history search is on and Ctrl-R
# still does what readline does by default. Set PSYCHIC_NO_CTRL_R before
# loading this to skip it; bind the function yourself with, e.g.,
#   bind -x '"\C-x\C-r": __psychic_history_widget'
if [[ $- == *i* && -z "${PSYCHIC_NO_CTRL_R-}" && "${BASH_VERSINFO[0]}" -ge 4 ]] && __psychic_history_enabled; then
    if ! \builtin bind -X 2>/dev/null | \command grep -q '^"\\C-r"' &&
        \builtin bind -p 2>/dev/null | \command grep -q '^"\\C-r": reverse-search-history'; then
        \builtin bind -x '"\C-r": __psychic_history_widget'
    fi
fi
```

### 11.3 fish

```fish
# =============================================================================
# Ctrl-R: search command history in psychic
# =============================================================================

# Enter on a command replaces the command line with it; Escape leaves it alone.
# Does nothing (and prints nothing) when the binary is not installed.
function __psychic_history_widget
    set -l __psychic_bin psychic
    set -q PSYCHIC_BIN; and set __psychic_bin $PSYCHIC_BIN
    command -q $__psychic_bin; or return
    set -l __psychic_dir
    set -q PSYCHIC_DATA_DIR; and set __psychic_dir --data-dir $PSYCHIC_DATA_DIR
    set -l __psychic_query (commandline | string collect)
    set -l result (command $__psychic_bin $__psychic_dir history --session="$__psychic_session" --cwd="$PWD" --query="$__psychic_query" | string collect)
    if test -n "$result"
        commandline --replace -- $result
    end
    commandline --function repaint
end

# Bound to Ctrl-R only where history search is on and nothing else has claimed
# the key. Set PSYCHIC_NO_CTRL_R before loading this to skip it; bind the
# function yourself with, e.g.,
#   bind \cx\cr __psychic_history_widget
if not set -q PSYCHIC_NO_CTRL_R; and __psychic_history_enabled
    # --user: fish 4 lists the preset binding beside the user's own. Each
    # mode on its own: a user's insert-mode Ctrl-R is theirs too.
    for __psychic_mode in default insert
        set -l __psychic_current (bind --user -M $__psychic_mode \cr 2>/dev/null)
        if test -z "$__psychic_current"; or string match -q -- '*history-pager*' $__psychic_current
            bind -M $__psychic_mode \cr __psychic_history_widget
        end
    end
end
```

`string collect` keeps a multi-line pick as one value. fish 3.6+ binds Ctrl-R
to `history-pager` by default; older fish has no default binding (empty output).
Checked on fish 4.2.1: `bind \cr` prints the preset (`bind --preset ctrl-r
history-pager`) next to a user's own binding, so without `--user` the guard
matched `history-pager` and took over an fzf or Atuin Ctrl-R; `bind --user \cr`
prints only the user's binding, or nothing (and fails) when there is none; it
covers one mode, so the default and insert modes are checked and bound each on
its own (a user's insert-mode Ctrl-R stays theirs). The
widget checks the binary with `command -q` first, as the recording hook does,
so a missing binary prints nothing (E-B-16). fish 4 asks the terminal for its
attributes at start-up and waits 2 s for an answer a test pty never gives; the
pty tests wait for fish's first prompt mark before typing.

| Detail common to all three | Decision |
|---|---|
| Option form | `--query=VALUE` / `--cwd=VALUE` / `--session=VALUE` with `=`, so a buffer starting with `-` is a value, as the existing `pd` does ([shell/psychic.zsh:46](../../shell/psychic.zsh)) |
| Cursor after a pick | End of the buffer |
| Empty result (cancel) | Buffer untouched |
| The picked command is not executed | The user presses Enter |

## 12. Latency design

Targets B1-B3, B5. The decisions that make them reachable:

| Decision | Detail |
|---|---|
| No SQL per keystroke | Everything a query needs is in `CommandIndex`; the only SQL after start-up is `Details` (off the query path), event logging (UI thread, not in `draw`) and `Delete` |
| Bounded start-up SQL | Exactly R1-R8, one full read (R1), the rest index-covered and proportional to one directory, one repo, 30 days or one session |
| Start-up reads side by side | R1 split by id over `CMD_LOAD_READERS` connections that also build the entries, R2-R8 on one more, each memory-mapped (section 5.2); the rayon pool starts and warms meanwhile (section 6.2) |
| Work in parallel with terminal set-up | The worker is spawned before the terminal is touched (section 4, steps 2-5) |
| Parallel filter with a floor | rayon from `CMD_PAR_FILTER_MIN` candidates and only with a query; sequential otherwise, where waking the pool costs more than it saves |
| Reject before scoring | A 64-bit byte-class mask per command in one array, then an in-order byte scan, before the scorer (section 6.3): most commands are rejected without reading their text, and the empty query reads only the array |
| First letters precomputed | The matches of each single lower-case letter are computed while the worker is idle after start-up (section 6.3, seeds), so the first letter of a search, a scan of every command, costs no filter |
| Narrowing and reuse | Typing a longer query re-matches only the matches of its longest prefix typed before; deleting back to a query typed before reuses its matches; the empty query's ranking is kept per scope until a delete (section 6.3) |
| Sort only what is shown | `select_nth` + sort of the first `CMD_PAGE_ROWS` 32-byte keys; the rest is sorted only when a page reaches it (section 7) |
| No cold global in the parallel loop | One matcher, warmed on every rayon thread at start (section 6.2, step 2); the registry of Phase C is a `static` array, not a `Lazy` |
| No per-row allocation on the query path | `Arc<str>` texts; the model-scored rows' features in one buffer of the ranking, not a vector per row; `CmdRow`s built only for the 200 sent |
| One log write per query, after the send | Section 6.5 |
| Render is pure | `draw(&mut Frame, &CmdApp)`; time-ago uses `list_clock`; `~` uses `app.home` |

Start-up allowances per stage; they sum to the B3 target:

| Stage | Allowance | Thread |
|---|---|---|
| Process start, clap, logger | 3 ms | main |
| Terminal set-up, input thread | 5 ms | main |
| `CmdStore::open` | 1 ms | worker |
| R1 over 50,000 rows, split over `CMD_LOAD_READERS`, entries built | 20 ms | worker + readers |
| R2-R8 | 6 ms | one reader, during R1 (not on the critical path) |
| Model load (Phase C) | 3 ms (measured 3.8 ms p50, 5.3 ms p95; beside the index load, so off the critical path) | worker, while a scoped thread loads the index |
| First query: 50,000 heuristics + sort (+ stage 2 in Phase C) | 8 ms | worker |
| First full draw | 4 ms | main |

Per-keystroke allowances on the worker, summing to the B1 target: filter 3 ms,
heuristic + sort 1.5 ms, features + predict + blend 3.5 ms (Phase C).

Measured at Phase B (not the Phase D harness; a stdlib Python pty driver
shaped like [04 section 6](04-bench.md#6-performance-benchmarks): release
build, the perf DB of 50,000 commands / 500,000 runs from `generate --seed 1`,
`--cwd` its most-used directory (13,517 runs; the directory does not exist on
disk, so no repository root), `--session` the newest run's, 40 x 120 pty,
50 start-ups after one warm-up, 360 keys of the 6.3 cycle with 50 ms between
them; i9-12950HX, 24 threads, `powersave` governor, load average about 2 from
other work; ranges over the last three runs of the final code):

| Metric | p50 | p95 | Target |
|---|---|---|---|
| B3 `cmd_first_full_render` | 37.7-40.4 ms | 46.0-52.4 ms | <= 50 ms |
| `cmd_first_results` / `cmd_index_load` | 33-35 / 29-31 ms | 39-45 / 35-40 ms | - |
| first query, worker | 2.4-2.7 ms | 3.9-4.6 ms | - |
| B1 `cmd_query.total_ms` (all but the first) | 1.5 ms | 5.4-5.6 ms | <= 8 ms |
| filter / heuristic + sort | 0.15 / 0.67 ms | 3.9 / 2.6-2.7 ms | 3 / 1.5 ms allowance |
| B2 keystroke -> first byte of redraw | 1.4 ms | 2.1-2.7 ms | <= 16 ms |

B3 is at its target, not under it with room: one run of three was 2.4 ms over
at p95 (an earlier run of the same design, before the review fixes, read 44 ms);
the load is most of it, R1 being 50,000 rows however it is split.

Measured at Phase C with the model active (same driver, corpus and context;
a model trained on the perf DB: 8,000 groups, 132 trees, ml weight 1.0;
load average 1.5-2.5 from other work; three runs of the final code): B1
`cmd_query.total_ms` 3.3-3.7 ms p50, 6.8-7.8 ms p95 (target 8); stage 2 alone
(2,000 rows) features 0.41 / 0.90, predict 1.13 / 1.99, blend 0.42 / 0.69 ms
(p50 / p95); B2 1.5-1.6 / 2.9-3.0 ms; B3 38.7-41.0 ms p50 and 46.1, 47.5,
50.9 ms p95 (one run of three 0.9 ms over the 50 ms target; an earlier run of
the same design under a load average of 3 read 46-61 ms, the spread being the
index load's); the first query 4.4-5.0 ms p50 against 2.4-2.7 ms without a
model. See [03 section 7.2](03-learn.md#72-rank_commands-with-a-model) for
why prediction is not a LightGBM call.

By query: one letter (typed over all 50,000, 37,000 match; or deleted back
to, reused) 2.4-2.5 ms p50, 3.8-4.3 ms p95; two letters, narrowed, 2.0-2.4 /
6.3-6.5 ms; longer queries under 4 ms p95; the empty query again 0.5 ms (kept). The heaviest cases are the
fuzzy matcher over tens of thousands of texts and the rayon pool waking from
idle at a low clock; at p95 they exceed the Phase B allowances (filter 3.9
against 3 ms, heuristic + sort 2.7 against 1.5 ms), which leaves Phase C
about 2.5 ms at p95 rather than 3.5 ms. The same code measured
in-process with the pool hot takes about half these times. Before the
start-up reads were split (one connection, R1-R8 in order) B3 was 55 / 61 ms;
before the partial sort and the reuse rules B1 was 4.9 / 11.7 ms.

## 13. Anti-patterns (DO NOT)

| Don't | Do instead | Why |
|---|---|---|
| Make `search_worker`, `App`, `render` or `preview` generic over files and commands | Parallel slim TUI in `src/cmd/` (ADR-2) | Fork drift; they are file-shaped throughout |
| Add rows to `keymap::KEYMAP` or variants to `keymap::Action` | `CMD_KEYMAP` (ADR-3) | `test_every_action_is_bound` and the file help screen |
| Run SQL, `stat`, `Timestamp::now()` or `std::env::var` inside `render::draw` | Read from `&CmdApp`; fetch on the worker; step `list_clock` on Tick | B5 |
| Query SQLite per keystroke | `CommandIndex` | B1 |
| Sort every candidate to show one page, or move 112-byte scores while sorting | `rank_commands(.., head)`: select and sort 32-byte keys; `sort_rest` when a page needs it | Most of an empty query's cost at 50,000 commands |
| Share one `CmdStore` between threads | One connection per thread (`CmdStore::open_file`) | A rusqlite `Connection` is `Send`, not `Sync` |
| Create a `SkimMatcherV2` per query or per candidate | One on the worker, shared by reference | Allocation per call; cold scratch space on every rayon thread |
| Put a `Lazy`/`OnceCell`/thread-local first-use inside the `par_iter` closure | Resolve before the loop and capture by reference | Startup cliff ([docs/how-it-works.md:1205-1207](../how-it-works.md)) |
| Write the TIMING line before sending `QueryUpdated`, or one line per stage | One line after the send | A log write is a syscall between the keystroke and the results |
| Put the query or a command in a TIMING or `log::` line | Lengths and counts only | [01 section 10.6](01-record.md#106-no-command-text-outside-eventsdb) |
| Re-rank, reload the model or refresh the index on a timer or when retraining finishes | Rank only in response to `UpdateQuery`/`Delete` | The list must not move without user input |
| Reset the selection on `Rows` or `Details` | Only an accepted `QueryUpdated` resets `selected` | A page arriving must not move the cursor |
| Print the pick while the alternate screen is active by writing to `/dev/tty` | `println!` to stdout after `leave_tui` | stdout is the widget's pipe |
| Execute the picked command, or append a newline that the shell would run | Print the text; the widget assigns the buffer | Requirement: lands on the prompt for editing |
| Take over Ctrl-R when the user rebound it (fzf, Atuin) or when recording is off | The three binding guards of section 11 | Never break a working Ctrl-R |
| `--query "$BUFFER"` with a space | `--query="$BUFFER"` | A buffer beginning with `-` would parse as an option |
| Use tuples for multi-field results (`(String, u32, i64)`) | Named structs (`DirUse`, `PrecededBy`, `Shown`) | [llm.md:5](../../llm.md) |
| Hard-code a heuristic weight or page size in `rank.rs`/`worker.rs` | `tuning.rs` constants (section 3) | The benchmark tunes them in one place |

## 14. Test case specifications

### 14.1 Unit tests

Fixtures use `now = 1_000_000` and an index built with the mutators of 5.1.

| ID | Component | Input | Expected | Edge cases covered |
|---|---|---|---|---|
| TC-B-01 | `heuristic` recency | Command last run 10 s ago, never here, empty query | `recency = 0.99723`, `heuristic = 0.43358` (+-1e-5) | `last_run_at > now` -> `recency = 1.0` |
| TC-B-02 | `heuristic` directory | 10 runs, all in `ctx.dir`, last run 172,800 s ago | `recency = 0.02041`, `dir = 1.0`, `heuristic = 0.22626` (+-1e-5) | `ctx.dir = None` -> `dir = 0`; 1 of 10 runs here -> `dir = 0.1 * ln2/ln11 = 0.02891` |
| TC-B-03 | `rank_commands` empty query | A (10 s ago, not here), B (TC-B-02) | Order A, B | Replace A by A2 run 7,200 s ago -> order B, A2 |
| TC-B-04 | `rank_commands` tie-break | Two commands, identical stats, `db_id` 5 and 9 | 9 before 5 | Same `score`, different `last_run_at` -> newer first |
| TC-B-05 | `rank_commands` fuzzy term | Query `gs`; candidates with `fuzzy_score` 40 and 80, same recency | `fuzzy` parts 0.5 and 1.0; the 80 first | All `fuzzy_score <= 0` -> `fuzzy = 0` for all |
| TC-B-06 | `filter` | Texts `git status`, `cargo build`, `ls`; query `gs` (not `cargo test`: its `g`...`s` is a fuzzy match) | Only `git status` | Empty query keeps all 3 with `fuzzy_score = 0`; a `deleted` entry is never returned |
| TC-B-07 | `filter` narrowing | Queries `g` then `gi` then `x` on 3 texts | Second call's base-set size equals the first call's match count; the third call's base set is all entries | Scope change between calls disables narrowing; result of narrowed `gi` equals un-narrowed `gi` |
| TC-B-08 | `filter` scopes | A run here only, B in repo only, C in session only, D none | `Directory` -> [A]; `Repo` -> [B]; `Session` -> [C]; `All` -> 4 | `ctx.dir = None` with `Directory` -> empty |
| TC-B-09 | supersession | Queue `UpdateQuery(1)`, `UpdateQuery(2)`, `Details`, `UpdateQuery(3)`, `UpdateQuery(4)` drained in that order | Processed: `UpdateQuery(2)`, `Details`, `UpdateQuery(4)` | A single request passes through unchanged; `Details(1)`, `Details(2)`, `GetRows`, `Details(3)` -> `GetRows`, `Details(3)` |
| TC-B-10 | stale responses | `shown_query_id = 5`; deliver `QueryUpdated(4)`, then `(6)`; `Rows(5, ..)` | 4 ignored, 6 applied (selection reset to 0), `Rows(5)` ignored | `Rows` with `start != rows.len()` ignored |
| TC-B-11 | `CMD_KEYMAP` | Iterate `CmdAction` | Every action has exactly one row; no trigger appears in two rows; every description is non-empty | `lookup(Char('x'), CONTROL)` -> `DeleteSelected`; `lookup(Char('x'), NONE)` -> `AppendToQuery`; `lookup(Char('q'), CONTROL)` -> `None` |
| TC-B-12 | modal keys | `confirm_delete = Some(7)`; press `n`, then set again and press `y` | First: no request, cleared. Second: one `Delete { command_id: 7, .. }` | `help_visible` + Esc -> help closed, picker still running; with a confirmation up, the wheel moves nothing and an accepted `QueryUpdated` clears it |
| TC-B-13 | row text | `"echo a\nb\tc\x1b[31m"`, width 40 | `echo a↵ b    c·[31m` | Width 6 -> `echo …`; a CJK command truncates on a character boundary |
| TC-B-14 | impressions | New subsession; `wants_impressions(false)` at once, then after `created_at` is aged 200 ms, then again after logging | `false`, `true`, `false` | `force = true` at once -> `true`; `episode_queries` gains the query on the first call even when it returns `false` |
| TC-B-15 | `CommandIndex` windows | `run_times = [now-4000, now-3000, now-10]` | `runs_in_window(.., 3600) = 2`, `(.., 60) = 1`, `(.., 5) = 0` | A run exactly at `now - secs` is excluded; at `now` included |
| TC-B-16 | `record_run` ordering | Runs at 300 (exit 0), 100 (exit 1), 200 (exit NULL) | `run_times = [100, 200, 300]`, `last_exit_code = Some(0)`, `last_fail_at = 100` | - |
| TC-B-17 | TIMING shape | A `CmdQueryTimings` with known numbers -> `to_json()` | Exactly the keys of section 6.5 (with `reused`); `features_ms` is `null`; contains no string value but `op` | - |
| TC-B-18 | filter reuse | Queries `g`, `gi`, `g`, `""`, `g`; then `g` in another scope; then a delete | The second `g` is `reused` (base set 0, same candidates), also after `""`; not in another scope or after `forget_narrowing`; `gr` after `gi` narrows from `g` and equals a fresh filter | - |
| TC-B-19 | partial order | 2,500 commands, `head` in {0, 1, 200, len-1} | `scores[..head]` equal the full order's head; the rest holds the same commands; after `sort_rest` the whole order equals the full one | - |
| TC-B-20 | empty-query cache | `""`, `c1`, `""` on 230 commands; `GetRows` past 200; delete | The second `""` answers with the same `Arc` (no ranking); the page past the head is in rank order and the cache holds the sorted copy; a delete empties the cache | - |
| TC-B-21 | id split | `split_ids` of `1..=10` in 4, `5..=6` in 4, `i64::MIN..=i64::MAX` in 4 | `[1-3, 4-6, 7-9, 10]`; `[5, 6]`; 4 contiguous parts covering both ends | - |

### 14.2 Integration tests

| ID | Flow | Setup | Verification | Teardown |
|---|---|---|---|---|
| IT-B-01 | `CommandIndex::load` | Temp data dir; `record_run` 6 runs over 3 commands, 2 dirs, 1 repo, 2 sessions; 1 pick event | `entries.len() == 3`; `dir_stat`, `runs_in_repo`, `follower_runs`, `session_commands`, `picks_in_window` equal hand-written expected values; `ctx.prev_templates` lists the session's last 3 templates newest first | `remove_dir_all` |
| IT-B-02 | Worker round trip | As IT-B-01; `worker::spawn`; send `UpdateQuery("", 1, All)`, `UpdateQuery("g", 2, All)`, `Details`, `Delete`, `Shutdown` | `Loaded`, then `QueryUpdated` per accepted query with the expected texts in order; `Details` has the expected dirs; after `Delete` the command is absent from the rows and from the three tables; the thread joins | `remove_dir_all` |
| IT-B-03 | Pick logging | `CmdEvents` on a temp store; subsessions `g`, `gi`; impressions for 3 rows; `log_pick` rank 2 | `command_events`: 3 impressions with ranks 1-3 and subsession ids, 1 pick with `episode_queries = ["g","gi"]`, `cwd`, `repo_root`, `shell_session` set; `commands.pick_count == 1` | `remove_dir_all` |
| IT-B-04 | Render | `ratatui::backend::TestBackend` 140x30 and 100x30 with a `CmdApp` holding 3 rows (one failed, one run here, one multi-line) | 140 wide: list and preview side by side; 100 wide: stacked; the failed row has `!`, the here row `*`, the multi-line row `↵`; the preview shows `Runs` and `Last exit` lines; debug on shows `model: none`; help overlay lists 12 key rows | none |
| IT-B-05 | Picker end to end (`tests/picker.rs`, pty helper `tests/support/mod.rs`) | Temp data dir with `echo one`, `echo two` (newer); spawn `psychic --data-dir T history --cwd /tmp` on a pty with stdout redirected to a file | Environment includes `PSYCHIC_NO_RETRAIN=1`. Before every key, wait until the pty has been quiet for 300 ms (the list is drawn). Send Enter: exit 0, file is `echo two\n`. Second run, send `one`, wait for quiet again, then Enter: `echo one\n`. Third run, send Esc: exit 0, file empty. `app.log` in T contains `cmd_first_full_render` and no `echo` | `remove_dir_all` |
| IT-B-06 | Widget blocks ship | Output of `psychic zsh`, `psychic bash`, `psychic fish` | Each contains `__psychic_history_widget`, `PSYCHIC_NO_CTRL_R`, `__psychic_history_enabled`, and `--query=`; none contains `--query "` | none |
| IT-B-07 | No query text in logs | IT-B-05's second run with the query `SENTINELq` | `app.log` does not contain `SENTINELq` | `remove_dir_all` |
| IT-B-08 | Ctrl-R in each shell (`tests/picker.rs`) | zsh, bash and fish on a pty with the script, recording on by the marker; `echo alpha`, `echo beta` recorded in session `seed` | Type `beta`, Ctrl-R, Enter (picker), Enter (shell): `echo beta` runs, is recorded once in the shell's session, and the one pick event has `query = beta`, the shell's session and cwd, and a timestamp 0-300 s before that run (the link `follows_pick` of [03 section 5.1](03-learn.md#51-inputs-and-the-replayer) makes, `CMD_PICK_RUN_SECS`). Then `echo kept`, Ctrl-R, Esc, Enter: `echo kept` runs as typed and no second pick is logged | Scratch dropped |
| IT-B-09 | Binding guards | Each shell: recording on; `PSYCHIC_NO_CTRL_R=1`; recording off; the user's own Ctrl-R bound before the script (bash: a readline function, and `bind -x` as fzf does; fish: both modes, and insert mode only) | The widget is bound in the first case only; fish with only insert mode taken binds the default mode and leaves insert alone | Scratch dropped |
| IT-B-11 | The line goes over verbatim | Each shell; `echo -x '$HOME'` recorded; type `-x '$H`, Ctrl-R, Enter, Enter | The pick event's query is `-x '$H` exactly; `-x $HOME` is printed | Scratch dropped |
| IT-B-10 | A failing or missing picker | Each shell with `PSYCHIC_BIN=false`; fish also with a binary that does not exist | Ctrl-R then Enter runs the typed line unchanged; fish prints nothing about the missing binary | Scratch dropped |

`tests/support/mod.rs` holds the pty helper shared by `tests/shell_hooks.rs`
and `tests/picker.rs` ([01 section 15.3](01-record.md#153-shell-level-tests-testsshell_hooksrs)).

## 15. Error handling matrix

| # | Failure | Detection | Response | Fallback | Logging |
|---|---|---|---|---|---|
| E-B-01 | Database cannot be opened or R1-R8 fail | `Err` in the worker's load | `CmdResponse::LoadFailed { message }` (operation and sqlite code only) | Picker stays open with an empty list, the status `Could not read history: <message>` and, once that is cleared, `Could not read history.` in the list; Enter/Esc exit 0 with nothing | `log::error!` |
| E-B-02 | No controlling terminal (`/dev/tty` cannot be opened) | `Err` at section 4 step 5 | Worker shut down; `Err` to `main`, exit 1, stdout empty | Widget leaves the buffer untouched | stderr via `main` |
| E-B-03 | Terminal set-up fails midway | `Err` after raw mode is on | `input::leave_tui` is called before returning the `Err` | As E-B-02 | stderr via `main` |
| E-B-04 | Worker thread panics | `event_rx`/`worker_tx` send fails, or `JoinHandle::join` is `Err` | UI sets status `History worker stopped`; list frozen; keys still work | Enter prints the selected row from `rows` | `log::error!` |
| E-B-05 | Fuzzy match yields nothing | `total_matched == 0` | List shows `No match.` | Enter exits with nothing | None |
| E-B-06 | Event logging fails (database busy/locked) | `Err` from `log_impressions`/`log_pick` | Ignored for the user: the pick is still printed | That episode is not logged | `log::error!("command event logging failed: <code>")` |
| E-B-07 | `Delete` fails in the database | `Err` from `delete_command` | Worker does not call `index.remove`; sends `CmdResponse::DeleteFailed`, then answers with the unchanged list | UI status `Could not delete` | `log::error!` |
| E-B-08 | `Details` query fails | `Err` | `Details` with empty vectors | Preview shows no directory lines | `log::warn!` |
| E-B-09 | Stale `QueryUpdated`/`Rows` | id tests of section 8.2 | Dropped | - | None |
| E-B-10 | `--session` given but that session has no rows | R5 returns nothing | `prev_templates` all `None`; R6 skipped; `Session` scope shows `No match.` | Sequence terms are 0 | None |
| E-B-11 | cwd unknown (no `--cwd`, no `$PWD`, `current_dir` fails) | all three absent | `cwd = ""`, `ctx.dir = None`, `repo_root = None` | Directory terms are 0; `Directory`/`Repo` scopes empty | `log::warn!` |
| E-B-12 | Terminal narrower than 20 columns or shorter than 5 rows | `f.area()` | Draw only the input box | - | None |
| E-B-13 | Command text contains control or escape characters | - | `path_display::printable` in rows and preview | - | None |
| E-B-14 | Query contains characters invalid for the matcher (any Unicode) | - | Passed through; `fuzzy_match` returns `None` or a score | - | None |
| E-B-15 | The picker binary fails or is killed under the widget | Non-zero exit / empty stdout | zsh, fish: buffer untouched, prompt repainted. bash: `\|\| return` leaves `READLINE_LINE` untouched | - | None |
| E-B-16 | `psychic` not on PATH under the widget | zsh, bash: shell error "command not found" on the terminal. fish: `command -q` fails | Buffer untouched; fish prints nothing | - | None |
| E-B-17 | Recording is off and nothing is recorded | `total_commands == 0 && !recording_on` | The two-line hint of section 8.3 | Esc/Enter exit 0 | None |
| E-B-18 | `tty_input::spawn` fails | `Err` | As E-B-02 | - | stderr via `main` |
| E-B-19 | The UI thread's `CmdStore::open` fails (section 4 step 6) | `Err` | `CmdEvents` is built with `store = None` | No impressions or picks are logged this session; the picker works | `log::error!` |

## 16. References

| Topic | Location |
|---|---|
| Targets B1, B2, B3, B5, R3; ADR-2, ADR-3, ADR-7, ADR-11, ADR-12 | [00-blueprint.md section 2](00-blueprint.md#2-success-metrics), [section 8](00-blueprint.md#8-architecture-decision-records) |
| SQL R1-R10, E1-E2, D1-D5 | [01-record.md section 6](01-record.md#6-sql-statements) |
| `HistoryArgs` | [01-record.md section 4.2](01-record.md#42-psychic-history) |
| Opt-in check used by the binding guards | [01-record.md section 10.1](01-record.md#101-opt-in) |
| Model stage added to `rank_commands` | [03-learn.md section 7](03-learn.md#7-serving-two-stage-rank-and-blend) |
| How B1-B3 are measured | [04-bench.md section 6](04-bench.md#6-performance-benchmarks) |
| File-mode worker protocol and supersession | [src/search_worker.rs:112-203](../../src/search_worker.rs), [src/search_worker.rs:1508-1534](../../src/search_worker.rs) |
| File-mode "send, then log one TIMING line" | [src/search_worker.rs:1285-1302](../../src/search_worker.rs), [src/search_worker.rs:1229-1271](../../src/search_worker.rs) |
| File-mode event loop to mirror | [src/main.rs:877-1146](../../src/main.rs) |
| File-mode terminal set-up and shutdown order | [src/main.rs:765-781](../../src/main.rs), [src/main.rs:796-820](../../src/main.rs) |
| File-mode pick output | [src/main.rs:1126-1129](../../src/main.rs) |
| Impression rule and episode queries | [src/analytics.rs:22](../../src/analytics.rs), [src/analytics.rs:94-111](../../src/analytics.rs), [src/analytics.rs:168-175](../../src/analytics.rs) |
| Existing Ctrl-T widgets and binding guards | [shell/psychic.zsh:71-94](../../shell/psychic.zsh), [shell/psychic.bash:75-95](../../shell/psychic.bash), [shell/psychic.fish:70-91](../../shell/psychic.fish) |
| Keymap types reused | [src/keymap.rs:83-97](../../src/keymap.rs), [src/keymap.rs:513-522](../../src/keymap.rs) |
| Layout threshold precedent | [src/render.rs:392-460](../../src/render.rs) |
