# Phase C: Learn (Implementation)

Document type: **Implementation** (HOW). Strategy and targets:
[00-blueprint.md](00-blueprint.md). This document is the single source for the
command features, the training-set replay, the second model and its files, and
the model stage of ranking (two-stage rank and blend).

## 1. Scope, inputs, done when

| | |
|---|---|
| Builds | `features.rs`, `train.rs`, the model stage in `rank.rs`, `psychic history --retrain`, the detached retrain at picker start, the debug-pane feature list |
| Inputs (must exist) | Phase A ([01](01-record.md)); Phase B: `CommandIndex` and its mutators/accessors ([02 section 5](02-browse.md#5-commandindex)), `rank_commands` heuristic stage ([02 section 7](02-browse.md#7-heuristic-ranking)), worker ([02 section 6](02-browse.md#6-worker)); Phase D0: the synthetic generator ([04 section 2](04-bench.md#2-files-touched-and-the-two-commits)) for the done-when commands |
| Leaves for Phase D | Measuring R1-R3, B1, B6, B8 and tuning the constants of section 3 |

Done when all of these pass:

```bash
just build && cargo test && cargo clippy
cargo test cmd::features && cargo test cmd::train && cargo test cmd::rank
./bench/train_parity.py            # must print "identical: N trees, byte for byte": the file model is unchanged by section 6.1
D=/tmp/psychic-bench/cmd-c; rm -rf $D
cargo run --release -- --data-dir $D internal bench-history generate --commands 2000 --runs 20000 --seed 1
cargo run --release -- --data-dir $D history --retrain     # prints "Trained on ... rows" and "Best iteration ..."
cargo run --release -- --data-dir $D history --status      # the model line of section 6.4, not "model: none"
ls $D/cmd_model.txt $D/cmd_model_stats.json
./bench/run.py setup HEAD~1 && ./bench/run.py startup 6 && ./bench/run.py keystroke 30   # file mode unchanged
```

## 2. Files touched

New files: `src/cmd/features.rs`, `src/cmd/train.rs` and its tests
`src/cmd/train/tests.rs`, `src/cmd/model.rs` (`CmdModel`: the load checks
of section 7.1, the blend weight of 7.3, and prediction: the model text read
into a `Forest` whose trees are walked in Rust, section 7.2 step 5). Extended: `src/cmd/rank.rs`
(section 7), `src/cmd/worker.rs` (loads the model, passes it to `rank_commands`),
`src/cmd/app.rs` (section 8), `src/cmd/render.rs` (section 7.4),
`src/cmd/mod.rs` (`--retrain`, `--status` model line), `src/cmd/tuning.rs` (section 3),
`src/cmd/store.rs` (`#[derive(Clone)]` on `EventRecord`, for `PickEpisode`;
the Phase C `dead_code` markers go), `src/cmd/index.rs` (its Phase C markers
go), `tests/no_text_leaks.rs` (a training test: report, `training.log`,
`app.log` and both model files hold no command text).

| Upstream file | Exact edit |
|---|---|
| [src/training/mod.rs](../../src/training/mod.rs) | Section 6.1 |
| [src/ranker.rs:465](../../src/ranker.rs) | `fn compute_blend_weights` -> `pub(crate) fn compute_blend_weights` |
| [src/ranker.rs:707](../../src/ranker.rs) | `fn logistic` -> `pub(crate) fn logistic` |
| `docs/how-it-works.md` | Extend the "Shell history search" section with features, training and blend |

Not edited: `src/feature_defs/*`, `src/features.rs`, `train.py`.
`crate::feature_defs::schema::FeatureType` ([src/feature_defs/schema.rs:9-12](../../src/feature_defs/schema.rs))
and `crate::feature_defs::Monotonicity` ([:15-18](../../src/feature_defs/schema.rs))
are already `pub` and are reused.

## 3. Constants (Phase C)

Appended to `src/cmd/tuning.rs`. All are tuning candidates
([04 section 9](04-bench.md#9-tuning-candidates-in-priority-order)).

| Constant | Type | Value | Meaning |
|---|---|---|---|
| `CMD_STAGE2_K` | `usize` | `2000` | Rows, taken from the top of the heuristic order, that the model scores |
| `CMD_BLEND_SCALE` | `f64` | `10.0` | `blend_examples` are divided by this before the file blend formula; crossover at 300, saturation near 600 |
| `CMD_PICK_WEIGHT` | `f64` | `4.0` | Sample weight of every row of a Ctrl-R pick group |
| `CMD_HOOK_WEIGHT` | `f64` | `1.0` | Sample weight of rows of a group built from a hook-recorded run |
| `CMD_IMPORT_WEIGHT` | `f64` | `0.3` | Sample weight of rows of a group built from an imported run |
| `CMD_TRAIN_SEED` | `u64` | `42` | Seed of query-length choice and negative sampling; also the LightGBM seed in production |
| `CMD_TRAIN_MAX_RUN_GROUPS` | `usize` | `8000` | Only the newest this-many eligible runs produce groups |
| `CMD_TRAIN_MIN_GROUPS` | `usize` | `50` | Below this many groups nothing is trained |
| `CMD_TRAIN_QLEN_MIX` | `[u32; 4]` | `[40, 20, 20, 20]` | Percent of run groups with synthetic query length 0, 1, 2, 3 |
| `CMD_TRAIN_FRAGMENT_SHARE` | `u32` | `25` | Percent of run groups with a non-empty query whose query is a fragment of a later word (section 5.2) rather than a prefix |
| `CMD_TRAIN_RECENT_NEGATIVES` | `usize` | `0` (was 15) | Negatives taken from the most recently run matching commands. 0 since 2026-11-17: fixed newest negatives skew every recency feature (section 5.2) |
| `CMD_TRAIN_RECENT_SCAN` | `usize` | `500` | Distinct recent commands examined to find them |
| `CMD_TRAIN_RANDOM_NEGATIVES` | `usize` | `30` (was 15) | Negatives drawn at random from all matching commands |
| `CMD_RECENCY_RANK_CAP` | `usize` | `15` | Feature 25 counts newer matching commands up to this |
| `CMD_TRAIN_RANDOM_TRIES` | `usize` | `200` | Draws allowed to find them |
| `CMD_NEVER_SECS_LOG` | `f64` | `19.57` | "Never" on the `ln(1 + seconds)` axis (`ln(1 + 10 years)`, as [src/feature_defs/implementations.rs:533](../../src/feature_defs/implementations.rs)) |
| `CMD_FAIL_RECENT_SECS` | `i64` | `120` | Window of `failed_within_2_minutes` |
| `CMD_RETRAIN_MIN_INTERVAL_SECS` | `u64` | `3600` | The picker starts a retrain only when the last attempt (successful or not) is older than this |
| `CMD_PICK_RUN_SECS` | `i64` | `300` | A run this soon after a Ctrl-R pick of the same command in the same shell session is that pick being executed; it gets no group of its own |
| `CMD_TRAIN_LOCK_STALE_SECS` | `u64` | `900` | A lock file older than this is taken over |

Unchanged from file training because the same function fits both models:
half-life 180 days, `MAX_ROUNDS` 1000, `STOPPING_ROUNDS` 50
([src/training/mod.rs:30-36](../../src/training/mod.rs)) and every LightGBM
parameter of `lightgbm_params` ([src/training/mod.rs:177-199](../../src/training/mod.rs)),
including `lambdarank_truncation_level=30`.

## 4. Features

`src/cmd/features.rs`.

```rust
pub(super) struct CommandFeatureInputs<'a> {
    pub query: &'a str,
    pub query_lower: &'a str,           // query.to_lowercase(), computed once per ranking call
    pub idx: u32,
    pub entry: &'a CommandEntry,
    pub fuzzy_score: i64,
    pub index: &'a CommandIndex,
    pub ctx: &'a RankContext,
}
pub(super) trait CommandFeature: Send + Sync {
    fn name(&self) -> &'static str;
    fn feature_type(&self) -> FeatureType;
    fn monotonicity(&self) -> Option<Monotonicity>;
    /// Infallible arithmetic over already-resolved inputs (as src/feature_defs/schema.rs:82-90).
    fn compute(&self, inputs: &CommandFeatureInputs) -> f64;
}
/// Append-only. The position is the column in the model. A `static` slice, not a
/// `Lazy`: it is read inside a rayon loop.
pub(super) static COMMAND_FEATURE_REGISTRY: &[&dyn CommandFeature] = &[ /* the rows below, in this order */ ];
pub(super) fn command_feature_names() -> Vec<&'static str>;
pub(super) fn command_feature_specs() -> Vec<crate::training::FeatureSpec<'static>>;   // name + monotone (1, -1, 0)
pub(super) fn compute_all(inputs: &CommandFeatureInputs, out: &mut [f64]);             // writes exactly REGISTRY.len() values (asserts out.len())
```

Notation: `e` = `inputs.entry`, `now` = `ctx.now`, `d` = `index.dir_stat(idx, ctx.dir)`,
`win(s)` = `index.runs_in_window(idx, now, s)`, `P30` = `CMD_EVENT_WINDOW_SECS`.
Type N = Numeric, B = Binary. Mono: `+` Increasing, `-` Decreasing, blank none.

| # | Name | Type | Mono | Formula | Value when unknown |
|---|---|---|---|---|---|
| 1 | `fuzzy_score` | N | + | `fuzzy_score as f64` | `0.0` when `query` is empty |
| 2 | `command_starts_with_query` | B | | `1.0` if `query` non-empty and `e.text_lower.starts_with(query_lower)` | `0.0` |
| 3 | `word_starts_with_query` | B | | `1.0` if `query` non-empty and any `e.text_lower.split_whitespace()` word starts with `query_lower` | `0.0` |
| 4 | `picks_for_this_query` | N | + | `index.picks_for_query(query, idx, now, P30)` | `0.0` |
| 5 | `picks_in_episode_with_query` | N | + | `index.episode_picks_for_query(query, idx, now, P30)` | `0.0` |
| 6 | `log_seconds_since_last_run` | N | - | `ln(1 + max(0, now - e.last_run_at))` | `CMD_NEVER_SECS_LOG` when `e.run_count == 0` |
| 7 | `runs_last_hour` | N | + | `win(3600)` | `0.0` |
| 8 | `runs_last_day` | N | + | `win(86_400)` | `0.0` |
| 9 | `runs_last_7_days` | N | + | `win(604_800)` | `0.0` |
| 10 | `runs_last_30_days` | N | + | `win(2_592_000)` | `0.0` |
| 11 | `log_total_runs` | N | + | `ln(1 + e.run_count)` | `0.0` |
| 12 | `picks_last_30_days` | N | + | `index.picks_in_window(idx, now, P30)` | `0.0` |
| 13 | `cwd_run_share` | N | + | `d.runs / e.run_count` | `0.0` when `d` is `None` or `run_count == 0` |
| 14 | `repo_run_share` | N | + | `index.runs_in_repo(idx, ctx.repo) / e.run_count` | `0.0` when `ctx.repo` is `None` or `run_count == 0` |
| 15 | `log_seconds_since_last_run_in_cwd` | N | - | `ln(1 + max(0, now - d.last_run_at))` | `CMD_NEVER_SECS_LOG` when `d` is `None` |
| 16 | `runs_in_cwd` | N | + | `d.runs` | `0.0` |
| 17 | `follows_prev_share` | N | + | `index.follower_runs(idx, ctx.prev_templates[0]) / e.run_count` | `0.0` when `prev_templates[0]` is `None` or `run_count == 0` |
| 18 | `follows_recent3_share` | N | + | `sum over the distinct Some(t) in ctx.prev_templates of index.follower_runs(idx, t)`, divided by `e.run_count` | `0.0` |
| 19 | `success_rate` | N | | `e.success_count / (e.success_count + e.fail_count)` | `f64::NAN` when the denominator is 0 (LightGBM treats NaN as missing) |
| 20 | `failed_within_2_minutes` | B | | `1.0` if `e.last_fail_at > 0 && now - e.last_fail_at < CMD_FAIL_RECENT_SECS && e.last_fail_at <= now` | `0.0` |
| 21 | `last_exit_nonzero` | B | | `1.0` if `e.last_exit_code` is `Some(n)` with `n != 0` | `0.0` |
| 22 | `command_length` | N | | `e.chars` | - |
| 23 | `argument_count` | N | | `e.words.saturating_sub(1)` | `0.0` |
| 24 | `fzf_score` | N | + | `inputs.fzf_score`: fzf's score of the text for the query under `--scheme=history` ([02 section 7.1](02-browse.md#71-fzfs-score-srccmdfzfrs)); serving takes it from the heuristic's parts, training computes it with the same `rank::fzf_score` | `0.0` for the empty query; `-1.0` (`NO_FZF_MATCH`) when fzf does not match |
| 25 | `recency_rank` | N | | `inputs.recency_rank`: how many commands matching the query (the candidates, for serving; the commands `newest_matching` finds, for training) have a newer `(last_run_at, db_id)`, at most `CMD_RECENCY_RANK_CAP`. Serving takes the `CMD_RECENCY_RANK_CAP` newest candidates in the heuristic's pass (`rank::newest_of`, one pass over the sort keys, no allocation per candidate); training walks the replay's recency order until it has that many matches (`train::newest_matching`); IT-N-02 checks both give the same list | - |

Features 24 on were added on 2026-11-17 (results.md). Feature 24 sits
beside feature 1 rather than replacing it: measured both ways, see results.md.

Feature 18 is "the share of this command's runs whose immediately preceding
command had the template of any of the last 3 commands of this shell session".
A feature added later goes at the end; a feature is never removed or reordered,
only replaced in place with a new `name` (which invalidates the model through
the name check of section 7.1).

## 5. Training-set replay

`src/cmd/train.rs`. A time-ordered replay with one accumulator, so a row's
features reflect only what had happened before it (the approach of
[src/features.rs:44-65](../../src/features.rs)). The accumulator **is** a
`CommandIndex`, filled through its mutators ([02 section 5.1](02-browse.md#51-mutators-the-only-writers));
features are computed by the same `compute_all` as serving.

### 5.1 Inputs and the replayer

```rust
// CommandRecord (T2), RunRecord (T3), EventRecord (T4): the row types of store.rs, 01 section 6.5.
pub(super) struct ReplayInput { pub commands: Vec<CommandRecord>, pub runs: Vec<RunRecord>, pub events: Vec<EventRecord> }
impl ReplayInput { pub(super) fn read(store: &CmdStore) -> anyhow::Result<Self>; }   // all_commands, all_runs, all_events

pub(super) struct Replayer { pub index: CommandIndex, /* private: command records by id, template id per command,
    session tails, recency stack, pending impressions */ }
impl Replayer {
    pub fn new(commands: &[CommandRecord]) -> Self;                 // empty index; commands enter it at their first run
    pub fn knows(&self, command_id: i64) -> Option<u32>;            // idx if the command has run before
    pub fn context(&self, now: i64, cwd: Option<&str>, repo_root: Option<&str>, shell_session: Option<&str>) -> RankContext;
    pub fn recent_distinct(&self) -> impl Iterator<Item = u32> + '_;// most recently run first, each idx once
    /// True when a pick already applied has this run's command_id and shell_session and
    /// 0 <= run.started_at - pick.timestamp <= CMD_PICK_RUN_SECS.
    pub fn follows_pick(&self, run: &RunRecord) -> bool;
    pub fn last_cwd(&self, idx: u32) -> Option<u32>;                // dir id of the command's most recent run with a known cwd
    pub fn apply_run(&mut self, run: &RunRecord);
    pub fn apply_event(&mut self, event: &EventRecord) -> Option<PickEpisode>;   // never changes the index
    pub fn commit_pick(&mut self, episode: &PickEpisode);           // index.record_pick for the pick
}
pub(super) struct PickEpisode { pub pick: EventRecord, pub impressions: Vec<EventRecord> }
```

| Method | Exact behaviour |
|---|---|
| `context` | `dir = cwd.and_then(\|c\| index.dirs.get(c))`, `repo` likewise (`&self`: nothing is interned; a directory never seen gives `None`, which yields the same feature values as an id with no statistics), `prev_templates` = the template ids of the last up-to-3 runs applied for `shell_session`, newest first; `now` as given |
| `apply_run` | A run whose `command_id` is not among the commands read (recorded between the T2 and T3 reads) is skipped; the Cap pre-pass ignores it too. First run of a command: `index.add_command(id, text)`. A known (non-empty) `cwd` updates `last_cwd[idx]`. Then `index.record_run(idx, started_at, dir id, repo id, exit_code, template id of prev_command_id if any)`; push the run's template id on that session's tail (keep 3); push `(idx, seq)` on the recency stack and set `last_seq[idx] = seq` |
| `recent_distinct` | Walk the recency stack from the end, yielding entries whose `seq == last_seq[idx]`. The stack is compacted (stale entries dropped) when its length exceeds `2 * entries.len() + 1024` |
| `apply_event`, impression | Append to `pending[session_id]`; return `None` |
| `apply_event`, pick | Remove and return `pending[session_id]` (possibly empty) together with the pick as a `PickEpisode`. The index is not changed, so the caller builds the group from the state before the pick. A pick or impression whose `command_id` has never run is dropped (`None`); a dropped pick still ends its episode (its pending impressions are removed with it) |
| `commit_pick` | `index.record_pick(idx, pick.timestamp, &pick.query, &pick.episode_queries)`. Called by the caller after it has built (or skipped) the group |

Timeline: runs in T3 order (`started_at, id`), events in T4 order (`id`), merged
by time (`started_at` for runs, `timestamp` for events); at equal times events
come first.

### 5.2 Groups from executed commands

For each run, **before** `apply_run`:

| Step | Rule |
|---|---|
| Eligible | `replayer.knows(run.command_id)` is `Some(pos)`: the command had run before, so it could have been found. A first-ever run produces no group. Also not eligible: a run for which `replayer.follows_pick(run)` holds - the pick group already represents that decision, and the index now contains the pick (features 4, 5, 12), a state serving never sees before the choice. `commit_pick` records the last pick per `(shell_session, command_id)` for this test; the Cap pre-pass applies the same rule in the same timeline order |
| Cap | Number eligible runs 0.. in timeline order; let `E` be their count among runs with `started_at < until` (all runs when `until` is `None`), found by a first pass over the timeline with a seen-set and the pick rule. Only eligible ordinals `>= E.saturating_sub(CMD_TRAIN_MAX_RUN_GROUPS)` produce groups. Earlier ones only feed the index |
| Query length | `r = splitmix64(seed ^ (run.id as u64)) % 100`; length 0 if `r < 40`, 1 if `r < 60`, 2 if `r < 80`, else 3 (the cumulative `CMD_TRAIN_QLEN_MIX`) |
| Query | `training_query`: with `d = splitmix64(seed ^ run.id ^ FRAGMENT_SALT)`, a non-empty length and `d % 100 < CMD_TRAIN_FRAGMENT_SHARE`, the fragment `fragment_query(text, length, d >> 8)` (the definition the relevance benchmark's fragment style uses, [04 section 5.2](04-bench.md#52-protocol), with another salt) when there is one; otherwise the first `length` characters (`chars().take(length)`) of the positive's text, the whole text if it is shorter |
| Context | `ctx = replayer.context(run.started_at, run.cwd, run.repo_root, Some(&run.shell_session))` |
| "Matches" | Empty query: every command. Otherwise `matcher.fuzzy_match(&entry.text, query).is_some()` with one `SkimMatcherV2` owned by the builder |
| Recent negatives | From `recent_distinct()`: skip the positive and deleted entries; examine at most `CMD_TRAIN_RECENT_SCAN` of the rest; keep those that match, up to `CMD_TRAIN_RECENT_NEGATIVES` (0 since 2026-11-17: none; see below) |
| Random negatives | `state = splitmix64(seed ^ (run.id as u64) ^ 0x9E37_79B9_7F4A_7C15)`; repeat at most `CMD_TRAIN_RANDOM_TRIES` times: `state = splitmix64(state)`, `cand = state % entries.len()`; keep `cand` if it is not the positive, not deleted, not already chosen (recent or random), and matches; stop at `CMD_TRAIN_RANDOM_NEGATIVES` |
| Skip | No negative found, or the positive does not match its own query (a prefix of its text always matches; the rule only makes the function total): no group |
| Rows | The positive (label 1) and the negatives (label 0), **ordered by idx ascending**; each row = `compute_all` with that candidate's own fuzzy score for the query (0 for the empty query) |
| Weight | `CMD_HOOK_WEIGHT` or `CMD_IMPORT_WEIGHT` by `run.source`, on every row of the group |
| Timestamp | `run.started_at` on every row |

```rust
fn splitmix64(mut x: u64) -> u64 { x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9); x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB); x ^ (x >> 31) }
```

The one definition is `corpus::splitmix64` (Phase D0's generator uses the
same function); `train.rs` imports it.

At most 31 rows per group (1 + `CMD_TRAIN_RECENT_NEGATIVES` +
`CMD_TRAIN_RANDOM_NEGATIVES`).

Why no recent negatives (2026-11-17). A ranker scored row by row learns, in
effect, how much likelier a command with given features is to be the
positive than a matching command drawn as a negative. With the 15 newest
matching commands always among the negatives, the newest commands are
negatives in every group while serving shows them once each, so the model
learned that recently run commands are less likely than they are: on the
interactive 50k corpus its top-10 at length 0 was 0.41 against newest-first
0.53, and it lost to plain score-sorted fuzzy matching at lengths 1-3.
Uniform negatives represent the candidates as serving sees them; the same
corpus went to 0.54 top-10 at length 0 and above every baseline at lengths
2-3 (results.md). Feature 25 made the effect visible: with a cap of 20 the
ranks 15-19 were almost never a negative (the 15 newest were, the random
ones rarely land there), and the model put those commands first.

The table from "Context" down is one function, so tests can fix the query:

```rust
/// `None` when no negative is found. Does not touch the replayer.
fn build_run_group(replayer: &Replayer, matcher: &SkimMatcherV2, run: &RunRecord, query: &str, seed: u64) -> Option<Group>;
struct Group { rows: Vec<GroupRow /* idx, label, features */>, weight: f64, timestamp: i64 }
```

### 5.3 Groups from Ctrl-R picks

For each `PickEpisode` (the file approach: the pick is the positive, the other
impressions of the episode are the negatives):

| Step | Rule |
|---|---|
| Rows | One per impression in `impressions` (all subsessions of that picker process), in event-id order, each with its **own** `query` |
| Label | 1 when the impression's `command_id` equals the pick's, else 0 |
| Skip | No row has label 1, or no row has label 0 |
| Context | `replayer.context(pick.timestamp, pick.cwd, pick.repo_root, pick.shell_session)` for every row: the whole episode is scored at the moment of the pick |
| Fuzzy score | `matcher.fuzzy_match(text, impression.query)`, 0 when the query is empty or there is no match |
| Weight | `CMD_PICK_WEIGHT` |
| Timestamp | `pick.timestamp` |
| Afterwards | `replayer.commit_pick(&episode)` |

Impressions of a picker process that never picked are never emitted.

### 5.4 Output

```rust
pub(super) struct BuildOptions { pub seed: u64, pub max_run_groups: usize,
    pub until: Option<i64> }      // Some(t): stop the timeline at the first run/event with time >= t (benchmark splits)
pub(super) struct GroupCounts { pub pick: usize, pub hook: usize, pub import: usize, pub rows: usize }
pub(super) struct BuiltSet { pub set: crate::training::TrainingSet, pub counts: GroupCounts, pub replayer: Replayer }
pub(super) fn build_training_set(input: &ReplayInput, options: &BuildOptions) -> BuiltSet;
```

Group ids (`episodes`) are 1, 2, 3, ... in the order groups are emitted, which
is time order, so `time_split` ([src/training/mod.rs:252-273](../../src/training/mod.rs))
cuts by time. The returned `replayer` holds the state at `until`, which is what
the benchmark continues from ([04 section 5.2](04-bench.md#52-protocol)).
Row count bound: `8000 * 31 = 248,000` run rows plus pick rows.

## 6. Fit and model files

### 6.1 Refactor of `src/training/mod.rs`

Minimal, and bit-for-bit neutral for the file model (`./bench/train_parity.py`
is the proof and is in "done when").

| Location | Change |
|---|---|
| `TrainingSet` ([:68-76](../../src/training/mod.rs)) | Add field `row_weights: Vec<f64>` (one per row). `from_csv` ([:137-149](../../src/training/mod.rs)) fills it with `1.0` per row (`row_weights: vec![1.0; rows.len()]` in the struct literal) |
| after `from_csv` | Add `pub fn from_parts(num_features: usize, features: Vec<f64>, labels: Vec<f32>, episodes: Vec<i64>, timestamps: Vec<f64>, row_weights: Vec<f64>) -> Self`. Asserts: `features.len() == labels.len() * num_features`; the four per-row vectors have equal length; `episodes.is_sorted()`; every weight is finite and `> 0`; every label is 0.0 or 1.0 |
| `lightgbm_params` ([:177](../../src/training/mod.rs)) | New third parameter `seed: u32`, passed at both call sites ([:395](../../src/training/mod.rs), [:426](../../src/training/mod.rs)); the four lines `seed=42`, `bagging_seed=42`, `feature_fraction_seed=42`, `data_random_seed=42` ([:192-195](../../src/training/mod.rs)) use it |
| `fit` ([:369](../../src/training/mod.rs)) | Body moves to `pub fn fit_seeded(set: &TrainingSet, features: &[FeatureSpec], seed: u32) -> Result<Fitted>`; `pub fn fit(set, features)` becomes `fit_seeded(set, features, 42)` |
| in `fit_seeded`, after `let weights = recency_weights(&set.timestamps);` ([:376](../../src/training/mod.rs)) | `let weights: Vec<f64> = weights.iter().zip(&set.row_weights).map(\|(w, r)\| w * r).collect();` (`w * 1.0` is exactly `w`, so the file model's weights are unchanged) |
| `atomic_write` ([:542](../../src/training/mod.rs)) | `fn` -> `pub(crate) fn` |

`train` ([:490](../../src/training/mod.rs)), `registry_features`, `time_split`,
`EarlyStopping` and `mod lgbm` are not touched.

### 6.2 `retrain`

```rust
pub(super) fn retrain(data_dir: &Path, seed: u64, quiet: bool) -> anyhow::Result<RetrainOutcome>;
pub(super) enum RetrainOutcome { Trained(Box<CmdModelStats>), NotEnoughData { groups: usize }, AlreadyRunning, HistoryShrank }
```

| Step | Action |
|---|---|
| 1 | Write the current time to `data_dir/"cmd_train_attempt"` (create or truncate; its mtime is the back-off clock of section 8). Take the lock: `OpenOptions::new().write(true).create_new(true).open(data_dir/"cmd_train.lock")`. The new lock holds a token (`<pid> <nanoseconds>`), and while it is held a heartbeat thread refreshes its mtime every 60 s, so a fit longer than `CMD_TRAIN_LOCK_STALE_SECS`, or one paused by a suspend, is not judged stale. If the lock exists and its mtime is older than `CMD_TRAIN_LOCK_STALE_SECS`, remove it if it still holds the token read when it was judged stale, and try once more. Still exists -> `AlreadyRunning`. A guard stops the heartbeat and removes the lock on every exit path, including panic unwinding, but only if the lock still holds its own token |
| 2 | `CmdStore::open`; `counts()` (for step 6); `ReplayInput::read` (the connection is closed once read) |
| 3 | `build_training_set(&input, &BuildOptions { seed, max_run_groups: CMD_TRAIN_MAX_RUN_GROUPS, until: None })` |
| 4 | Total groups `< CMD_TRAIN_MIN_GROUPS` -> `NotEnoughData`; no file is written or removed |
| 5 | `crate::training::fit_seeded(&set, &command_feature_specs(), seed as u32)`. An `Err` whose message starts with `every split needs rows` -> `NotEnoughData`; any other `Err` propagates |
| 6 | `write_unless_shrunk`: write the model text and `serde_json::to_string_pretty(&stats)` beside their targets as `<name>.<pid>.pending`; unless the history shrank since step 2 (fewer commands or runs in `CmdStore::counts`; E-L-15), rename them into place, `cmd_model.txt` first, then `cmd_model_stats.json` (the write-then-rename of `training::atomic_write`, with the check between); `privacy::tighten_permissions(data_dir)`; check once more |
| 7 | Report: `quiet == false` -> print to stdout; `quiet == true` -> append to `data_dir/"training.log"` under `\n=== Command model training run at <local time> ===` (format as [src/ranker.rs:880-895](../../src/ranker.rs)). An `Err` from any step is logged (`log::error!`) and, under `quiet`, appended the same way as `Training failed: <error>`, then returned (E-L-03) |

Report text (no command text, section 10.6 of 01):

```
Trained on <rows> rows in <groups> groups (<pick> picks, <hook> runs, <import> imported): train <n>, validation <n>, test <n>
Best iteration <n>, validation NDCG@1,5 <a>, <b>
Took <s.ss>s
```

`NotEnoughData` prints/logs `Nothing to train on yet: <groups> groups (need 50). Ranking stays on the heuristic.`

### 6.3 `cmd_model_stats.json`

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct CmdModelStats {
    pub trained_at: String,               // jiff::Timestamp::now().to_string()
    pub trained_at_unix: i64,
    pub training_duration_seconds: f64,   // rounded to 2 decimals
    pub feature_names: Vec<String>,       // registry order at training time
    pub num_rows: usize,
    pub pick_groups: usize, pub hook_groups: usize, pub import_groups: usize,
    pub blend_examples: f64,              // pick_groups*CMD_PICK_WEIGHT + hook_groups*CMD_HOOK_WEIGHT + import_groups*CMD_IMPORT_WEIGHT
    pub best_iteration: usize,
    pub validation_ndcg: Vec<f64>,        // NDCG@1, NDCG@5
    pub feature_gain: Vec<FeatureGain>,   // FeatureGain { feature: String, gain: f64 }, every feature, gain descending
    pub seed: u64,
}
impl CmdModelStats {   // the one constructor; used by `retrain` and by the benchmark
    pub(super) fn from_fit(fitted: &crate::training::Fitted, counts: &GroupCounts, seed: u64, seconds: f64, now: jiff::Timestamp) -> Self;
}
```

### 6.4 Status line

`psychic history --status` replaces `model: none` with
`model: trained <trained_at>, <groups> groups (<pick> picks, <hook> runs, <import> imported), <best_iteration> trees, ml weight <w to 2 decimals>`
when `CmdModel::load` (section 7.1) succeeds; otherwise `model: none`.

## 7. Serving: two-stage rank and blend

### 7.1 Loading the model

```rust
pub(super) struct CmdModel { forest: Forest, pub stats: CmdModelStats, pub ml_weight: f64 }   // src/cmd/model.rs
impl CmdModel {
    /// Reads dir/cmd_model.txt (LightGBM's text format, into a `Forest`) and dir/cmd_model_stats.json.
    pub(super) fn load(dir: &Path) -> Option<CmdModel>;
    /// Raw scores, one per row of the row-major `flat` (rows = flat.len() / num_features);
    /// `Err` when `num_features` is not the model's input width.
    pub(super) fn predict(&self, flat: &[f64], num_features: usize) -> anyhow::Result<Vec<f64>>;
}
```

Loaded **by the worker thread**, once, at the same time as
`CommandIndex::load` ([02 section 6.2](02-browse.md#62-thread-body)): the
index load runs on a scoped thread while the worker thread loads the model. A
`TIMING {"op":"cmd_model_load","ms":..}` line is written only when a model was
loaded. It is never reloaded (ADR-11). Measured on the perf DB's model (132
trees, 238 KB): 3.8 ms p50, 5.3 ms p95 (read and parse), against an index
load of 28-29 / 32-34 ms, so it is off the critical path.
`load` returns `None` (ranking stays heuristic-only) unless all hold:

| Check | Failure log |
|---|---|
| `cmd_model.txt` and `cmd_model_stats.json` exist | none (normal before the first training) |
| The stats parse | `log::warn!("command model stats unreadable; using the heuristic")` |
| The model text reads and `Forest::parse` accepts it (below) | `log::warn!("command model unreadable; using the heuristic")` |
| The model file's `feature_names` (which also fixes its width) **and** `stats.feature_names` both equal `command_feature_names()`, in order (ADR-15) | `log::info!("command model was trained with a different feature set; using the heuristic until the next retrain")` |

`ml_weight` is computed once at load (section 7.3).

### 7.2 `rank_commands` with a model

Signature from this phase on (the Phase B body is steps 1-2). `CommandRanking`
gains the field `pub model_failed: bool` (false in every case but step 5's
error), `pub features: Vec<f64>` and `pub feature_rows: Vec<u32>`: the
features of the model-scored rows, row-major in the order step 4 computed
them, and for each rank `0..stage2` the row of `features` that is its; read
through `features_of(rank) -> &[f64]` (empty for a tail row). `CommandScore`
has no `features` field: one buffer instead of 2,000 vectors per keystroke,
which cost 0.3-0.5 ms to allocate and free, and no copy into rank order; the
worker copies a row's slice into `CmdRow.features` only for the rows it sends.

```rust
pub(super) fn rank_commands(query: &str, candidates: &[CommandCandidate], index: &CommandIndex,
                            ctx: &RankContext, head: usize, model: Option<&CmdModel>) -> CommandRanking;
```

`head` is Phase B's ([02 section 7](02-browse.md#7-heuristic-ranking)): only
`scores[..sorted]` are in rank order. With a model, step 1 selects the top
`max(head, k)` rows as a set and sorts only the first `head` of them (stage 2
re-sorts its `k` rows itself); without one it sorts `head`, as in Phase B.
After step 7 `sorted = max(head, k)`; on a failed prediction it stays `head`
and `sort_rest` orders everything after it.

| Step | Action |
|---|---|
| 1 | Heuristic for every candidate and the total order of [02 section 7](02-browse.md#7-heuristic-ranking): `select_nth` at `max(head, k)` (with a model), then `select_nth` and a sort of the first `head`. Call the result `H` |
| 2 | `model` is `None`: return `H` (`ml_score = None`, `ml_weight = 0.0`, `features` empty, stage times `None`, `stage2 = 0`) |
| 3 | `k = min(CMD_STAGE2_K, H.len())`. Stage 2 is `H[..k]`; the tail `H[k..]` is final as it is |
| 4 | Features for the `k` rows into one preallocated flat row-major `Vec<f64>` of `k * F` values, `par_chunks_mut(F)` zipped with the rows, each row written by `compute_all`. `query_lower` is computed once before the loop. Sequential when `k < 256` |
| 5 | `stage2.predict(&flat, F)`. For a real model, `CmdModel::predict` walks the `Forest` once per row, on the rayon pool from `PAR_PREDICT_MIN` (256) rows, sequentially below; the scores are LightGBM's raw scores bit for bit (below). `ml = crate::ranker::logistic(raw)` per row. An `Err`, or a result whose length is not `k`: return `H` as in step 2 with `CommandRanking.model_failed = true` |
| 6 | `score = (1 - w) * heuristic + w * ml` with `w = model.ml_weight`; store `ml_score = Some(ml)`, `ml_weight = w` |
| 7 | Sort `H[..k]` by the same total order (`score` desc, `last_run_at` desc, `db_id` desc): 32-byte sort keys as in step 1, then one in-place permutation of the `k` scores; `feature_rows` records where each rank's features are |
| 8 | Result = sorted stage 2 followed by the untouched tail. Timings: `features_ms`, `predict_ms`, `blend_ms` (steps 6-7) as `Some`, `stage2 = k` |

Why step 5 is not a LightGBM call. LightGBM still trains the model and
writes `cmd_model.txt`; serving reads that text into a `Forest` (in
`model.rs`) and walks its trees in Rust. Measured on the perf DB's model (132
trees, 238 KB):

| Serving with | Load | Predict 2,000 rows, p50 / p95 | B1 p95 (3 runs) | Why not |
|---|---|---|---|---|
| `booster.predict_with_params(.., "num_threads=8")`, as specified first | 13-19 ms | 8.5 / 14.5 ms | 19-20 ms | The `lightgbm3-sys` build has OpenMP off (`USE_OPENMP=OFF`; `Cargo.toml` is not edited): one thread, about 4 us a row |
| One booster, chunks predicted on the rayon pool through an `unsafe impl Sync` wrapper | 13-17 ms | 1.2 / 2.2 ms | 6.6-8.3 ms | `unsafe` (file mode removed its last such wrapper), and a data race: LightGBM's `Booster::Predict` takes a shared lock, but the predictor it builds per call writes `GBDT::InitPredict`'s fields unlocked |
| A pool of dedicated threads, a booster each, fed over channels | 9-17 ms | 2.0-3.0 / 3.2-4.9 ms | 8.9-11 ms | Its threads sleep between keystrokes and wake cold |
| A booster per rayon thread in a thread-local, parsed by `rayon::broadcast` | 16 ms, 24 parses | 1.7 / 2.4 ms | 8.0-8.8 ms | Over B1; the 24 parses slowed the index load beside them (B3 p95 55-60 ms) |
| **`Forest`** (chosen) | 3.8 ms | 1.1 / 2.0 ms | 6.8-7.8 ms | - |

`Forest::parse` accepts only what command training produces and refuses the
rest (the model is then unreadable, E-L-06): LightGBM `version=v4`,
`num_class=1`, `num_tree_per_iteration=1`, no `average_output` line (LightGBM
writes it bare, without `=`), every tree
`num_cat=0` and `is_linear=0`, no categorical split bit, every list as long as
`num_leaves` says, every split feature below `max_feature_idx + 1`, a text
that reaches `end of trees`, and every internal child numbered above its
parent (LightGBM numbers nodes in split order), so a walk always ends. The walk
is `GBDT::PredictRaw` with `Tree::NumericalDecision` of LightGBM 4.7.0, after
LightGBM's dense-row input conversion: a value within `1e-35f` of 0 is 0
(the conversion drops it), and NaN becomes 0 unless the split's missing type
is NaN; a Zero-type split sends
values within `1e-35f` of 0, a NaN-type split NaN, to the default side
(`decision_type` bit 1); otherwise `value <= threshold` goes left; a one-leaf
tree is its leaf value; the trees are summed in order from 0.0. Tests compare
its scores with LightGBM's bit for bit, on the fixture model and on that model
rewritten to use all six numerical decision types or thresholds inside the
zero threshold, over rows with NaN, zeros and values either side of it.

`rank_commands` is a thin wrapper over the testable core:

```rust
pub(super) struct Stage2<'a> { pub k: usize, pub ml_weight: f64,
    pub predict: &'a dyn Fn(&[f64], usize) -> anyhow::Result<Vec<f64>> }   // (flat features, F) -> one raw score per row
pub(super) fn rank_with(query: &str, candidates: &[CommandCandidate], index: &CommandIndex,
                        ctx: &RankContext, head: usize, stage2: Option<Stage2<'_>>) -> CommandRanking;
```

`rank_commands` passes `Stage2 { k: CMD_STAGE2_K, ml_weight: model.ml_weight, predict: &|flat, f| model.predict(flat, f) }`
(in the code the predictor type is the alias `rank::Predict<'a>`).
Tests pass a stub `predict`; the two-stage check passes `k = usize::MAX`.

A row of the tail is never promoted above a stage-2 row, whatever its
heuristic value. Metric B8 bounds what that costs
([04 section 6.5](04-bench.md#65-two-stage-check-b8)). Prediction failure:
section 11, E-L-07.

### 7.3 Blend

`ml_weight = model::ml_weight(blend_examples)` = `crate::ranker::Ranker::compute_blend_weights((stats.blend_examples / CMD_BLEND_SCALE).round() as usize).1`
- the file formula `(1 + tanh(x/15 - 2)) / 2` ([src/ranker.rs:465-474](../../src/ranker.rs))
applied to scaled weighted group counts (ADR-14).

| `blend_examples` | `x` | `ml_weight` |
|---|---|---|
| 0 | 0 | 0.018 |
| 150 | 15 | 0.119 |
| 300 | 30 | 0.500 |
| 450 | 45 | 0.881 |
| 600 | 60 | 0.982 |

The heuristic it blends with is fzf's order expressed as a value in (0, 1]
([02 section 7.2](02-browse.md#72-types-and-the-value)): the fzf score
rescaled over the candidates' range, plus a recency term in (0, 1] that only
breaks ties. With a small `ml_weight` (a new install) the model can reorder
commands of equal fzf score but rarely overrides a better match; at full
weight the model decides alone. Stage 1 (step 1 of section 7.2) is therefore
fzf's order: the model scores fzf's top `CMD_STAGE2_K`.

`blend_examples` examples: 300 hook groups; or 75 pick groups; or 1,000
imported groups. A fresh install that imported 8,000 or more eligible runs
reaches full model weight at its first training; the `import` slice
([04 section 5.4](04-bench.md#54-slices)) reports model minus recency top-1 for
exactly that case (reported beside the verdicts; R2 itself covers `all` and `gt10min`).

### 7.4 What the debug pane shows

Added below the heuristic lines of [02 section 8.3](02-browse.md#83-layout-and-rows):
one line per registry feature `name = value` (`NaN` printed as `n/a`, a binary
feature as `0`/`1`, a numeric one to 4 decimals) for rows with non-empty
`features`, or, when a model is in use, `not model-scored (rank beyond stage 2)`
for tail rows. Then, whether or not a row is selected, `model: none` without a
model, or `model: <best_iteration> trees, <groups> groups, ml weight <w>`,
`trained <trained_at>`, and the top 3 of `feature_gain`.
`CmdResponse::Loaded` gains a field `model: Option<CmdModelStats>` that the
UI stores for this (`CmdApp.model`); `CmdResponse::ModelDropped` (sent once,
after E-L-07) clears it.

## 8. Retraining trigger

Training never runs in the picker process (ADR-10).

| Trigger | Mechanism |
|---|---|
| `psychic history --retrain [--quiet]` | `train::retrain(data_dir, CMD_TRAIN_SEED, quiet)` in the foreground. `AlreadyRunning` prints `Another training run is in progress.` (nothing when `--quiet`) and exits 0 |
| Picker start | After the first accepted `QueryUpdated`, the UI thread spawns one short-lived thread running `train::maybe_spawn_retrain(data_dir, SystemTime::now())` (a wrapper reading the environment over `start_retrain_if_due(data_dir, now, disabled, current_exe)`, which tests call). The thread waits for the child, so a child that ends while the picker still runs is reaped; when the picker exits first the child carries on alone |

`maybe_spawn_retrain`: return at once when the environment variable
`PSYCHIC_NO_RETRAIN` is set (benchmarks and tests set it). Otherwise return unless (`cmd_train_attempt` is missing, or its
mtime is at least `CMD_RETRAIN_MIN_INTERVAL_SECS` old) and (`cmd_train.lock` is
missing or stale). Failed and `NotEnoughData` attempts therefore back off like
successful ones. Then spawn, without waiting:

```rust
Command::new(std::env::current_exe()?)
    .arg("--data-dir").arg(data_dir).args(["history", "--retrain", "--quiet"])
    .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
    .process_group(0)          // std::os::unix::process::CommandExt: survives the picker and the terminal's SIGINT
    .spawn()
```

The new model is used by the **next** picker start. The first picker start on
an install with imported history therefore ranks by heuristic and starts the
first training.

A model trained with another feature list (an older release: the registry
grew) is refused by `CmdModel::load` (section 7.1), so ranking falls back to
the heuristic. The retrain is then due at once, whatever the interval:
`retrain_due` also holds when `cmd_model_stats.json` parses and its
`feature_names` differ from the registry, unless `cmd_train_attempt` is newer
than that file (an attempt since then found too little to train; it backs
off by the interval as usual). Test:
`a_model_with_an_older_feature_list_is_refused_and_retrained_at_once`.

## 9. Anti-patterns (DO NOT)

| Don't | Do instead | Why |
|---|---|---|
| Compute training features with code other than `features::compute_all` over a `CommandIndex` | One feature path, two fill paths (ADR-12) | Train/serve skew is silent and is the usual cause of "the model is worse in production" |
| Apply a run to the index before building its group | Group first, `apply_run` second | Label leakage: the answer would be "run 0 seconds ago" |
| Insert, reorder or delete a row of `COMMAND_FEATURE_REGISTRY` | Append; replace in place with a new name | Position is the model column |
| Make the registry a `Lazy`, or resolve anything inside `compute` | A `static` slice; inputs resolved before the loop | Parallel first-use cliff ([docs/how-it-works.md:1205-1207](../how-it-works.md)) |
| Return `Result` from `compute`, or use `0.0` for an unknown success rate | Infallible; `f64::NAN` for missing | A `Result` in the rayon loop can only be unwrapped; 0.0 would read as "always fails" |
| Emit all four query lengths for every run | One length per run by seeded hash (ADR-13) | 4x rows; B6 |
| Use `rand::thread_rng`, `StdRng` or hash-map iteration order in sampling | `splitmix64` on `(seed, run.id)`; rows ordered by idx | Training must be reproducible for the benchmark's seed comparison |
| Put the positive first in its group | Order rows by idx | Tied scores would resolve in the positive's favour and flatter NDCG |
| Train in a thread of the picker | Detached child (section 8) | The picker lives for seconds |
| Reload `cmd_model.txt` while the picker runs | Load once at worker start | The list must not reorder without input |
| Load a model whose feature names differ, or with missing stats | `CmdModel::load` returns `None` | A repointed column ranks confidently and wrongly |
| Write `cmd_model.txt` in place with `fs::write` | Write beside it and rename (step 6) | A picker starting mid-write would read half a file |
| Change a LightGBM parameter, the half-life or the split for the command model inside `src/training/mod.rs` | Keep one `fit_seeded`; a command-specific parameter becomes an explicit argument added in its own commit with benchmark numbers | The file model must stay byte-identical to `train.py` |
| Copy the blend formula into `src/cmd/` | Call `Ranker::compute_blend_weights` | "Sharing the blend maths" must stay true when upstream retunes it |
| Promote tail rows past stage-2 rows, or run the model on all matches "when there is time" | The fixed cut of section 7.2 | B1 at 50,000 candidates; determinism |
| Count picker impressions without a pick as negatives | Drop them | No positive in the group; LambdaRank learns nothing and the rows only cost time |
| Print or log a query or command while training | Counts and feature names only | [01 section 10.6](01-record.md#106-no-command-text-outside-eventsdb) |

## 10. Test case specifications

### 10.1 Unit tests

Fixture `F1`: built with the `CommandIndex` mutators; `now = 1_000_000`; command
`git status` (idx 0) with runs at `now-10` (dir A, exit 0, prev template `cd PATH`),
`now-4000` (dir A, exit 1), `now-90000` (dir B, exit 0, prev `cd PATH`),
`now-700000` (dir B, exit NULL); `ctx.dir = A`, `ctx.repo = None`,
`ctx.prev_templates = [cd PATH, None, None]`.

| ID | Component | Input | Expected | Edge cases covered |
|---|---|---|---|---|
| TC-L-01 | every feature on `F1`, query `gs`, fuzzy 57 | `compute_all` | `[57, 0, 0, 0, 0, ln(11)=2.3979, 1, 2, 3, 4, ln(5)=1.6094, 0, 0.5, 0, 2.3979, 2, 0.5, 0.5, 0.6667, 0, 0, 10, 1, 51, 3]` (+-1e-4; `fzf_score` 51 = 16 + 2*8 for `g` at the start, -3-1-1 for the gap, 16 + 8 for `s` at a word start; `recency_rank` 3 as the fixture passes it) | `word_starts_with_query` is 0 for `gs`; `last_exit_nonzero` is 0 because the newest run exited 0 |
| TC-L-02 | query features | queries `""`, `git`, `st`, `GIT` | features 1-3: `[0,0,0]`, `[f,1,1]`, `[f,0,1]`, `[f,1,1]` (`f` = the fuzzy score passed in) | case-insensitive prefix; empty query |
| TC-L-03 | unknowns | An entry with `run_count 1`, exit NULL, `ctx.dir = None`, `prev_templates` all `None` | `success_rate` is NaN; features 13-18 are `0, 0, 19.57, 0, 0, 0` | - |
| TC-L-04 | `failed_within_2_minutes` | `last_fail_at` = `now-119`, `now-120`, `0`, `now+5` | `1, 0, 0, 0` | boundary and clock skew |
| TC-L-05 | `follows_recent3_share` | `follower_runs` 2 for template X and 1 for Y, `run_count 6`, `prev_templates = [X, Y, X]` | `0.5` | the duplicate X is counted once |
| TC-L-06 | registry shape | `COMMAND_FEATURE_REGISTRY` | 25 entries; names unique; the names in the order of section 4; `command_feature_specs()` monotone column equals `[1,0,0,1,1,-1,1,1,1,1,1,1,1,1,-1,1,1,1,0,0,0,0,0,1,0]` | - |
| TC-L-07 | `splitmix64` | `0`, `1` | `0xE220A8397B1DCDAF`, `0x910A2DEC89025CC1` | published SplitMix64 vectors |
| TC-L-08 | query-length mix | Run ids `1..=10_000`, seed 42 | Shares of lengths 0/1/2/3 within 40/20/20/20 +-2 percentage points; the same ids and seed give the same lengths twice; seed 43 changes at least one | text shorter than the length -> whole text |
| TC-L-09 | no leakage | Commands A, B; timeline A, B, A (`build_run_group` for the third run, query `""`) | In that group the positive's `runs_last_hour` counts 1 run (the first A), and `log_seconds_since_last_run` is the gap to the first A, not 0 | - |
| TC-L-10 | first-ever run | Timeline A, B | No group for either run; `counts.hook == 0` | - |
| TC-L-11 | negatives (`build_run_group`) | 40 commands run once each, then command 0 again, query `""` | 31 rows: 1 positive and 30 random distinct others (no fixed recent ones since 2026-11-17), older commands among them; rows in ascending idx; labels sum to 1 | With query `zz` matching nothing else -> no group |
| TC-L-19 | `training_query` | 4,000 run ids, text `git commit --amend` | Length as `query_length`; of the non-empty ones `CMD_TRAIN_FRAGMENT_SHARE`% (+-3) are the start of `commit` or `amend`, the rest prefixes | A one-word text: always a prefix |
| TC-L-20 | old model on disk | Stats whose `feature_names` are the first 23 of the registry | `CmdModel::load` is `None` (heuristic ranking); `retrain_due` holds although the last attempt is a minute old; an attempt newer than the stats backs off; current names wait for the interval | - |
| TC-L-12 | weights and cap | 5 eligible hook runs, 5 eligible import runs, `max_run_groups = 4` | 4 groups, built from the 4 newest eligible runs; `row_weights` are 1.0 or 0.3 by source | - |
| TC-L-13 | pick group | Impressions (q `g`: A, B, C), (q `gi`: A, B), pick A | 5 rows, labels `[1,0,0,1,0]`, weight 4.0, every row's timestamp = the pick's; afterwards `picks_in_window(A) == 1` | An episode with no impression of the picked command -> no group; impressions without a pick -> no group |
| TC-L-14 | `from_parts` asserts | Unsorted `episodes`; a zero weight; `features.len()` mismatch | Each panics with its assert message | - |
| TC-L-15 | blend table | `blend_examples` 0, 150, 300, 450, 600 | `ml_weight` 0.018, 0.119, 0.500, 0.881, 0.982 (+-1e-3) | - |
| TC-L-16 | stats file | A `CmdModelStats` value | Serialises and parses back equal; `blend_examples` = `pick*4 + hook*1 + import*0.3` | - |
| TC-L-17 | two-stage order | 6 candidates, `rank_with` with `head = usize::MAX` and `Stage2 { k: 3, ml_weight: 1.0, predict: <stub returning raw scores that reverse the input order> }` | First 3 rows are the heuristic top 3 in reversed order; last 3 are heuristic ranks 4-6 unchanged with `ml_score = None` | `k >= len` -> all rows model-scored |
| TC-L-18 | pick then run | Timeline: runs A (`git log`), B (`log git`: any query of one matches the other); pick of A in shell session `s` at t; run of A in `s` at t+5; run of A in `s` at t+400; run of A in session `u` at t+6 | The t+5 run is not eligible; the t+400 run and the session-`u` run are | - |

### 10.2 Integration tests

| ID | Flow | Setup | Verification | Teardown |
|---|---|---|---|---|
| IT-L-01 | Train/serve parity | Temp data dir; `record_run` 60 runs over 12 commands, 3 dirs, 2 repos, 3 sessions, mixed exit codes, all within 20 days; 4 pick episodes through `CmdEvents` | For a fixed `(now, cwd, repo_root, session)`: feature vectors of every command from `CommandIndex::load` + `compute_all` equal those from a `Replayer` that applied everything, for queries `""` and `g` (exact `f64` equality, NaN equal to NaN) | `remove_dir_all` |
| IT-L-02 | Retrain end to end | Temp data dir filled by the synthetic generator ([04 section 4.3](04-bench.md#43-corpus-3-synthetic-generator)) with 300 commands / 4,000 runs, seed 1 | `retrain` returns `Trained`; both files exist with mode 600; `feature_names` equals the registry; `CmdModel::load` is `Some`; a second `retrain` with the same seed writes a byte-identical `cmd_model.txt`; no `cmd_train.lock` remains | `remove_dir_all` |
| IT-L-03 | Not enough data | Temp data dir with 10 runs | `NotEnoughData { groups }` with `groups < 50`; no model file; lock removed | `remove_dir_all` |
| IT-L-04 | Stale or foreign model is refused | Valid model + stats, then edit `feature_names[0]` in the JSON; separately, delete the stats file; separately, truncate `cmd_model.txt` | `CmdModel::load` is `None` in all three; the worker still answers `UpdateQuery` with heuristic rows | `remove_dir_all` |
| IT-L-05 | Lock | Create a fresh `cmd_train.lock` by hand, call `retrain`; then set its mtime 1,000 s back and call again | First `AlreadyRunning`; second trains (or `NotEnoughData`) and removes the lock | `remove_dir_all` |
| IT-L-06 | File model untouched | The existing `training` tests plus `fit(set, features)` versus `fit_seeded(set, features, 42)` on the CSV of `test_from_csv_drops_unclicked_episodes_and_sorts_stably` enlarged to 40 episodes | Identical `model` strings | none |
| IT-L-07 | Model changes the ranking only inside stage 2 | IT-L-02's data and model; worker with and without the model; 20 queries | Same set of matched ids both ways; with the model, rows `0..k` carry `ml_score` and every registry feature; no TIMING or log line contains a command text | `remove_dir_all` |

Where they live: TC-L-01..06, E-L-08 and E-L-13 in `src/cmd/features.rs`;
TC-L-07..14, 16, 18, IT-L-01..06 and E-L-02, 03, 09, 10, 11, 12, 15 in
`src/cmd/train/tests.rs` (E-L-05's stale lock is IT-L-05; the lock's token
and heartbeat, and `PSYCHIC_NO_RETRAIN`, have tests there too); TC-L-15, the
load checks of 7.1, `Forest` parity with LightGBM and its refusals in
`src/cmd/model.rs`; TC-L-17 and the ranking half of E-L-07 in
`src/cmd/rank.rs`; IT-L-07, the worker half of IT-L-04 (all three cases) and
of E-L-07 in `src/cmd/worker.rs`; `ModelDropped` in `src/cmd/app.rs`.
IT-L-07's "no log line contains a command text" is checked on the built
binary by `tests/no_text_leaks.rs` (`--retrain`, `--retrain --quiet`,
`--status`; every file in the data dir but the database); the TIMING line
holds numbers only by construction (TC-B-17).

## 11. Error handling matrix

| # | Failure | Detection | Response | Fallback | Logging |
|---|---|---|---|---|---|
| E-L-01 | Fewer than `CMD_TRAIN_MIN_GROUPS` groups | Section 6.2 step 4 | `NotEnoughData` | Existing model (if any) stays; else heuristic | info line of section 6.2 |
| E-L-02 | `time_split` cannot give every part rows | `Err` starting `every split needs rows` | `NotEnoughData` | As E-L-01 | info |
| E-L-03 | LightGBM error during fit | Other `Err` from `fit_seeded` | `--retrain`: `Err` to `main`, exit 1. `--quiet`: same, plus the message appended to `training.log` | Previous model files untouched (nothing written before step 6) | `log::error!` |
| E-L-04 | Another training holds the lock | Section 6.2 step 1 | `AlreadyRunning`, exit 0 | - | `log::info!` |
| E-L-05 | Training process killed | Lock file left behind, its mtime no longer refreshed | Next attempt after `CMD_TRAIN_LOCK_STALE_SECS` removes it (if it still holds the token it was judged by) | Files `*.<pid>.pending` beside the targets are orphaned and harmless; the next successful write does not reuse them (pid in the name) | None |
| E-L-06 | Model file missing, unreadable, or feature mismatch at picker start | Section 7.1 | `CmdModel::load` -> `None` | Heuristic-only ranking | Per the table in 7.1 |
| E-L-07 | `CmdModel::predict` returns `Err` (rows not as wide as the model) or a different number of scores than `k` | `Err` / length check | That query is answered with `H` (heuristic order, `ml_score = None`) and `CommandRanking.model_failed = true`; the worker then sets its `Option<CmdModel>` to `None` for the rest of the process and sends `CmdResponse::ModelDropped`, so the debug pane says `model: none` | Heuristic-only | `log::warn!("command model prediction failed; using the heuristic")` once |
| E-L-08 | A feature computes NaN other than `success_rate`, or an infinity | `assert!(value.is_finite() \|\| name == "success_rate")` in `compute_all` | Panic (a bug; [llm.md:3](../../llm.md)) | - | panic message names the feature, not the command |
| E-L-09 | `current_exe()` fails or the detached spawn fails | `Err` in `maybe_spawn_retrain` | Ignored | No retrain this time | `log::warn!` |
| E-L-10 | Stats written but model write failed (disk full between the two) | Cannot happen in that order; model is written first. Model written, stats write fails | `Err`; old stats + new model on disk | Next load: names still equal -> loads with the old `blend_examples`; next successful training repairs it | `log::error!` |
| E-L-11 | Event row references a deleted command | `replayer.knows` is `None` | Row dropped | - | None |
| E-L-12 | Run with `started_at` earlier than its predecessor in id order | T3 orders by `started_at, id` | Replayed in time order | - | None |
| E-L-13 | Clock skew puts a run in the future of `now` at serving time | `now - last_run_at < 0` | `max(0, ..)` in features 6 and 15; excluded from windows (`t <= now`) | - | None |
| E-L-14 | `train_parity.py` reports a difference after section 6.1 | Script output | The refactor is wrong; fix it before committing | - | - |
| E-L-15 | History deleted (`--purge`, or a delete in the picker) while a training runs on what it read before | `CmdStore::counts` lower than at step 2, before the renames or after them | Before: `HistoryShrank`, the new files dropped, the previous model kept. After (a purge between the check and the renames): the model files removed, as the purge does. Exit 0 | The next training (an hour later) uses what is left | `log::info!`; the report says `History was deleted while training; no model written.` |

## 12. References

| Topic | Location |
|---|---|
| Targets R1, R2, B1, B6, B8; ADR-10..ADR-15 | [00-blueprint.md section 2](00-blueprint.md#2-success-metrics), [section 8](00-blueprint.md#8-architecture-decision-records) |
| T2, T3, T4 | [01-record.md section 6.4](01-record.md#64-events-delete-purge-import-status) |
| `prev_command_id` and the gap rule | [01-record.md section 6.1](01-record.md#61-write-path-s) |
| `CommandIndex`, `RankContext`, accessors | [02-browse.md section 5](02-browse.md#5-commandindex) |
| Heuristic stage and total order | [02-browse.md section 7](02-browse.md#7-heuristic-ranking) |
| How pick episodes are logged | [02-browse.md section 10](02-browse.md#10-event-logging) |
| Benchmark use of `build_training_set` and `Replayer` | [04-bench.md section 5.2](04-bench.md#52-protocol) |
| File feature trait and registry (the pattern) | [src/feature_defs/schema.rs:70-91](../../src/feature_defs/schema.rs), [src/feature_defs/registry.rs:14-40](../../src/feature_defs/registry.rs) |
| File "never" constant and log-seconds | [src/feature_defs/implementations.rs:533-553](../../src/feature_defs/implementations.rs) |
| File no-leakage accumulator | [src/features.rs:44-65](../../src/features.rs) |
| `fit`, weights, split, early stopping | [src/training/mod.rs:369-444](../../src/training/mod.rs), [:203-213](../../src/training/mod.rs), [:252-273](../../src/training/mod.rs), [:293-326](../../src/training/mod.rs) |
| LightGBM parameters | [src/training/mod.rs:177-199](../../src/training/mod.rs) |
| Atomic write | [src/training/mod.rs:540-554](../../src/training/mod.rs) |
| File fits-registry check | [src/ranker.rs:142-144](../../src/ranker.rs) |
| File predict, logistic, blend | [src/ranker.rs:598-604](../../src/ranker.rs), [:707-709](../../src/ranker.rs), [:465-474](../../src/ranker.rs), [:616-637](../../src/ranker.rs) |
| File retrain and training log format | [src/ranker.rs:835-910](../../src/ranker.rs) |
| Training-parity rule | [llm.md:77-88](../../llm.md) |
