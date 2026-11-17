# Phase D: Bench and iterate (Implementation)

Document type: **Implementation** (HOW). Strategy and targets:
[00-blueprint.md](00-blueprint.md). This document is the single source for how
every metric of [00 section 2](00-blueprint.md#2-success-metrics) is measured,
the three corpora, the benchmark commands, the iteration loop and the results log.
Numbers for the targets themselves are in 00 only; here they are cited by ID.

## 1. Scope, inputs, done when

| | |
|---|---|
| Builds | `psychic internal bench-history` (Rust), `bench/history.py` (Python, pty), `docs/ctrl-r/results.md` |
| Inputs (must exist) | D0 needs Phase A only. D needs Phases A, B, C |
| Output | A results-log row per corpus showing R1-R3 and B1-B9 with PASS/FAIL, then the tuning loop of section 7 |

Done when all of these pass (D0: the first three lines and `generate`,
`extract-agent`, `stats`; D: everything):

```bash
just build && cargo test && cargo clippy
cargo test cmd::corpus && cargo test cmd::bench
P="cargo run --release --"; W=/tmp/psychic-bench
$P --data-dir $W/cmd-synth-small internal bench-history generate --commands 2000 --runs 20000 --seed 1
$P --data-dir $W/cmd-synth-small internal bench-history stats
$P --data-dir $W/cmd-agent internal bench-history extract-agent && $P --data-dir $W/cmd-agent internal bench-history stats
$P --data-dir $W/cmd-real history --import && $P --data-dir $W/cmd-real internal bench-history stats   # real $HOME: the import reads its files
for C in cmd-real cmd-agent cmd-synth-small; do
  $P --data-dir $W/$C internal bench-history relevance --seeds 5 --folds --json $W/$C/relevance.json --md $W/$C/relevance.md
done
./bench/history.py setup          # builds the perf DB of 00 section 2 in $W/cmd-perf, trains and pins its model
./bench/history.py all            # B1-B4, B6, B7, B8; writes $W/cmd-results.json and prints the table of section 6.7
./bench/run.py setup HEAD~1 && ./bench/run.py startup 6 && ./bench/run.py keystroke 30   # file mode unchanged (B9)
ls ~/.local/share/psychic -la     # mtimes unchanged by any command above
```

## 2. Files touched and the two commits

| Commit | New files | Upstream edits |
|---|---|---|
| **D0** (after Phase A; developed in parallel with B, committed **before** B, which rebases onto it) | `src/cmd/corpus.rs`; `src/cmd/bench.rs` with `BenchArgs` and the `Generate`, `ExtractAgent`, `Stats` variants and arms (no stubs for the rest) | [src/cli.rs:121-157](../../src/cli.rs): add `InternalCommands::BenchHistory(crate::cmd::bench::BenchArgs)` with the doc comment `/// Benchmark shell history search: build corpora, measure relevance and latency`. [src/main.rs:500](../../src/main.rs) (the `Commands::Internal` match): add `InternalCommands::BenchHistory(args) => return cmd::run_bench(&data_dir, cli.data_dir.is_some(), args),` |
| **D** (after Phase C) | The `Relevance`, `Twostage`, `Latency` variants of section 3 and their arms in `src/cmd/bench.rs`; `bench/history.py`; `docs/ctrl-r/results.md`; one test added to `src/cmd/render.rs` (TC-N-10) | `docs/how-it-works.md`: add the command benchmarks to "The two benchmark harnesses" ([docs/how-it-works.md:2906](../how-it-works.md)) |

`bench/harness.py`, `bench/run.py` and `bench/model.py` are imported or run, never edited (ADR-19).

```rust
// src/cmd/mod.rs
pub fn run_bench(data_dir: &Path, data_dir_was_given: bool, args: bench::BenchArgs) -> anyhow::Result<()>;
```

**Data-dir guard**, first statement of `run_bench`: `bail!("bench-history needs
an explicit --data-dir that is not your real data directory")` unless
`data_dir_was_given` and `data_dir != cli::get_default_data_dir()` (compared
after `canonicalize` when both exist, else after `std::path::absolute`). Benchmarks and tests never write to
`~/.local/share/psychic`.

## 3. CLI surface

`psychic --data-dir DIR internal bench-history <SUBCOMMAND>`. D0 builds the
first three variants; D adds the other three.

```rust
#[derive(clap::Args, Debug)] pub struct BenchArgs { #[command(subcommand)] pub command: BenchCommand }
#[derive(clap::Subcommand, Debug)]
pub enum BenchCommand {
    /// Write a seeded synthetic history into --data-dir
    Generate { #[arg(long)] commands: usize, #[arg(long)] runs: usize, #[arg(long, default_value_t = 1)] seed: u64,
               #[arg(long)] anchor: Option<i64> },
    /// Import Bash tool calls from Claude Code transcripts into --data-dir
    ExtractAgent { #[arg(long)] projects_dir: Option<PathBuf> },
    /// Describe the history in --data-dir
    Stats,
    /// Replay the history and score three rankers
    Relevance { #[arg(long, default_value_t = 5)] seeds: u64, #[arg(long, default_value_t = 2000)] max_test_runs: usize,
                #[arg(long)] folds: bool, #[arg(long)] json: Option<PathBuf>, #[arg(long)] md: Option<PathBuf>,
                #[arg(long, default_value = "")] label: String, #[arg(long)] strict: bool },
    /// Compare two-stage ranking with full scoring
    Twostage { #[arg(long, default_value_t = 400)] queries: usize },
    /// Time filter+rank without a terminal
    Latency { #[arg(long, default_value_t = 300)] keystrokes: usize },
}
```

| Subcommand | Reads | Writes | Section |
|---|---|---|---|
| `generate` | nothing | `--data-dir/events.db` (must hold no commands; else `bail!("--data-dir already holds commands")`) | 4.3 |
| `extract-agent` | `<projects dir>/**/*.jsonl`, read-only | `--data-dir/events.db` (same emptiness rule) | 4.2 |
| `stats` | `--data-dir/events.db` | stdout | 4.4 |
| `relevance` | `--data-dir/events.db` | stdout, `--json`, `--md`, model files under `--data-dir/bench/` | 5 |
| `twostage` | `--data-dir/events.db`, `cmd_model.txt` | stdout | 6.5 |
| `latency` | same | stdout | 6.6 |

## 4. Corpora

| # | Name | Label printed on every result | Built by | Role |
|---|---|---|---|---|
| 1 | Real shell history | `human, shell history import` | `psychic --data-dir $W/cmd-real history --import` (the production importer, [01 section 12](01-record.md#12-backfill)); reads `$HOME` files and an Atuin DB read-only | Decision corpus when large enough (section 7.2) |
| 2 | Claude Code Bash calls | `agent-run commands, not human` | `bench-history extract-agent` | Decision corpus otherwise; true cwd, timestamps, sessions |
| 3 | Synthetic | `synthetic` | `bench-history generate` | Scale, performance, regression guard. Never used alone to claim relevance |

All three live under `/tmp/psychic-bench/` (the `WORK` directory of
[bench/harness.py:37](../../bench/harness.py)), outside the repository and
outside the real data dir.

### 4.1 Corpus 1: real shell history

No new code. Caveats the report must print (from `stats`):

| Property | Consequence |
|---|---|
| Rows from files have `exit_code` NULL and a cwd inferred by replay ([01 section 12.2](01-record.md#122-changes-to-srcimport)) | Outcome features are missing; directory features are approximate |
| Rows whose `shell_session` starts with `import-synth:` have synthetic 1-second timestamps | Excluded from every time-based slice (5.4) |
| Atuin rows have true cwd and exit codes | Reported separately in `stats` (`with cwd`, `with exit code` shares) |

### 4.2 Corpus 2: agent-run commands

`src/cmd/corpus.rs`: `pub(super) fn extract_agent(projects_dir: &Path, store: &CmdStore) -> anyhow::Result<AgentReport>`.
Projects dir: `--projects-dir`, else `$CLAUDE_CONFIG_DIR/projects`, else
`~/.claude/projects` (the location of [src/import/sources.rs:413-417](../../src/import/sources.rs)).
Files: every `*.jsonl` at most 3 directories below it (session transcripts and
their sub-agent transcripts). Its own walker; `jsonl_files`
([src/import/sources.rs:376-391](../../src/import/sources.rs)) stays private.

Transcript format (verified on this machine against Claude Code transcripts of
2026-11): one JSON object per line.

| Line | Fields used |
|---|---|
| any line | top-level `cwd` (string, absent on some lines), `sessionId` (string), `agentId` (string, on sub-agent transcripts only; they carry their parent's `sessionId`), `timestamp` (RFC 3339 string) |
| `type == "assistant"` | `message.content[]` blocks with `type == "tool_use"`, `name == "Bash"`, `id`, `input.command` (string), `input.run_in_background` (optional bool) |
| `type == "user"` | `message.content[]` blocks with `type == "tool_result"`, `tool_use_id`, `is_error` (bool) |

Algorithm, per file, lines in order (a line that is not valid JSON is skipped;
lines containing none of `"Bash"`, `tool_result`, `"cwd"` are skipped before
parsing; several blocks on one line are handled in array order):

| Step | Rule |
|---|---|
| Track cwd | `last_cwd` = the most recent top-level `cwd` seen in this file |
| Tool use | For each Bash `tool_use` block (counted in `tool_uses`), in this order: skip when `run_in_background == true` (`skipped_background`), when `timestamp` is missing or unparseable (`str::parse::<jiff::Timestamp>`; `skipped_no_timestamp`), when `input.command` is not a string (`skipped_malformed`), or when the trimmed text fails `track::is_recordable(text, None)` (`skipped_filtered`). Otherwise keep `{ text: trimmed, cwd: line cwd or last_cwd, started_at: seconds, started_ms, session }` and remember `pending[id]` (a block without a string `id` is kept with no result). `session` = `sessionId` (else the file stem), plus `:<agentId>` when the line has one: parallel sub-agents of one session are separate shells, as two terminals would be |
| Tool result | For a `tool_result` whose `tool_use_id` is pending (then no longer pending): `exit_code = if is_error { 1 } else { 0 }`; `duration_ms = result timestamp - started_ms` when the result line has a timestamp and the difference is `>= 0` |
| End of file | Every pending entry is kept; without a result its `exit_code` and `duration_ms` are NULL |

Then all entries of all files are stably sorted by `(started_ms, file path, line number)`
(block order within a line is kept) and written inside one `CmdStore::transaction` with `record_run_in`: `source = Hook` (they carry true cwd
and outcome), `shell_session = "agent:<session>"`, `repo_root =
find_repo_root(cwd, home)` (memoised per cwd; `home` is `BenchEnv::home`), `template = templatize(text)`. The filters of
[01 section 9](01-record.md#9-track-command) steps 2-4 apply (empty, over
`CMD_MAX_TEXT_BYTES`, psychic's own commands); `PSYCHIC_HISTORY_IGNORE` does not.
Exit status is a boolean in the transcripts, so `exit_code` is 0 or 1 only.
`AgentReport { files, tool_uses, kept, skipped_background, skipped_no_timestamp, skipped_malformed, skipped_filtered, with_result }`
is printed as one `key: value` line each, in that order (`files: 194`,
`tool uses: 5821`, `skipped background: 53`, ...).

Measured on the authoring machine (2026-11-17): 194 files, 5,821 Bash uses,
5,724 kept, but **5,690 distinct texts**: an agent almost never types the same
command twice (mean text length 546 characters: heredocs, `cd X && ...`
chains, one-off pipelines), so the immediate-repeat share is 0.001 and only
about 0.6% of runs are reachable lookups (section 5.2). This corpus cannot
decide R1 on that machine either; see [README](README.md) residual item 10.

### 4.3 Corpus 3: synthetic generator

`pub(super) fn generate(store: &CmdStore, commands: usize, runs: usize, seed: u64, anchor: i64) -> anyhow::Result<()>`
(`anchor` = `--anchor`, else the current time). Asserts `1 <= commands <= runs`.
Deterministic for a given `(commands, runs, seed, anchor)`. One random stream:
`next() = { state = splitmix64(state); state }` starting from `state = seed`
(`splitmix64` as [03 section 5.2](03-learn.md#52-groups-from-executed-commands));
`u()` = `(next() >> 11) as f64 / 2^53` (in `[0, 1)`, never 1.0, so `p_new = 1` always takes the new branch); `below(n)` = `next() % n`.

World:

| Element | Definition |
|---|---|
| Directories | 40: `dirs[k * 5 + j] = /synth/repo<k>/dir<j>`, `k` in 0..8, `j` in 0..5; `repo_root` = `/synth/repo<k>` |
| Hash `h(salt, i)` | `splitmix64(splitmix64(seed.wrapping_add(salt)) ^ i)` |
| Command `i` (0-based; lower `i` = more popular) | Text `"{TOOLS[i % 40]} {SUBS[(i / 40) % 25]} {arg}"` where `arg` = `src/file{i}.rs` when `i % 3 == 0` else `arg{i}`. `TOOLS` = `["git","cargo","npm","docker","kubectl","make","ls","cat","grep","rg","ssh","curl","python","pytest","just","go","terraform","aws","vim","tar","rsync","scp","find","sed","awk","jq","node","yarn","pnpm","pip","uv","brew","systemctl","journalctl","psql","sqlite3","gh","helm","ansible","tmux"]`; `SUBS` = `["status","build","test","run","push","pull","log","diff","add","commit","checkout","fetch","install","update","list","show","apply","plan","deploy","exec","logs","start","stop","clean","check"]`. Every text is unique because `arg` contains `i`. `template = templatize(text)` |
| Home directory of `i` | `dirs[h(1, i) % 40]` when `h(2, i) % 10 < 7`; otherwise none (a "global" command) |
| Chain successor of `i` | `h(4, i) % (i + 1)` when `h(3, i) % 4 == 0`; otherwise none. (edit -> build -> test style chains: a successor's own successor continues the chain) |
| Zipf pick from a list of length `n` | `pos = ((((n + 1) as f64).powf(u())) as usize).saturating_sub(1).min(n - 1)` - an exponent-1 Zipf: `P(pos = k) = ln((k + 2) / (k + 1)) / ln(n + 1)`, so the first element is the most likely and every element is possible |

State: `seen` (commands `0..seen` have run), per-directory list of seen commands
homed there (in first-run order), current `dir` (initially `dirs[below(40)]`, drawn once before the loop),
current `session` (initially 0), the session's last 3 commands (the last 3
runs, newest first; duplicates allowed), `t` (seconds,
starts at 0), `retry: Option<i>` (initially `None`). Draws happen in the order
written, and only on the branch taken: the always-drawn ones are `g`, the
move `u()`, `x`, `c` (when choosing), the outcome `u()` and `below(5000)`; the
retry `u()` is drawn only when `retry` is `Some`; the repeat branch draws one
`u()` for its weighted pick only when the last-3 is non-empty; the chain
branch draws nothing; the directory branch draws one `u()` for its Zipf pick
only when the list is non-empty; the global pick (directly or as a
fall-through) draws one `u()`.

For each run `n` in `0..runs`:

| Step | Rule |
|---|---|
| Gap | `g = u()`. `g < 0.005`: new session (`session += 1`, `dir = dirs[below(40)]`, last-3 cleared), `t += 3600 + below(39_600)`. `g < 0.155`: `t += 300 + below(1500)`. Otherwise: `t += 5 + below(55)` |
| Move | `u() < 0.08`: `dir = dirs[below(40)]` |
| New? | `p_new = (commands - seen) / (runs - n)`; `x = u()` (always drawn). If `seen == 0` or `x < p_new`: command = `seen`; `seen += 1`; register it in its home directory's list. This makes exactly `commands` distinct commands by the end |
| Else choose | If `retry` is `Some(i)` and `u() < 0.5`: `i`. Otherwise `c = u()`: `c < 0.55` -> repeat: one of the session's last 3 with weights 0.80 / 0.12 / 0.08 (newest first; renormalised over those that exist). `c < 0.68` -> chain: the successor of the session's last command. `c < 0.87` -> directory: Zipf pick from the current directory's list. Else -> global: Zipf pick from `0..seen`. A branch that has nothing to offer falls through to global |
| Outcome | `exit_code = 1` when `u() < 0.10`, else 0; `retry = Some(command)` after a failure, else `None`. `duration_ms = 10 + below(5000)` |
| Record | Remember `(command, t, dir, session, exit_code, duration_ms)`; push the command on the session's last-3 |

Afterwards shift every `t` by `anchor - 60 - t_last` and write all runs in
order with `record_run_in` (`source = Hook`, `shell_session = "synth:<session>"`,
`cwd = dir`, `repo_root` of that dir), one `CmdStore::transaction` per 10,000 runs.

Resulting structure (asserted by TC-N-03 within tolerance): about half of all
runs are an immediate repeat of the session's previous command; mean gap about 300 s
(500,000 runs span about 4.8 years, so the 30-day window holds about 8,600
runs); `commands / runs` of the runs are a never-before-seen command. The
generator writes no `command_events` (section 12, E-N-10).

Measured at D0 (release build, `--seed 1 --anchor 1794873600`): 2,000 / 20,000
in 0.2 s, 4.3 MB after `VACUUM`, immediate-repeat share 0.471, span 68.2 days;
50,000 / 500,000 in 7.6 s, 120.7 MB as written and 111.4 MB after `VACUUM`
(B7 budget 300 MB; `command_runs` 33 MB plus seven run indexes 67 MB),
immediate-repeat share 0.459, span 1,758.8 days, 8,725 runs in the last 30 days.

### 4.4 `stats`

Prints one `key: value` line each: `commands`, `runs`, `runs by source`
(`hook <n>, import <n>`), `span days` (newest minus oldest `started_at`, one
decimal), `runs in last 30 days of the corpus` (`started_at > newest -
CMD_EVENT_WINDOW_SECS`), `with cwd`, `with exit code`,
`synthetic timestamps` (share of `import-synth:` rows), `sessions` (distinct
`shell_session`), `immediate repeat share` (runs whose command equals the previous run of the same
session), `first-ever share` (runs that are their command's first),
`pick episodes` (`pick` rows in `command_events`). Shares are of all runs, three
decimals, `0.000` on an empty history; counts are plain integers. No command text. Computed in memory from
`counts`, `all_runs`, `all_events` ([01 section 6.5](01-record.md#65-cmdstore-methods-and-row-types)); `bench.rs` and `corpus.rs` contain no SQL.

## 5. Relevance benchmark

`bench-history relevance`, in `src/cmd/bench.rs`. It calls the production
`train::build_training_set`, `training::fit_seeded`, `rank::rank_commands`,
`features::compute_all` and the production matcher (ADR-16). It contains no
feature or ranking logic of its own except the two baselines of 5.3.

### 5.1 Splits

`runs` = T3 order. `time_at(f)` = `started_at` of `runs[floor(f * runs.len())]`.

| Split | Train on timeline before | Test on runs with `started_at` in | When |
|---|---|---|---|
| `holdout` (headline) | `time_at(0.80)` | `[time_at(0.80), end]` | always |
| `fold60`, `fold70`, `fold80` | `time_at(0.60)`, `time_at(0.70)`, `time_at(0.80)` | `[time_at(s), time_at(s + 0.10))` | with `--folds` (the fold starts of [bench/model.py:53](../../bench/model.py)) |

### 5.2 Protocol

For each split, with `t_s` its training cut:

1. For each seed `s` in `1..=seeds`:
   `built = build_training_set(&input, &BuildOptions { seed: s, max_run_groups: CMD_TRAIN_MAX_RUN_GROUPS, until: Some(t_s) })`;
   `fitted = fit_seeded(&built.set, &command_feature_specs(), s as u32)`; write
   `fitted.model` and `CmdModelStats::from_fit(..)` to `--data-dir/bench/<split>-seed<s>/`
   (as `cmd_model.txt` / `cmd_model_stats.json`) and load them with
   `CmdModel::load` (so `ml_weight` is what production would compute from the
   same group counts). "Not enough data" here means: fewer than
   `CMD_TRAIN_MIN_GROUPS` groups, or `fit_seeded` returning an `Err` that starts
   with `every split needs rows`; it does not depend on the seed's LightGBM
   randomness, so the whole split is reported as `skipped: not enough training
   groups`. Any other `Err` skips that seed only (E-N-12).
2. The replay state at `t_s` is `built.replayer` (identical for every seed;
   taken from seed 1).
3. Sample the test runs. A pre-pass over the timeline with a seen-set (commands
   run before `t_s` plus those run earlier in the window) marks each test run
   reachable or not and counts `reachable`. Keep every `stride`-th reachable run
   in timeline order, `stride = max(1, reachable / max_test_runs)`. Sampling does not
   depend on the seed.
4. Walk the test window in timeline order. For each run, before applying it:
   - not reachable: count it in `unreachable` (by source) and continue to "apply";
   - `replayer.follows_pick(run)` holds: never sampled (the pick that caused it is already in the index); continue to "apply";
   - sampled: for `len` in 0, 1, 2, 3 (skipping a `len` greater than the text's
     character count): `query` = first `len` characters of the run's command
     text; `ctx = replayer.context(run.started_at, run.cwd, run.repo_root, Some(session))`;
     candidates = **all** commands in `replayer.index` that match `query`:
     `filter_candidates(.., Scope::All, None)` of [02 section 6.3](02-browse.md#63-filter); rank them with every ranker of 5.3; record the 1-based rank of
     the run's command per ranker (and per seed for `model`);
   - apply: `replayer.apply_run(run)`; events in the window are applied with
     `apply_event` / `commit_pick`. The models stay fixed while the index moves on.

### 5.3 Rankers

| Key | Order | Seeds | Role |
|---|---|---|---|
| `recency` | `last_run_at` descending, then `db_id` descending | none | **The baseline of R1-R3** ("fzf: newest first among fuzzy matches") |
| `fzf_scored` | `fuzzy_score` descending, then `last_run_at` descending, then `db_id` descending | none | Information only: what fzf does when it sorts by match score with recency as tie-break (ADR-17) |
| `heuristic` | `rank_commands(.., model = None)` | none | R3 |
| `model` | `rank_commands(.., Some(&model_s))` | per seed | R1, R2 |

### 5.4 Slices

A sampled lookup belongs to every slice whose condition holds.

| Slice | Condition (state just before the run) |
|---|---|
| `all` | always |
| `gt10min` | The target's `last_run_at < run.started_at - 600`. Lookups whose `shell_session` starts with `import-synth:` are excluded |
| `other_dir` | `run.cwd` is known, `replayer.last_cwd(target)` is `Some`, and the two differ. A target with no known previous cwd is never in this slice |
| `hook` / `import` | `run.source` |
| `repeat` | The target is the most recent command of the same shell session (information: the half that recency already solves) |

Unreachable share = `unreachable / (unreachable + reachable)`, overall and per
source, reported beside the table and excluded from every metric.

### 5.5 Metrics and verdicts

Per (split, ranker, slice, query length): `n`, `top1`, `top3`, `top10`
(share of lookups with rank <= 1, 3, 10) and `mrr` (mean of `1 / rank`). For
`model`: mean, sample standard deviation, min and max over seeds.

| Verdict | Computed as | Reported |
|---|---|---|
| R1 | `model.top1.mean - recency.top1` on (`holdout`, `gt10min`, length 2) | value, PASS/FAIL, and `n` |
| R2 | min over the 8 headline cells of `model.top1.mean - recency.top1` | value, the worst cell, PASS/FAIL |
| R3 | min over (`all`, `gt10min`, `other_dir`) x 4 lengths x 4 metrics of `heuristic - recency` | value, the worst cell, PASS/FAIL |

A cell with `n < 200` is printed with its value and marked `low n`; a verdict
that depends on such a cell is `INCONCLUSIVE`, not PASS. With `--strict` the
process exits 1 unless all three verdicts are PASS.

### 5.6 Output

stdout and `--md`: the label, the `stats` block, the unreachable shares, then
for the holdout split one table per slice:

```
slice gt10min (n=1,412)       len 0            len 1            len 2            len 3
                           top1   mrr       top1   mrr       top1   mrr       top1   mrr
recency                   0.081  0.172     0.214  0.340     0.402  0.521     0.561  0.655
fzf_scored (info)         0.081  0.172     0.198  0.322     0.371  0.498     0.540  0.641
heuristic                 0.102  0.201     0.240  0.366     0.431  0.548     0.580  0.670
model (5 seeds)           0.19   0.31      0.36   0.47      0.55   0.64      0.66   0.73
  +- sd                   0.011  0.008     0.012  0.009     0.010  0.008     0.009  0.007
```

(the numbers above show the format only), then the three verdict lines, then
with `--folds` a compact per-fold table of `top1` at length 2 for `all` and
`gt10min`, then the reference line `file search on its own benchmark: top-1
0.7960, MRR 0.8656 (different task; see 00-blueprint.md section 2)`.

`--json` (the machine-readable form; no command text, no query text):

```json
{ "label": "...", "commit": "<git rev-parse --short HEAD or 'unknown'>", "date": "YYYY-MM-DD",
  "tuning": { "CMD_H_W_RECENCY": 1.0, "...every constant of tuning.rs...": 0 },
  "stats": { "commands": 0, "runs": 0, "span_days": 0.0, "...": 0 },
  "unreachable": { "all": 0.0, "hook": 0.0, "import": 0.0 },
  "splits": { "holdout": { "seeds": 5, "cells": [
      { "ranker": "model", "slice": "gt10min", "len": 2, "n": 1412,
        "top1": {"mean": 0.0, "sd": 0.0, "min": 0.0, "max": 0.0}, "top3": {}, "top10": {}, "mrr": {} } ] } },
  "verdicts": { "R1": {"value": 0.0, "status": "PASS"}, "R2": {"value": 0.0, "cell": "all/len0", "status": "PASS"},
                "R3": {"value": 0.0, "cell": "...", "status": "PASS"} } }
```

For unseeded rankers `sd` is 0 and `min == max == mean`.

## 6. Performance benchmarks

`bench/history.py` (new; `#!/usr/bin/env python3`, standard library only),
importing `REPO`, `WORK`, `ROWS`, `COLS`, `Version`, `spawn`, `stop`,
`drain_for`, `read_until_quiet` from `bench/harness.py`
([:36-41](../../bench/harness.py), [:44](../../bench/harness.py), [:83](../../bench/harness.py),
[:100](../../bench/harness.py), [:135](../../bench/harness.py), [:118](../../bench/harness.py)).
Binary: `REPO/target/release/psychic`. Data dir: `WORK/cmd-perf`. Terminal:
40x120. Percentile: `p(q) = sorted(v)[min(len(v) - 1, ceil(q * len(v)) - 1)]`.
Every picker launch sets `PSYCHIC_NO_RETRAIN=1`
([03 section 8](03-learn.md#8-retraining-trigger)) and restores the pinned model
files first, so no training runs beside a measurement.

### 6.1 `setup [--commands 50000 --runs 500000 --seed 1]`

`rm -rf WORK/cmd-perf`; `bench-history generate` with those numbers;
`history --enable --no-import`; `history --retrain`; copy `cmd_model.txt` and
`cmd_model_stats.json` to `WORK/pinned-cmd-*`; pick the benchmark context with
`sqlite3`: `cwd` = the most frequent `cwd`, `session` = the `shell_session` of
the newest run; store both in `WORK/cmd-perf/bench-context.json`.

### 6.2 `startup [N=30]` - B3

Per trial: note the size of `app.log`; `spawn(version, ("history", "--cwd", cwd,
"--session", session), cwd="/tmp")`; `drain_for(fd, 1.5)`; `stop`; parse the
appended `TIMING` lines. One extra warm-up trial first, discarded.

| Reported (p50, p95) | From |
|---|---|
| **first results on screen (B3)** | `cmd_first_full_render.ms` |
| first results | `cmd_first_results.ms` |
| first paint | `cmd_first_render.ms` |
| index load | `cmd_index_load.ms` |
| model load | `cmd_model_load.ms` |
| first query, worker | the first `cmd_query.total_ms` |

### 6.3 `keystroke [N=200]` - B2 and B1

One picker process: spawn as 6.2; `time.sleep(1.0)`; `read_until_quiet(fd,
idle=0.5, limit=4.0)`. Keys cycle through `b"git st"` then six `0x7F`
(Backspace): every key changes the query, and the cycle covers query lengths
0-6, narrowing (typing) and widening (deleting). Per key: `read_until_quiet(fd)`,
start the clock, `os.write`, wait for the first readable byte (2 s limit; a
miss is dropped and counted), stop the clock, `os.read`, `time.sleep(0.05)` -
the method of [bench/harness.py:218-248](../../bench/harness.py).

| Reported (p50, p95, n, misses) | From |
|---|---|
| **keystroke -> redraw (B2)** | the measured intervals |
| **worker time per keystroke (B1)** | `total_ms` of every `cmd_query` TIMING line of this run except the first |
| filter / heuristic / features / predict / blend (p50) | the same lines |
| matched, stage2 (p50, max) | the same lines |

### 6.4 `track [N=200]` - B4

`N + 10` sequential runs of
`psychic --data-dir WORK/cmd-perf track-command --cwd <cwd> --exit-code 0 --duration-ms 5 --session bench -- "bench command <i % 50>"`
timed with `time.perf_counter()` around `subprocess.run`; the first 10 are
discarded; report p50, p95, max. Run against the perf DB because insert cost
depends on index size. (hyperfine is not required.)

### 6.5 Two-stage check (B8)

`bench-history twostage --queries 400`: build a `Replayer` over the whole
database; load the model. Take 400 runs at an even stride over the last 20% of
the timeline; lookup `j` uses the context of run `j` (its cwd, repo, session,
with `now` = the newest `started_at` + 1) and query length `j % 4` taken from
that run's command text. For each: `full` = ranking with stage 2 over all
matches (`rank_with` with `k = usize::MAX`, [03 section 7.2](03-learn.md#72-rank_commands-with-a-model)),
`two` = ranking with `k = CMD_STAGE2_K`. Report the mean of
`|top10(full) ∩ top10(two)| / min(10, matched)`, the share with equal row 1, the
share of lookups with `matched > CMD_STAGE2_K`, and PASS/FAIL against B8.

### 6.6 `latency` (iteration aid, no terminal)

`bench-history latency --keystrokes 300`: `CommandIndex::load` for the context
in `--data-dir/bench-context.json` (written by 6.1; missing file ->
`bail!("run ./bench/history.py setup first")`), the model, then the key cycle of 6.3 driven straight through
`filter_candidates` (with the worker's narrowing rule) and `rank_commands` on the calling thread; prints p50/p95 of the
total and the stage means. Not a pass bar: B1 is judged from 6.3.

### 6.7 `train`, `size`, `all`

| Mode | Measurement | Metric |
|---|---|---|
| `train [N=3]` | Wall time of `psychic --data-dir WORK/cmd-perf history --retrain`, N times; median and max | B6 |
| `size` | `sqlite3 events.db 'VACUUM'`, then the size of `events.db` in MB | B7 |
| `all` | `startup 30`, `keystroke 200`, `train 3`, `size`, `bench-history twostage`, and last `track 200` (it adds 50 commands and 210 runs to the perf DB, 0.04% of it; `setup` is rerun before comparing two commits); prints the table below; writes `WORK/cmd-results.json` | B1-B4, B6-B8 |

```
perf DB: 50,000 commands / 500,000 runs, 40x120 pty      p50      p95   target  verdict
first results on screen (B3)                            ..ms     ..ms    50 ms   PASS
keystroke -> redraw (B2)                                ..ms     ..ms    16 ms   PASS
worker per keystroke (B1)                               ..ms     ..ms     8 ms   PASS
track-command wall (B4)                                 ..ms     ..ms    10 ms   PASS
retrain (B6)                                            ..s   max ..s    60 s    PASS
events.db after VACUUM (B7)                             ..MB             300 MB  PASS
two-stage top-10 overlap / top-1 agreement (B8)         ..      ..    0.98/0.995 PASS
file search reference: round trip 1.16 ms, keystroke -> redraw 2.45 ms, first full render 11.55 ms
```

The figures in the `target` column and in the sample entry of section 8 are
sample output, not definitions. Targets are read from a dict at the top of `history.py` whose comment cites
[00 section 2](00-blueprint.md#2-success-metrics) as the definition.
B5 is checked by TC-N-10 and IT-B-04, not by timing. B9 is `./bench/run.py
startup 6` against the parent commit ([llm.md:47-52](../../llm.md)).

## 7. Iteration protocol

### 7.1 The loop

| Step | Action |
|---|---|
| 1 | State one hypothesis and change **one** thing (one constant, one feature, one sampler rule). One feature per commit ([llm.md:104-107](../../llm.md)) |
| 2 | `just build && cargo test && cargo clippy` |
| 3 | `relevance --seeds 5 --folds` on the decision corpus and on corpus 3; both JSON files kept |
| 4 | `./bench/history.py all` |
| 5 | Decide with 7.3. Where the decision is within 2x the threshold, rerun step 3 with `--seeds 8` and decide on that ([llm.md:119-122](../../llm.md)) |
| 6 | Keep: commit, with the before/after table in the message and a row in `results.md`. Revert otherwise, and still add a `results.md` row marked `rejected` (a measured dead end is not retried) |
| 7 | If the change touched training or the feature set: `./bench/history.py setup` again before the next comparison (the pinned model no longer fits) |

### 7.2 Which corpus decides

| Condition | Decision corpus |
|---|---|
| Corpus 1 has at least 500 sampled reachable test lookups in the `holdout` `gt10min` slice | Corpus 1 |
| Otherwise | Corpus 2, and every claim carries its label |
| Always | Corpus 3 is run as a guard: a change is rejected if it lowers R2 or R3 there by more than 0.01, and is never accepted because of corpus 3 alone |

### 7.3 Keep or revert

`d` = after minus before on the target cell (default target: `holdout`,
`gt10min`, length 2, `model.top1.mean`).

| Change affects | Keep only if all hold |
|---|---|
| The model (training, features, weights, K, blend) | `d > max(0.01, 2 * sqrt(sd_before^2 / n_seeds + sd_after^2 / n_seeds))`; R2 and R3 still PASS; no B metric moved from PASS to FAIL |
| The heuristic only (deterministic) | Mean of `heuristic.top1` over the 8 headline cells rises by at least 0.003; no headline cell falls by more than 0.002; R3 still PASS; no B metric moved from PASS to FAIL |
| Speed only | The targeted B metric's p95 improves by more than 15% (sub-millisecond figures) or 3% (others) - the noise floors of [llm.md:142-147](../../llm.md) - and R1-R3 are unchanged to 3 decimals |

A difference smaller than 0.03 measured at one seed is not evidence in either
direction ([llm.md:124-125](../../llm.md)).

## 8. Results log format

`docs/ctrl-r/results.md`, created by the Phase D commit, newest entry first,
one entry per benchmark run that was used for a decision:

```markdown
## 2026-11-20 - a1b2c3d - K 2000 -> 1000 (rejected)

Corpus: agent-run commands, not human (3,912 commands, 11,204 runs, 61 days). Seeds 5.

| holdout | recency | fzf_scored | heuristic | model (mean +- sd) |
|---|---|---|---|---|
| gt10min len2 top-1 | 0.402 | 0.371 | 0.431 | 0.548 +- 0.010 |
| all len0 top-1 | ... | ... | ... | ... |
| all len2 MRR | ... | ... | ... | ... |

R1 +0.146 PASS (n=1,412) | R2 -0.004 PASS (all/len0) | R3 +0.000 PASS | unreachable 0.21
B1 5.1/7.2 ms PASS | B2 6.0/9.8 PASS | B3 31/44 PASS | B4 3.1/4.9 PASS | B6 21 s PASS | B7 212 MB PASS | B8 0.991/0.998 PASS
Synthetic guard: R2 -0.002, R3 +0.001 (unchanged).
Decision: rejected - d = -0.004, inside noise (threshold 0.013); B1 improved 7.2 -> 6.1 ms but was already passing.
```

(the numbers show the format only). Required fields: date, short commit, the
one change, kept/rejected, corpus label and size, seed count, the three
headline rows at least, all verdicts with values, the synthetic guard line, the
decision with the threshold used.

## 9. Tuning candidates in priority order

Work top to bottom; within a row try the listed values one at a time.

| # | Candidate | Constant(s) | Values to try | Target |
|---|---|---|---|---|
| 1 | Heuristic weights | `CMD_H_W_FUZZY` | 0.3, 0.6, 1.0 | R3 at lengths 1-3 |
| 1 | | `CMD_H_W_DIR`, `CMD_H_W_REPO` | (0.3, 0.1), (0.5, 0.2), (0.8, 0.3) | R3 on `other_dir`, `all` length 0 |
| 1 | | `CMD_H_RECENCY_SCALE_SECS` | 900, 3600, 14400 | R3 on `all` length 0 |
| 2 | Stage-2 size | `CMD_STAGE2_K` | 500, 1000, 2000, 4000 | B1, B8, R1 |
| 3 | Negative sampler mix | `CMD_TRAIN_RECENT_NEGATIVES` / `CMD_TRAIN_RANDOM_NEGATIVES` | 30/0, 20/10, 15/15, 5/25 | R1, R2 |
| 4 | Synthetic-query mix | `CMD_TRAIN_QLEN_MIX` | [40,20,20,20], [25,25,25,25], [10,30,30,30], [70,10,10,10] | R1 at length 2 against R2 at length 0 |
| 5 | Group weights and blend | `CMD_IMPORT_WEIGHT` | 0.1, 0.3, 1.0 | model minus recency top-1 on the `import` slice (reported; must stay >= -0.01) |
| 5 | | `CMD_PICK_WEIGHT` | 1, 4, 10 (only once real picks exist) | R1 |
| 5 | | `CMD_BLEND_SCALE` | 5, 10, 30 | R2 |
| 6 | Feature ablations | Remove one feature group at a time in a scratch branch (query 1-5, recency 6-12, directory 13-16, sequence 17-18, outcome 19-21, shape 22-23) | - | Gain share from `cmd_model_stats.json` and `d` on R1; a group with under 1% of total gain and `d` inside noise is dropped by replacing its features in place per [03 section 4](03-learn.md#4-features) |
| 7 | Training volume | `CMD_TRAIN_MAX_RUN_GROUPS` | 4000, 8000, 16000 | R1 against B6 |
| 8 | Budget levers, used only when a B metric FAILS | B3: add `CREATE INDEX idx_commands_last_run ON commands(last_run_at)` and load only commands run in the last 365 days plus those with runs in the cwd/repo. B1: lower `CMD_STAGE2_K`; lower `CMD_PAR_FILTER_MIN` to 500. B4: drop `idx_command_runs_hook` and let I1 scan (it runs only during an import) | - | The failing metric; each lever is its own commit and results row |

## 10. Anti-patterns (DO NOT)

| Don't | Do instead | Why |
|---|---|---|
| Re-implement features, the heuristic or the blend in the benchmark (or in Python) | Call `compute_all` and `rank_commands` | A benchmark that has drifted from what ships is worse than none ([docs/how-it-works.md:3004-3011](../how-it-works.md)) |
| Rank a sample of candidates in the test replay | Rank all then-existing matching commands | Sampled negatives inflate top-1; sampling is for training only |
| Let a test run's own execution into the index before ranking it | Rank, then `apply_run` | Leakage: the target would be "run 0 seconds ago" |
| Train on anything at or after the split time | `BuildOptions.until` | The model must predict the future from the past |
| Count never-run-before targets as misses or as hits | Report them as the unreachable share | No ranker can retrieve a command that does not exist yet |
| Compare models at one seed, or read a difference under the 7.3 threshold as a result | 5 seeds, 8 when close | File-mode seed spread is 0.027 ([llm.md:112-113](../../llm.md)) |
| Claim relevance from the synthetic corpus | Label it; use it for scale and as a guard | Its structure is what the generator was told to produce |
| Present corpus 2 numbers as human behaviour | Print `agent-run commands, not human` on every table | An agent's command stream is burstier and more repetitive |
| Write to `~/.local/share/psychic` or read-write open any real history file from a benchmark or test | The data-dir guard; scratch dirs under `/tmp/psychic-bench`; read-only opens | The real data directory is only ever read from ([docs/how-it-works.md:2952](../how-it-works.md)) |
| Put command or query text in JSON, Markdown, stdout or `results.md` | Aggregates, lengths, slice names | [01 section 10.6](01-record.md#106-no-command-text-outside-eventsdb) |
| Measure a picker while a retrain runs beside it | `PSYCHIC_NO_RETRAIN=1`, pinned model restored per launch | A training process takes the cores the filter needs; a new model changes predict time |
| Time B1 from outside the process, or B2 from TIMING lines | B1 from `cmd_query` lines; B2 from the pty | They measure different things: the work, and the work plus the redraw |
| Report medians only | p50 and p95 | The targets are p95 |
| Measure latency on a small database | The perf DB of 6.1 | The budgets are defined at 50,000 / 500,000 |
| Change two things between benchmark runs | One change, one results row | A gain and a loss cancel and both look like noise |
| Edit `bench/harness.py` or `bench/run.py` | `bench/history.py` imports them | ADR-19 |
| Skip the `results.md` row for a rejected change | Record it as `rejected` | Otherwise the same dead end is measured twice |

## 11. Test case specifications

### 11.1 Unit tests

| ID | Component | Input | Expected | Edge cases covered |
|---|---|---|---|---|
| TC-N-01 | metrics | ranks `[1, 2, 4, 11]` | `top1 0.25, top3 0.5, top10 0.75, mrr (1 + 0.5 + 0.25 + 1/11)/4 = 0.4602` | empty rank list -> `n = 0`, metrics `NaN` printed as `-` |
| TC-N-02 | seed aggregation | per-seed top1 `[0.50, 0.52, 0.54]` | `mean 0.52, sd 0.02, min 0.50, max 0.54` | one seed -> `sd 0` |
| TC-N-03 | generator structure | `generate(commands 500, runs 20_000, seed 7, anchor 1_800_000_000)` | Exactly 500 commands and 20,000 runs; newest `started_at == anchor - 60`; immediate-repeat share (as `stats` defines it) between 0.40 and 0.60; mean gap between 200 and 400 s; 40 or fewer distinct cwds; every `repo_root` is a prefix of its `cwd` | `commands == runs` -> every run is a new command |
| TC-N-04 | generator determinism | The same arguments twice into two stores; then seed 8 | The first two have identical `(text, started_at, cwd, exit_code)` sequences; seed 8 differs | - |
| TC-N-05 | transcript parsing | A fixture of 10 JSONL lines (8 assistant lines and the 2 result lines): Bash use with result (`is_error` false), use with error result, background use, use without timestamp, use on a line without `cwd` after a line with one, a non-Bash tool use, a malformed line, a use with no result | 4 kept: exit 0 with duration, exit 1, cwd carried forward, exit NULL; report counts `tool_uses 6`, `skipped_background 1`, `skipped_no_timestamp 1`, `with_result 2` | `input.command` not a string -> `skipped_malformed`; own or empty text -> `skipped_filtered`; two uses on one line keep their order; `agentId` -> session `<sessionId>:<agentId>` |
| TC-N-06 | systematic sample | 10 reachable runs, `max_test_runs` 3 | stride 3 -> ordinals 0, 3, 6, 9 | `max_test_runs >= reachable` -> all |
| TC-N-07 | slices | Target last run 601 s ago in another dir; 600 s ago same dir; an `import-synth:` run 5,000 s ago | `gt10min`+`other_dir`; neither; not in `gt10min` | unknown `run.cwd` -> never `other_dir` |
| TC-N-08 | verdicts | Cells where model - recency is +0.12 on the R1 cell (`n` 300) and -0.02 on `all/len0` | R1 PASS, R2 FAIL naming `all/len0` | R1 cell with `n` 150 -> `INCONCLUSIVE` |
| TC-N-09 (in `bench.rs`, so `cargo test cmd::bench` runs it) | data-dir guard | `run_bench(default dir, true, ..)`, `run_bench(tmp, false, ..)` | Both `Err` with the guard message | `run_bench(tmp, true, Stats)` on an empty dir succeeds and prints zeros |
| TC-N-10 | render purity (B5), in `src/cmd/render.rs` | `include_str!("render.rs")` with the `#[cfg(test)]` module cut off | Contains none of `rusqlite`, `std::fs`, `std::env`, `Instant::now`, `Timestamp::now`, `std::process`, `log::` | - |
| TC-N-11 | baselines | 3 candidates with (`last_run_at`, `fuzzy`) = (10, 5), (30, 5), (20, 9) | `recency` order: 2nd, 3rd, 1st; `fzf_scored` order: 3rd, 2nd, 1st | equal `last_run_at` -> higher `db_id` first |

### 11.2 Integration tests

| ID | Flow | Setup | Verification | Teardown |
|---|---|---|---|---|
| IT-N-01 | Relevance end to end | Temp data dir; `generate(300, 6000, seed 3, anchor fixed)`; `relevance --seeds 2 --max-test-runs 200 --json out.json` | Exit 0; JSON parses; has `splits.holdout.cells` for 4 rankers x 4 lengths x slice `all`; for `recency` and `heuristic` `sd == 0`; `unreachable.all` between 0 and 1; `verdicts` has R1, R2, R3; a second run produces a byte-identical JSON apart from `date` | `remove_dir_all` |
| IT-N-02 | Replay state equals a fresh load | Same data; a `Replayer` with every run applied versus `CommandIndex::load` at `now` = newest run + 1 for one (cwd, repo, session) | `rank_commands(.., None)` over all commands returns the same order of `db_id`s from both, for queries `""` and `g` | `remove_dir_all` |
| IT-N-03 | Agent extraction | Temp projects dir: `p1/s1.jsonl` (the TC-N-05 fixture), `p1/s1/subagents/agent-a9.jsonl` (two uses with `agentId` `a9`, timed between the main file's first two), a non-JSONL file | Runs from both files; `shell_session` values `agent:s1` and `agent:s1:a9`; rows in global time order with `prev_command_id` set inside one session only; `source = 'hook'`; the fixture files' mtimes unchanged | `remove_dir_all` |
| IT-N-04 | Two-stage check | IT-N-01's data plus `retrain`; `twostage --queries 40` with `CMD_STAGE2_K` larger than the corpus | Overlap 1.0 and top-1 agreement 1.0 (no cut applies); exit 0 | `remove_dir_all` |
| IT-N-05 | Real data dir is never written | Record size and mtime of every file in `get_default_data_dir()` (skip if it does not exist); run IT-N-01's commands | Unchanged afterwards | none |
| IT-N-06 | No text leaks from the benchmark | IT-N-01 with one command text replaced by `SENTINEL_c0ffee` | Neither stdout, the JSON nor the Markdown contains `SENTINEL_c0ffee` | `remove_dir_all` |

`bench/history.py` has no automated test; its check is the "done when" run and
the self-consistency rule of E-N-08.

## 12. Error handling matrix

| # | Failure | Detection | Response | Fallback | Logging |
|---|---|---|---|---|---|
| E-N-01 | `--data-dir` missing or the real data dir | Guard of section 2 | `Err` with the guard message, exit 1 | - | stderr via `main` |
| E-N-02 | `generate` / `extract-agent` into a non-empty history | `SELECT COUNT(*) FROM commands > 0` (T1 of [01 section 6.4](01-record.md#64-events-delete-purge-import-status)) | `bail!("--data-dir already holds commands")` | - | stderr |
| E-N-03 | Projects dir missing or holds no transcripts | `read_dir` error / zero files | Prints `no transcripts found under <dir>`; exit 0; nothing written | Corpus 2 is unavailable; say so in `results.md` | stdout |
| E-N-04 | Transcript line malformed, or a newer format without the fields of 4.2 | JSON parse error / field missing | Skip that line or block; counted in the report | If `kept == 0` with `tool_uses > 0`, print `transcript format not recognised` | stdout |
| E-N-05 | Too few runs to split or train | `runs.len() < 100`, or `NotEnoughData` | `relevance` prints `skipped: not enough data (<n> runs)` for that split; verdicts `INCONCLUSIVE`; exit 0 (1 with `--strict`) | Use another corpus | stdout |
| E-N-06 | A slice or cell has `n < 200` | Count | Printed, marked `low n`; dependent verdict `INCONCLUSIVE` | - | stdout |
| E-N-07 | Corpus 1 has only synthetic timestamps | Every lookup excluded from `gt10min` | R1 `INCONCLUSIVE` on that corpus | Section 7.2 picks corpus 2 | stdout |
| E-N-08 | A picker trial produced no `cmd_first_full_render` line, or a keystroke got no redraw within 2 s | Missing key / `select` timeout | Trial dropped and counted; if more than 10% of trials are dropped the mode prints `UNRELIABLE` instead of a verdict | Rerun | stdout |
| E-N-09 | `sqlite3` CLI not installed (`setup`, `size`) | `FileNotFoundError` | Message `bench/history.py needs the sqlite3 command`; exit 1 | - | stderr |
| E-N-10 | Corpus has no pick episodes | `stats` shows `pick episodes: 0` | Pick-derived features read 0 and pick groups are absent; the report prints `no Ctrl-R picks in this corpus: pick features and CMD_PICK_WEIGHT are untested here` | Correctness of the pick path rests on TC-L-13 and IT-B-03 until real picks exist | stdout |
| E-N-11 | Model file from another feature set in `WORK/pinned-cmd-*` | `cmd_model_load` line absent, or `stage2 == 0` in every `cmd_query` line | `keystroke` prints `model not loaded: rerun setup` and marks B1 `UNRELIABLE` | `./bench/history.py setup` | stdout |
| E-N-12 | Training fails for one seed | `Err` from `fit_seeded` | That seed is skipped and the seed count printed is the number that succeeded; fewer than 3 -> `INCONCLUSIVE` | - | stdout |
| E-N-13 | `git rev-parse` unavailable | Command error | `"commit": "unknown"` | - | None |
| E-N-14 | The binary under test is stale | `history.py` compares the mtime of `target/release/psychic` with the newest `src/**/*.rs` | Prints `binary older than sources: run just build` and exits 1 | - | stderr |

## 13. References

| Topic | Location |
|---|---|
| Every target (R1-R3, B1-B9) and the reference numbers | [00-blueprint.md section 2](00-blueprint.md#2-success-metrics) |
| ADR-16, ADR-17, ADR-19 | [00-blueprint.md section 8](00-blueprint.md#8-architecture-decision-records) |
| `record_run`, T1-T4 | [01-record.md section 6](01-record.md#6-sql-statements) |
| Production importer used for corpus 1 | [01-record.md section 12](01-record.md#12-backfill) |
| TIMING lines read by `history.py` | [02-browse.md section 6.5](02-browse.md#65-timing-line), [02-browse.md section 8.2](02-browse.md#82-event-loop) |
| Filter used for candidates | [02-browse.md section 6.3](02-browse.md#63-filter) |
| `build_training_set`, `Replayer`, `BuildOptions.until` | [03-learn.md section 5](03-learn.md#5-training-set-replay) |
| `fit_seeded` | [03-learn.md section 6.1](03-learn.md#61-refactor-of-srctrainingmodrs) |
| `rank_with`, stage 2 | [03-learn.md section 7.2](03-learn.md#72-rank_commands-with-a-model) |
| Tunable constants | [01-record.md section 3](01-record.md#3-constants-phase-a), [02-browse.md section 3](02-browse.md#3-constants-phase-b), [03-learn.md section 3](03-learn.md#3-constants-phase-c) |
| pty harness functions reused | [bench/harness.py:83-97](../../bench/harness.py), [:100-115](../../bench/harness.py), [:118-144](../../bench/harness.py), [:218-248](../../bench/harness.py) |
| File-mode benchmark commands | [bench/run.py:117-162](../../bench/run.py) |
| Rolling-origin folds precedent | [bench/model.py:50-53](../../bench/model.py) |
| Benchmark-every-commit rule, table format | [llm.md:42-62](../../llm.md) |
| Seeds and the noise floor | [llm.md:109-125](../../llm.md), [llm.md:142-147](../../llm.md) |
| Harness pitfalls (geometry, stdin, teardown, pinned model) | [docs/how-it-works.md:2945-2990](../how-it-works.md) |
| Claude Code transcript location (existing importer) | [src/import/sources.rs:407-418](../../src/import/sources.rs) |
