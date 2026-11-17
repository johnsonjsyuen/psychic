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
| Output | A results-log row per corpus showing R1-R4 and B1-B10 with PASS/FAIL, then the tuning loop of section 7 |

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
H="env HOME=<scratch home> ./bench/history.py --work <scratch dir on a real disk>"   # section 6: it refuses the real HOME
$H setup          # builds the perf DB of 00 section 2 in <work>/cmd-perf, trains and pins its model
$H all            # B1-B4, B6, B7, B8; writes <work>/cmd-results.json and prints the table of section 6.7
./bench/run.py setup HEAD~1 && ./bench/run.py startup 6 && ./bench/run.py keystroke 30   # file mode unchanged (B9)
ls ~/.local/share/psychic -la     # mtimes unchanged by any command above
```

## 2. Files touched and the two commits

| Commit | New files | Upstream edits |
|---|---|---|
| **D0** (after Phase A; developed in parallel with B, committed **before** B, which rebases onto it) | `src/cmd/corpus.rs`; `src/cmd/bench.rs` with `BenchArgs` and the `Generate`, `ExtractAgent`, `Stats` variants and arms (no stubs for the rest) | [src/cli.rs:121-157](../../src/cli.rs): add `InternalCommands::BenchHistory(crate::cmd::bench::BenchArgs)` with the doc comment `/// Benchmark shell history search: build corpora, measure relevance and latency`. [src/main.rs:500](../../src/main.rs) (the `Commands::Internal` match): add `InternalCommands::BenchHistory(args) => return cmd::run_bench(&data_dir, cli.data_dir.is_some(), args),` |
| **D** (after Phase C) | The `Relevance`, `Twostage`, `Latency` variants of section 3 and their arms in `src/cmd/bench.rs`, with the relevance benchmark in `src/cmd/bench/relevance.rs` (its tests in `src/cmd/bench/relevance/tests.rs`) and `twostage` / `latency` in `src/cmd/bench/perf.rs`; `bench/history.py`; `docs/ctrl-r/results.md`; one test added to `src/cmd/render.rs` (TC-N-10). Small edits to Phase C files the benchmark needs: `train.rs` (`Timeline`, `Step` and `Timeline::window` visible to the benchmark, `Replayer::apply_step`, `replay`, `BuildOptions.shuffle_labels`), `rank.rs` (`CmdRankTimings.sort_ms`, not on the TIMING line), `tuning.rs` (`snapshot`, the `tuning` object of 5.6), `app.rs` (the `cmd_key_to_results` TIMING line of 6.3) | `docs/how-it-works.md`: add the command benchmarks to "The two benchmark harnesses" ([docs/how-it-works.md:2906](../how-it-works.md)) |

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
               #[arg(long)] anchor: Option<i64>,
               #[arg(long, value_enum, default_value_t = Profile::Interactive)] profile: Profile },   // corpus::Profile
    /// Import Bash tool calls from Claude Code transcripts into --data-dir
    ExtractAgent { #[arg(long)] projects_dir: Option<PathBuf> },
    /// Describe the history in --data-dir
    Stats,
    /// Replay the history and score the rankers
    Relevance { #[arg(long, default_value_t = 5)] seeds: u64, #[arg(long, default_value_t = 2000)] max_test_runs: usize,
                #[arg(long)] folds: bool, #[arg(long)] json: Option<PathBuf>, #[arg(long)] md: Option<PathBuf>,
                #[arg(long, default_value = "")] label: String, #[arg(long)] strict: bool,
                #[arg(long)] shuffle_labels: bool },   // the control of section 5.7
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

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile { Interactive, Repetitive, Agent }   // --profile interactive|repetitive|agent
pub(super) fn generate_profile(store: &CmdStore, commands: usize, runs: usize, seed: u64, anchor: i64, profile: Profile) -> anyhow::Result<()>;
pub(super) fn generate(store: &CmdStore, commands: usize, runs: usize, seed: u64, anchor: i64) -> anyhow::Result<()>; // Profile::Interactive
```

(`anchor` = `--anchor`, else the current time). Asserts `1 <= commands <= runs`.
Deterministic for a given `(commands, runs, seed, anchor, profile)`. The
profiles share one algorithm and differ only in the parameters of the table
"Profiles" below; `repetitive` is the D0 generator, run for run (TC-N-15).
One random stream:
`next() = { state = splitmix64(state); state }` starting from `state = seed`
(`splitmix64` as [03 section 5.2](03-learn.md#52-groups-from-executed-commands));
`u()` = `(next() >> 11) as f64 / 2^53` (in `[0, 1)`, never 1.0, so `p_new = 1` always takes the new branch); `below(n)` = `next() % n`.

World:

| Element | Definition |
|---|---|
| Directories | 40: `dirs[k * 5 + j] = /synth/repo<k>/dir<j>`, `k` in 0..8, `j` in 0..5; `repo_root` = `/synth/repo<k>` |
| Hash `h(salt, i)` | `splitmix64(splitmix64(seed.wrapping_add(salt)) ^ i)` |
| Command `i` (0-based; lower `i` = more popular) | Spelled by the profile's text style. `three-words` (the D0 text, `World::text(i)`): `"{TOOLS[i % 40]} {SUBS[(i / 40) % 25]} {arg}"` where `arg` = `src/file{i}.rs` when `i % 3 == 0` else `arg{i}`. `TOOLS` = `["git","cargo","npm","docker","kubectl","make","ls","cat","grep","rg","ssh","curl","python","pytest","just","go","terraform","aws","vim","tar","rsync","scp","find","sed","awk","jq","node","yarn","pnpm","pip","uv","brew","systemctl","journalctl","psql","sqlite3","gh","helm","ansible","tmux"]`; `SUBS` = `["status","build","test","run","push","pull","log","diff","add","commit","checkout","fetch","install","update","list","show","apply","plan","deploy","exec","logs","start","stop","clean","check"]`. Every text is unique because `arg` contains `i`. `interactive`: `TOOLS[i]` for `i < 8`; else by `h(5, i) % 10`: 0-3 and `i < 1000` -> `"{TOOLS[i % 40]} {SUBS[i / 40]}"`; 0-7 -> the D0 text; 8-9 -> the D0 text + `" 2>&1 \| grep -v pattern{i % 97}"`. `agent`: `"cd /synth/repo{i % 8} && {D0 text} --message \"{w words}\" 2>&1 \| tail -n {20 + i % 80}"` with `w = 5 + floor(200 u^3)`, `u` = `h(5, i)` as a uniform, word `k` = `SUBS[(i + 7k) % 25]`. Unique in every style (TC-N-14). `template = templatize(text)` |
| Home directory of `i` | When `h(2, i) % 10 < 7`: `dirs[h(1, i) % 40]`, or, when the profile's `home where first run` is set, the directory `i` first runs in; otherwise none (a "global" command) |
| Reusable | `i < 10`, or `h(6, i)` as a uniform `>= one_off`. Only reusable commands enter the directory lists, the working set and the global list, and only they are chain successors; a one-off runs again only through retry or repeat |
| Chain successor of `i` | `h(4, i) % (i + 1)` when `h(3, i) % 4 == 0`; otherwise none. (edit -> build -> test style chains: a successor's own successor continues the chain) |
| Zipf pick from a list of length `n` | `pos = ((((n + 1) as f64).powf(u())) as usize).saturating_sub(1).min(n - 1)` - an exponent-1 Zipf: `P(pos = k) = ln((k + 2) / (k + 1)) / ln(n + 1)`, so the first element is the most likely and every element is possible |
| Zipf pick of exponent `s` (`zipf_s`) | `s == 1`: the formula above. Otherwise, with `a = 1 - s`: `pos = floor((1 + u() * ((n + 1)^a - 1))^(1 / a)) - 1`, clamped to `0..n` (the inverse CDF of `(x + 1)^-s`; `s = 0` is uniform) |

State: `seen` (commands `0..seen` have run), `reusable` (the reusable ones among
them, in first-run order), per-directory list of reusable commands
homed there (in first-run order), the working set (the last 256 distinct
reusable commands run, most recent first, across sessions), current `dir` (initially `dirs[below(40)]`, drawn once before the loop),
current `session` (initially 0), the session's last 3 commands (the last 3
runs, newest first; duplicates allowed), `t` (seconds,
starts at 0), `retry: Option<i>` (initially `None`). Draws happen in the order
written, and only on the branch taken: the always-drawn ones are `g`, the
move `u()`, `x`, `c` (when choosing), the outcome `u()` and `below(5000)`; the
retry `u()` is drawn only when `retry` is `Some`; the repeat branch draws one
`u()` for its weighted pick only when the last-3 is non-empty; the chain
branch draws nothing; the directory branch draws one `u()` for its Zipf pick
only when the list is non-empty; the recent branch draws one `u()` only when
the working set has at least two entries; the global pick (directly or as a
fall-through) draws one `u()`.

For each run `n` in `0..runs`:

| Step | Rule |
|---|---|
| Gap | `g = u()`. `g < 0.005`: new session (`session += 1`, `dir = dirs[below(40)]`, last-3 cleared), `t += 3600 + below(39_600)`. `g < 0.155`: `t += 300 + below(1500)`. Otherwise: `t += 5 + below(55)` |
| Move | `u() < p_move`: `dir = dirs[below(40)]` |
| New? | `p_new = (commands - seen) / (runs - n)`; `x = u()` (always drawn). If `seen == 0` or `x < p_new`: command = `seen`; `seen += 1`; if reusable, push it on `reusable` and on its home directory's list. This makes exactly `commands` distinct commands by the end |
| Else choose | If `retry` is `Some(i)` and `u() < p_retry`: `i`. Otherwise `c = u()`: `c < repeat` -> repeat: one of the session's last 3 by `repeat weights` (newest first; renormalised over those that exist). `c < chain` -> chain: the successor of the session's last command, if reusable. `c < directory` -> directory: Zipf pick from the current directory's list. `c < recent` -> recent: `zipf_s(recent_s)` pick from the working set without its newest entry. Else -> global: `zipf_s(zipf_s)` pick from `reusable`. A branch that has nothing to offer falls through to global. With `avoid repeat` set, a chain pick equal to the session's last command falls through, and a directory or global pick equal to it takes the next entry of its list (wrapping; a one-entry list keeps it), so immediate repeats come only from retry and repeat |
| Outcome | `exit_code = 1` when `u() < p_fail`, else 0; `retry = Some(command)` after a failure, else `None`. `duration_ms = 10 + below(5000)` |
| Record | Remember `(command, t, dir, session, exit_code, duration_ms)`; push the command on the session's last-3; if reusable, move it to the front of the working set |

Afterwards shift every `t` by `anchor - 60 - t_last` and write all runs in
order with `record_run_in` (`source = Hook`, `shell_session = "synth:<session>"`,
`cwd = dir`, `repo_root` of that dir), one `CmdStore::transaction` per 10,000 runs.

Profiles (`src/cmd/corpus.rs`, `Profile::shape`):

| Parameter | `interactive` (default) | `repetitive` (D0) | `agent` |
|---|---|---|---|
| `p_move` | 0.02 | 0.08 | 0.005 |
| `p_fail` / `p_retry` | 0.08 / 0.25 | 0.10 / 0.5 | 0.10 / 0 |
| `c` bounds: repeat, chain, directory, recent | 0.045, 0.10, 0.30, 0.65 | 0.55, 0.68, 0.87, 0.87 | 0, 0.05, 0.60, 0.85 |
| `repeat weights` | 1, 0, 0 | 0.80, 0.12, 0.08 | 1, 0, 0 |
| `zipf_s` (global) / `recent_s` | 1.5 / 1.8 | 1 / - | 0 (uniform) / 1.0 |
| `avoid repeat` | yes | no | yes |
| `one_off` | 0.5 | 0 | 0 |
| `home where first run` | yes | no | yes |
| Text style | `interactive` | `three-words` | `agent` |

The gap and session rules are the same in all three: mean gap about 300 s,
a new session about every 200 runs, so 500,000 runs span about 4.8 years and
the 30-day window holds about 8,600 runs; `commands / runs` of the runs are
a never-before-seen command. The generator writes no `command_events`
(section 12, E-N-10).

**Calibration of `interactive`.** The targets come from `stats` of corpus 1
(203 commands, 500 runs of one person's bash history). What is data and what
is assumed:

| Statistic | Real (corpus 1) | Source of the target | `interactive` at 203 / 500 | at 2,000 / 20,000 | at 50,000 / 500,000 |
|---|---|---|---|---|---|
| immediate repeat share | 0.064 | data; held constant across sizes is an assumption | 0.072 | 0.062 | 0.058 |
| top-10 share | 0.412 | data at 500 runs; that the head keeps about 0.4 at 500,000 runs is an assumption | 0.352 | 0.413 | 0.380 |
| top-100 share | 0.794 | data, but at 203 commands it is half the vocabulary; it falls with size | 0.794 | 0.694 | 0.558 |
| commands run once | 0.680 | data at 500 runs; a long tail at every size is an assumption | 0.650 | 0.579 | 0.628 |
| recurrence gap in runs p25 / p50 / p75 / p90 | 2 / 7 / 27 / 69 | data, truncated by the 500-run window; longer tails at size follow from the head and tail above | 2 / 6 / 24 / 72 | 3 / 9 / 51 / 652 | 3 / 13 / 254 / 9,387 |
| same directory as last run | 0.600 | weak data: only 10.8% of corpus 1 has a cwd (replayed from `cd`); corpus 2, with true cwds, has 0.882. 0.6-0.8 is assumed | 0.761 | 0.696 | 0.675 |
| same session as last run | 1.000 (one session) | no data: corpus 1 is one file. The session model is D0's (assumption) | 0.872 | 0.786 | 0.687 |
| length in chars p10 / p50 / p90 | 3 / 10 / 42 | data | 3 / 13 / 44 | 3 / 10 / 43 | 3 / 16 / 44 |
| timestamps, gaps in seconds, exit codes, durations | none (synthetic 1 s timestamps, no exit codes) | assumption: D0's rules unchanged | | | |

How a long tail fits 10 runs per command on average: a `one_off` half of
the commands is never chosen again by the chain, directory, recent or global
branches, and the global pick has a heavy head (`zipf_s` 1.5), so the mean
is carried by a few hundred popular commands while the median command runs
once. At 50,000 / 500,000: top-10 0.380, 62.8% of commands run once, every
run is one of 50,000 distinct texts.

The stress profiles: `repetitive` is D0's shape (immediate repeats 0.459-0.471,
gap p50 1 run). `agent` matches corpus 2 only when run at its ratio
(`--commands 5690 --runs 5724`: immediate repeats 0.000, top-10 0.003,
commands run once 0.994, length p50 237 against corpus 2's 0.001 / 0.005 /
0.995 / 237); at 10 runs per command it stays flat (top-10 0.019-0.063) with
almost no immediate repeats. Relevance is reported per profile; none is used
alone to claim relevance (section 4, row 3).

Measured (release build, `--seed 1 --anchor 1794873600`): `interactive`
2,000 / 20,000 in 0.2 s, 4.6 MB as written, span 69.5 days
(`cmd-synth2-small`); 50,000 / 500,000 in 8.6-9.4 s, 120.0 MB as written,
span 1,767.4 days, 8,623 runs in the last 30 days (`cmd-synth2-50k`). D0
(now `--profile repetitive`): 2,000 / 20,000 in 0.2 s, 4.3 MB after
`VACUUM`, immediate-repeat share 0.471, span 68.2 days; 50,000 / 500,000 in
7.6 s, 120.7 MB as written and 111.4 MB after `VACUUM` (B7 budget 300 MB;
`command_runs` 33 MB plus seven run indexes 67 MB), immediate-repeat share
0.459, span 1,758.8 days, 8,725 runs in the last 30 days.

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

Then a second block, `ShapeStats::compute(&all_runs, &all_commands)`: how
repetitive the history is. In T3 order; a run *recurs* when its command ran
before (anywhere); percentiles as section 6, `0` when empty.

| Line | Value |
|---|---|
| `top-10 share`, `top-100 share` | Runs of the 10 (100) most-run commands / runs |
| `commands run once` | Commands with exactly one run / commands with a run |
| `recurrence gap in runs p25/p50/p75/p90: a / b / c / d` | Per recurring run: its position minus the position of the command's previous run (1 = the run just before, in any session) |
| `recurrence gap in seconds p50/p90: a / b` | The same in `started_at` seconds |
| `same directory as last run` | Of recurring runs where both it and the previous run have a cwd: same cwd |
| `same session as last run` | Of recurring runs: same `shell_session` |
| `length in chars p10/p50/p90: a / b / c` | `text.chars().count()` of each run's command, over runs |

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
   groups`. Any other `Err` skips that seed only (E-N-12). The seeds train
   side by side, one thread each (LightGBM here is single-threaded); each
   result depends on its seed only, so the output is the same as in sequence.
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
   - sampled: for each (style, `len`) of the table below (skipping one the text
     is too short for): `query` as the table says;
     `ctx = replayer.context(run.started_at, run.cwd, run.repo_root, Some(session))`;
     candidates = **all** commands in `replayer.index` that match `query`:
     `filter_candidates(.., Scope::All, None)` of [02 section 6.3](02-browse.md#63-filter); rank them with every ranker of 5.3; record the 1-based rank of
     the run's command per ranker (and per seed for `model`);
   - apply: `replayer.apply_run(run)`; events in the window are applied with
     `apply_event` / `commit_pick`. The models stay fixed while the index moves on.

| Style | Lengths | `query` | Role |
|---|---|---|---|
| `prefix` | 0, 1, 2, 3 | The first `len` characters of the command text | **Headline**; every verdict reads it (continuity with the first results) |
| `fragment` | 1, 2, 3, 5 | `fragment_query(text, len, splitmix64(run.id ^ FRAGMENT_SALT))`: the words after the first (split on whitespace), each from its first letter or digit (words without one are dropped); the hash picks one word, the same at every length; its first `len` characters. None (the lookup is skipped at that length) when the text has one word or the chosen word is shorter than `len` | Information: what a user types who remembers a word from the middle of the command. A prefix query flatters any scorer that rewards a match at the start of the text (Skim's, which `fzf_scored` uses) |

| `infix` | 2, 3, 4, 5 | `infix_query(text, len, splitmix64(run.id ^ INFIX_SALT))`: every start, in the words after the first, at a letter or digit that is not the word's first letter or digit (inside a word, or after a `/`, `.`, `-` in it), with `len` characters left; the hash picks one; its first `len` characters. None when there is no such start | Information, and R4: a query rule training never uses (its fragments start a word), so it measures how the model does on queries it was not trained on |

A fragment or infix is a substring of the target, so the target always
matches it. The fragment style was added on 2026-11-17 (before it every
query was a prefix), the infix style on 2026-11-18.

### 5.3 Rankers

| Key | Order | Seeds | Role |
|---|---|---|---|
| `recency` | `last_run_at` descending, then `db_id` descending | none | **The baseline of R1-R3** ("fzf: newest first among fuzzy matches") |
| `fzf_scored` | `fuzzy_score` descending, then `last_run_at` descending, then `db_id` descending | none | Information only: the first approximation of fzf, built on Skim's score (ADR-17). Skim gives the first character of the text 16 more than a later word start, which the prefix style always rewards |
| `fzf` | `fzf_score(FzfQuery::new(query), text)` descending ([02 section 7.1](02-browse.md#71-fzfs-score-srccmdfzfrs)), then as `recency` | none | Information: fzf's own Ctrl-R order (`--scheme=history`), computed in the benchmark from the score alone. Without a model `heuristic` is this order, so the two rows are equal; the row stays so that a later change to the heuristic is measured against it |
| `heuristic` | `rank_commands(.., head = usize::MAX, model = None)` | none | R3 (the cold start) |
| `model` | `rank_commands(.., head = usize::MAX, Some(&model_s))` | per seed | R1, R2 |

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

Two information rows go with every table:

| Row | Computed as | Why |
|---|---|---|
| `stage1_recall` | Share of lookups whose target is among the rows the model scores, `rank::stage2_rows` (fzf's first `CMD_STAGE2_K` rows and the directory's `CMD_STAGE2_DIR_ROWS`; all four metric columns hold it) | The model only re-orders those rows; a target outside them keeps its heuristic rank. B8 measures agreement of top-10s, not this |
| Bayes ceiling (synthetic corpora only, not in the JSON) | `corpus::ceiling` (an ignored test, run by hand): re-simulate the generator with the corpus's arguments; for each reused run of the last 20% (every `stride`-th, about 2,000) compute the generator's exact probability of every command being next from the state it chose from; rank the chosen command among the commands with the same first `len` characters by that probability (equal probabilities share their places) | The best any ranker could do on the prefix style: it knows the generator and that the query is a prefix of the target (it ranks only the commands with the same first characters). The gap between the model and it is what remains to learn; the gap between it and 1 is irreducible |

A third row, the share of lookups whose target has exactly the features of
another stage-2 row (a ceiling for any function of the features), was
measured once on 2026-11-17 and removed: every stage-2 row of every lookup
had distinct features (`log_seconds_since_last_run` alone separates them),
so it was 1.000 everywhere and said nothing.

| Verdict | Computed as | Reported |
|---|---|---|
| R1 | `model.top1.mean - recency.top1` on (`holdout`, `gt10min`, prefix length 2) | value, PASS/FAIL, and `n` |
| R2 | min over the 8 headline cells of `model.top1.mean - recency.top1` | value, the worst cell, PASS/FAIL |
| R3 | min over (`all`, `gt10min`, `other_dir`) x 4 lengths x 4 metrics of `heuristic - recency` | value, the worst cell, PASS/FAIL |
| R4 | min over (`all`, `gt10min`) x the `fragment` and `infix` lengths of `model.top1.mean - fzf.top1` | value, the worst cell, PASS/FAIL (added 2026-11-18) |

A cell with `n < 200` is printed with its value and marked `low n`; a verdict
that depends on such a cell is `INCONCLUSIVE`, not PASS. For R2 and R3, which
take a minimum over many cells: a cell with `n >= 200` below the bar makes the
verdict FAIL (it names the worst such cell); otherwise any low-`n` or empty
cell makes it `INCONCLUSIVE`; otherwise PASS. R4 follows the same rule. R1, R2 and R4 are `INCONCLUSIVE`
when fewer than 3 seeds trained (E-N-12), whatever their cells say. A lookup whose target does not match
its own prefix (none has been seen) counts as a miss for every ranker and is
counted in the output. With `--strict` the
process exits 1 unless all four verdicts are PASS.

### 5.6 Output

stdout and `--md` (the same Markdown; progress and elapsed times go to
stderr): the label, the `stats` block, the unreachable shares, then
for the holdout split one table per slice (all six of 5.4), with `top1`,
`top3`, `top10` and `mrr` at each length and `n` per length in the heading;
the layout below is abridged to two metrics:

```
slice gt10min (n=1,412)       len 0            len 1            len 2            len 3
                           top1   mrr       top1   mrr       top1   mrr       top1   mrr
recency                   0.081  0.172     0.214  0.340     0.402  0.521     0.561  0.655
fzf_scored (info)         0.081  0.172     0.198  0.322     0.371  0.498     0.540  0.641
heuristic                 0.102  0.201     0.240  0.366     0.431  0.548     0.580  0.670
model (5 seeds)           0.19   0.31      0.36   0.47      0.55   0.64      0.66   0.73
  +- sd                   0.011  0.008     0.012  0.009     0.010  0.008     0.009  0.007
```

(the numbers above show the format only), then the same tables for the `fragment` style for `all`,
`gt10min` and `other_dir`, then those for the `infix` style, then the four verdict lines, then
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
      { "ranker": "model", "slice": "gt10min", "style": "prefix", "len": 2, "n": 1412,
        "top1": {"mean": 0.0, "sd": 0.0, "min": 0.0, "max": 0.0}, "top3": {}, "top10": {}, "mrr": {} } ] } },
  "verdicts": { "R1": {"value": 0.0, "status": "PASS"}, "R2": {"value": 0.0, "cell": "all/len0", "status": "PASS"},
                "R3": {"value": 0.0, "cell": "...", "status": "PASS"},
                "R4": {"value": 0.0, "cell": "all/fragment1", "status": "PASS"} } }
```

For unseeded rankers `sd` is 0 and `min == max == mean`. Each split also
carries `sampled`, `reachable`, `not_matched` and `after_pick` (sampled runs
skipped as the execution of a pick), a skipped split a `skipped` reason, and
each verdict its `n`. Values are rounded to 6 decimals; an empty cell's are
`null`.

### 5.7 Leakage controls

What keeps the future out of a measurement, and the test that checks each:

| Risk | Guard | Checked by |
|---|---|---|
| The model sees runs or events at or after the cut | `BuildOptions.until` stops the training replay at the first step at or after `t_s` | `training_at_a_cut_sees_nothing_after_it`: the model text trained on the whole history cut at `t_s` equals the one trained on the history with every run at or after `t_s` deleted |
| A test run's own execution is in the index when it is ranked | Rank, then `apply_run`; a `debug_assert` that the target's `last_run_at` is not after the run | `a_lookup_is_ranked_before_its_own_run_is_applied` (recency rank 2, not 1) |
| A command's later runs feed its features | Features read only the replay index, which holds the past | IT-N-02 (the replay state equals a fresh load from SQL at that moment) |
| The model's gain comes from something other than what it learned | `relevance --shuffle-labels`: each training group's labels are permuted (seeded Fisher-Yates), the counts and features unchanged | Run on each corpus; the model must fall to the heuristic's level or below. Reported in `results.md` |

## 6. Performance benchmarks

`bench/history.py` (new; `#!/usr/bin/env python3`, standard library only),
importing `REPO`, `ROWS`, `COLS`, `Version`, `spawn`,
`drain_for`, `read_until_quiet` from `bench/harness.py`
([:36-41](../../bench/harness.py), [:44](../../bench/harness.py), [:83](../../bench/harness.py),
[:135](../../bench/harness.py), [:118](../../bench/harness.py)).
Usage: `HOME=<scratch> ./bench/history.py --work <dir> <mode> [N]`.
`WORK` is `--work`, required, not harness's `/tmp/psychic-bench`: the timings
belong on a real disk, and `/tmp` may be a small tmpfs. The script refuses to
run (exit 1) when `HOME` is the account's home directory from the password
database, or `--work` lies inside the real data directory: every psychic
subcommand except `track-command` writes a log into its data directory. Every
psychic process gets `--data-dir WORK/cmd-perf` and a fixed environment
(`HOME` = the scratch home, `PATH`, `TERM`, `LANG`, `PSYCHIC_NO_RETRAIN=1`),
nothing else inherited. A picker is stopped with Esc and reaped, with
`SIGKILL` only after 2 s; not with harness's `stop`, which also runs `pkill -f`
for a file-mode `train.py` that command mode never starts.
Binary: `REPO/target/release/psychic`. Data dir: `WORK/cmd-perf`. Terminal:
40x120. Percentile: `p(q) = sorted(v)[min(len(v) - 1, ceil(q * len(v)) - 1)]`.
Every picker launch sets `PSYCHIC_NO_RETRAIN=1`
([03 section 8](03-learn.md#8-retraining-trigger)) and restores the pinned model
files first, so no training runs beside a measurement.

The picker resolves its repository root from the file system; the synthetic
`/synth/...` directories do not exist, so on the perf DB the picker (and
`latency`, which resolves it the same way) ranks with no repository: the
repository term and features read 0. `twostage` uses each run's recorded
`repo_root`.

### 6.1 `setup [--commands 50000 --runs 500000 --seed 1 --profile interactive --anchor <now>] [--from DIR]`

`rm -rf WORK/cmd-perf`; `bench-history generate` with those numbers (or, with
`--from DIR`, a `sqlite3 .backup` of `DIR/events.db` opened read-only, to
measure a corpus already built);
`history --enable --no-import`; `history --retrain`; copy `cmd_model.txt` and
`cmd_model_stats.json` to `WORK/pinned-cmd-*`; pick the benchmark context with
`sqlite3`: `cwd` = the most frequent `cwd`, `session` = the `shell_session` of
the newest run; store both in `WORK/cmd-perf/bench-context.json`.

### 6.2 `startup [N=30]` - B3

Per trial: one `track-command` as in 6.4 (`track_once`: session `bench`,
one of 50 texts); note the size of `app.log`; `spawn(version, ("history", "--cwd", cwd,
"--session", session), cwd="/tmp")`; `drain_for(fd, 1.5)`; `stop`; parse the
appended `TIMING` lines. One extra warm-up trial first, discarded.

The `track-command` is what a shell's hook runs before every prompt, so
before every Ctrl-R, and it keeps the write-ahead log within
`CMD_WAL_TRUNCATE_BYTES` ([01 section 9.1](01-record.md#91-the-write-ahead-log-of-track-command)).
Without it (until 2026-11-17) the pickers' own impression rows grew the log
without bound, 45 MB on the perf DB, and every start paid for rebuilding
the log's index in its first connection (13-20 ms, measured): a cost no
user sees, inside B3. The recorded runs stay in the perf DB (session
`bench`, never the context's session; 50 texts at most), so later modes
on the same DB see them as the newest runs; `setup` again before comparing
two commits.

| Reported (p50, p95) | From |
|---|---|
| **first results on screen (B3)** | `cmd_first_full_render.ms` |
| first results | `cmd_first_results.ms` |
| first paint | `cmd_first_render.ms` |
| index load | `cmd_index_load.ms` |
| its stages: commands read, R2-R8 read, entries in the index, R2-R8 applied (each from the load's start) | `cmd_index_stages.read_ms`, `.context_ms`, `.pushed_ms`, `.applied_ms` |
| model load | `cmd_model_load.ms` |
| first query, worker | the first `cmd_query.total_ms` |

The startup `ms` values count from psychic's `PROCESS_START` (the start of
`main`), as file mode's do; process creation and dynamic loading before it
are not included.

### 6.3 `keystroke [N=200] [--pause S]` - B2 and B1

One picker process: spawn as 6.2; `time.sleep(1.0)`; `read_until_quiet(fd,
idle=0.5, limit=4.0)`. Keys cycle through six words, `b"git st"`, `b"cargo "`,
`b"docker"`, `b"make t"`, `b"npm in"`, `b"kubect"`, each typed and then
deleted with six `0x7F` (Backspace): every key changes the query, and the
cycle covers query lengths 0-6, narrowing (typing) and widening (deleting).
Each word starts with another letter: the worker keeps the matches of the
prefixes typed (its trail, [02 section 6.3](02-browse.md#63-filter)) across
the empty query, so one repeated word would answer every first letter but the
first from that trail and B1 would never see the full scan that starts a real
search. Per key: `read_until_quiet(fd)`, with `--pause S` a further sleep of
`S` seconds, start the clock, `os.write`, wait for the first readable byte
(2 s limit; a miss counts as 2,000 ms in B2 and is counted), stop the clock,
`os.read`, `time.sleep(0.05)` - the method of
[bench/harness.py:218-248](../../bench/harness.py).

The first byte after a key is the picker's echo of the query line: it redraws
before the worker answers. B2 is defined on that byte (00 section 2) and so
is the echo; what the user waits for, the key to its results drawn, is the
picker's own `cmd_key_to_results` TIMING line (the time from sending the
query, in the key handler, to the end of the draw that first shows its
results), reported beside B2 for information. Without a pause the script
types about every 0.17-0.19 s, under `CMD_IMPRESSION_AGE_MS` (200 ms), so the
picker seldom logs impressions (an SQLite write on the UI thread, before the
next draw); `all` therefore also runs `keystroke 100 --pause 0.3`, and B2's
verdict is the worse of the two runs.

| Reported (p50, p95, n, misses) | From |
|---|---|
| **keystroke -> first byte, the echo (B2)** | the measured intervals |
| **keystroke -> results drawn (B10)** | `cmd_key_to_results` lines except the first |
| of which the draw | their `draw_ms` |
| **worker time per keystroke (B1)** | `total_ms` of every `cmd_query` TIMING line of this run except the first (the Backspaces are answered by the worker's reuse rules, [02 section 6.3](02-browse.md#63-filter), as a user's are; `reused` says which) |
| worker per keystroke, not reused | the lines with `reused: false` (each did a filter and a ranking) |
| first letter | the lines with `reused: false`, `narrowed: false` and `query_len` 1: a scan of every command |
| first letter, reused or not | the lines with `narrowed: false` and `query_len` 1 (a seeded letter, 02 section 6.3, is a reuse) |
| filter / heuristic / features / predict / blend (p50, p95) | the lines with `reused: false` |
| matched, stage2 (p50, max) | the same lines |

`app.log` is rotated to `app.log.1` at start-up once it reaches 8 MiB; the
script then reads the rest of the old file and the new one.

### 6.4 `track [N=200]` - B4

`N + 10` sequential runs of
`psychic --data-dir WORK/cmd-perf track-command --cwd <cwd> --exit-code 0 --duration-ms 5 --session bench -- "bench command <i % 50>"`
timed with `time.perf_counter()` around `subprocess.run`; the first 10 are
discarded; report p50, p95, max. Run against the perf DB because insert cost
depends on index size. The cwd is the bench context's synthetic `/synth/...`
directory, which does not exist, so `track-command`'s repository-root walk
stops at once; in a real deep repository it makes a few more `stat` calls
(microseconds). (hyperfine is not required.)

### 6.5 Two-stage check (B8)

`bench-history twostage --queries 400`: build a `Replayer` over the whole
database; load the model. Take 400 runs at an even stride over the last 20% of
the timeline; lookup `j` uses the context of run `j` (its cwd, repo, session,
with `now` = the newest `started_at` + 1) and query length `j % 4` taken from
that run's command text. For each: `full` = ranking with stage 2 over all
matches (`rank_with` with `head = k = usize::MAX`, [03 section 7.2](03-learn.md#72-rank_commands-with-a-model)),
`two` = ranking with `k = CMD_STAGE2_K` and the directory's
`CMD_STAGE2_DIR_ROWS` (what the picker scores; [03 section 7.2](03-learn.md#72-rank_commands-with-a-model) step 3). Report the mean of
`|top10(full) ∩ top10(two)| / min(10, matched)`, the share with equal row 1, the
share of lookups with `matched > CMD_STAGE2_K`, and PASS/FAIL against B8.

### 6.6 `latency` (iteration aid, no terminal)

`bench-history latency --keystrokes 300 [--pause-ms 0] [--now SECS] [--dump FILE]`: `CommandIndex::load` for the context
in `--data-dir/bench-context.json` (written by 6.1; missing file ->
`bail!("run ./bench/history.py setup first")`), the model, then the key cycle of 6.3 driven straight through
`filter_candidates` (with the worker's narrowing rule; it reuses nothing, so it measures every key; its "total" is filter plus ranking, without the building of the result rows the worker's `total_ms` also counts) and `rank_commands` (`head = CMD_PAGE_ROWS`, as the worker) on the calling thread, after one unrecorded cycle that warms the thread pool; prints the mean, p50, p95 and max of the
total and of each stage (filter, heuristic, features, predict, blend, and
`sort`: `CmdRankTimings.sort_ms`, the selection and sort inside the heuristic
and blend stages), and per query length (total and filter). Not a pass bar: B1 is judged from
6.3. `--pause-ms` sleeps that long before each recorded key, as a typist
pauses: the cores clock down and the caches go cold, as in the picker (every
stage takes about twice as long after 170 ms, on one thread as on the pool).
`--now` ranks as of a fixed second; `--dump` replays the same keys (warm-up
cycle included) through the worker's own filter (all seeds computed first,
then its trail, reuse and narrowing) and the worker's ranking, and writes per
key the query length, the match count, a digest of the candidates (idx, fzf
score) in order, the sorted-row count and a digest of every score in ranking
order (ids and numbers only; std's `DefaultHasher`, so compare builds of one
Rust toolchain): two builds rank identically when their dumps are
byte-equal. `history.py latency [N] [--pause S]` runs it on the perf DB with
the pinned model. `PSYCHIC_BENCH_BINARY=<path>` makes `history.py` measure
another build (a saved copy of an earlier commit) on the same perf DB, to
compare two builds interleaved.

### 6.7 `train`, `size`, `all`

| Mode | Measurement | Metric |
|---|---|---|
| `train [N=3]` | Wall time of `psychic --data-dir WORK/cmd-perf history --retrain`, N times; median and max; the pinned model is restored after. `cmd_model.txt` is removed before each run, and a run that does not write it again (a training lock held: `--retrain` then exits 0 at once) stops the mode with exit 1 rather than counting as fast | B6 |
| `size` | `sqlite3 events.db 'VACUUM'`, then the size of `events.db` in MB | B7 |
| `all` | `startup 30`, `keystroke 200`, `keystroke 100 --pause 0.3`, `train 3`, `size`, `bench-history twostage`, and last `track 200` (it adds 50 commands and 210 runs to the perf DB, 0.04% of it; the pickers before it add their impression and pick-less events, a few thousand rows; `setup` is rerun before comparing two commits); prints the table below; writes `WORK/cmd-results.json` | B1-B4, B6-B8 |

```
perf DB: 50,000 commands / 500,000 runs, 40x120 pty      p50      p95   target  verdict
first results on screen (B3)                            ..ms     ..ms    50 ms   PASS
keystroke -> redraw (B2)                                ..ms     ..ms    16 ms   PASS
keystroke -> results drawn (B10)                        ..ms     ..ms    16 ms   PASS
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
| 1 | Cold start (done 2026-11-17) | The heuristic is fzf's order ([02 section 7](02-browse.md#7-heuristic-ranking)); the weights of this row are gone. A psychic signal (frequency, runs in 30 days, directory share, directory recency, a shorter recency scale) may be added only as a tie-break inside equal fzf scores, and only if it is not below pure fzf on any cell of `all`/`gt10min`/`other_dir` x lengths x 4 metrics, both query styles, on every corpus. Measured: every one was below somewhere (results.md) | - | R3, and pure fzf on every cell |
| 2 | Stage-2 size and sources | `CMD_STAGE2_K`, `CMD_STAGE2_DIR_ROWS` | K 500, 1000, 2000, 4000 (4000 measured 2026-11-17 for B8: 0.957 on the shipped model, still FAIL; B1 does not need a smaller K). Directory rows 0, 50, 100 (2026-11-18: 100 adopted, B8 0.93 -> 0.999; 50 gave 0.998; the 100 most-run overall instead gave 0.987, both 0.9997) | B1, B8, R1 |
| 3 | Negative sampler mix (done 2026-11-17: 0/30; pool 2026-11-18) | `CMD_TRAIN_RECENT_NEGATIVES` / `CMD_TRAIN_RANDOM_NEGATIVES` | 15/15, 0/30; also "hard" (8 of the 15 recent by best fzf score), rejected. Pool of the random ones (`CMD_TRAIN_STAGE2_NEGATIVES`): how many of the 30 come from the rows serving scores (`stage2_rows`), the rest from every match: 0, 10, 20, 30 measured 2026-11-18; 20 adopted (30 fails B8) | R1, R2, `all` length 0 top-10 |
| 4 | Query style mix (done 2026-11-17) | `CMD_TRAIN_FRAGMENT_SHARE` | 0, 25, 50 | Prefix lengths 1-3 against the fragment style |
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
| TC-N-03 | generator structure | `generate_profile(commands 500, runs 20_000, seed 7, anchor 1_800_000_000, Repetitive)` | Exactly 500 commands and 20,000 runs; newest `started_at == anchor - 60`; immediate-repeat share (as `stats` defines it) between 0.40 and 0.60; mean gap between 200 and 400 s; 40 or fewer distinct cwds; every `repo_root` is a prefix of its `cwd` | `commands == runs` -> every run is a new command |
| TC-N-12 | `interactive` calibration | `generate_profile(2000, 20_000, seed 7, Interactive)` | immediate repeat 0.04-0.10; top-10 0.30-0.50; commands run once 0.40-0.80; same directory 0.60-0.90; gap p50 3-20 runs, p90 at least 30; length p50 4-20 chars | - |
| TC-N-13 | `agent` stays flat | `Agent` at 1,900 / 2,000 and at 2,000 / 20,000 | immediate repeat below 0.01 both; top-10 below 0.03 and below 0.10; length p50 at least 100 | - |
| TC-N-14 | texts, `zipf_s`, working set, `ShapeStats` | 3,000 texts per style; `zipf_s` at `s` 1, 0, 2; touches 1, 2, 3, 2 then 0..1000; the six-run history of `shape_stats_of_a_small_history` | all distinct, none psychic's own; `zipf_s(u, n, 1) == zipf(u, n)`, `zipf_s(0.5, 10, 0) = 5`, `zipf_s(0.5, 1000, 2) = 0`; working set `[2, 3, 1]`, then 256 long with 999 first; the eight lines of that test | - |
| TC-N-15 | `repetitive` is D0 | `simulate(500, 20_000, 7, Repetitive)` | FNV-1a of `"{command},{t},{dir},{session},{exit_code},{duration_ms};"` per run is `0xd62d4f1ce841ad5f`, the value of commit `d65f6aa` | - |
| TC-N-04 | generator determinism | The same arguments twice into two stores; then seed 8 | The first two have identical `(text, started_at, cwd, exit_code)` sequences; seed 8 differs | - |
| TC-N-05 | transcript parsing | A fixture of 10 JSONL lines (8 assistant lines and the 2 result lines): Bash use with result (`is_error` false), use with error result, background use, use without timestamp, use on a line without `cwd` after a line with one, a non-Bash tool use, a malformed line, a use with no result | 4 kept: exit 0 with duration, exit 1, cwd carried forward, exit NULL; report counts `tool_uses 6`, `skipped_background 1`, `skipped_no_timestamp 1`, `with_result 2` | `input.command` not a string -> `skipped_malformed`; own or empty text -> `skipped_filtered`; two uses on one line keep their order; `agentId` -> session `<sessionId>:<agentId>` |
| TC-N-06 | systematic sample | 10 reachable runs, `max_test_runs` 3 | stride 3 -> ordinals 0, 3, 6, 9 | `max_test_runs >= reachable` -> all |
| TC-N-07 | slices | Target last run 601 s ago in another dir; 600 s ago same dir; an `import-synth:` run 5,000 s ago | `gt10min`+`other_dir`; neither; not in `gt10min` | unknown `run.cwd` -> never `other_dir` |
| TC-N-08 | verdicts | Cells where model - recency is +0.12 on the R1 cell (`n` 300) and -0.02 on `all/len0` | R1 PASS, R2 FAIL naming `all/len0` | R1 cell with `n` 150 -> `INCONCLUSIVE` |
| TC-N-09 (in `bench.rs`, so `cargo test cmd::bench` runs it) | data-dir guard | `run_bench(default dir, true, ..)`, `run_bench(tmp, false, ..)` | Both `Err` with the guard message | `run_bench(tmp, true, Stats)` on an empty dir succeeds and prints zeros |
| TC-N-10 | render purity (B5), in `src/cmd/render.rs` | `include_str!("render.rs")` with the `#[cfg(test)]` module cut off | Contains none of `rusqlite`, `std::fs`, `std::env`, `Instant::now`, `Timestamp::now`, `std::process`, `log::` | - |
| TC-N-16 | fragment queries, verdict scope | `fragment_query("git commit --amend 'fix it'", len, pick)`; a fragment cell far below the baseline | Words `commit`, `amend`, `fix`, `it` (from the first letter or digit); the same word at every length; none past the word's length; `pick` wraps; verdicts unchanged by the fragment cell | One word, or no letter or digit -> none; characters, not bytes |
| TC-N-17 | `fzf` baseline | `git log` (10), `tig git` (30), `grep -i` (40), `ls` (50); query `gi` | Ranks 2, 1, 3, 4 (equal word-start scores newest first, the scattered match, then the non-match) | Target absent -> `NOT_MATCHED`; empty query -> newest first |
| TC-N-18 | Bayes ceiling (`src/cmd/corpus/ceiling.rs`) | `zipf_pmf` against 200,000 draws of `zipf_s` for (n, s) in (1, 1), (7, 1), (50, 1.5), (300, 1.8), (20, 0); the generator's distribution over 2,800 reused runs of interactive 500 / 20,000 | Each probability within 0.005 of the observed frequency; every distribution sums to 1; the mean probability of the chosen command equals the mean sum of squared probabilities within 0.01 (both estimate the same quantity only if the distribution is the generator's) | - |
| TC-N-19 | infix queries | `infix_query("git commit src/a.rs", len, pick)` over picks 0-19 | Starts `om, mm, mi, it, rc, c/, ...`: inside a later word, never at a word's start or a separator, never in the first word; none for a one-word text, a word too short, or nothing after a first letter; a substring of the text | - |
| TC-N-20 | R4 | fzf 0.40 and model 0.395 on fragment and infix cells of `all` and `gt10min`; then an infix cell 0.02 below | PASS, then FAIL naming `all/infix3` with -0.02; an `other_dir` cell far below does not count | - |
| TC-N-11 | baselines | 3 candidates with (`last_run_at`, `fuzzy`) = (10, 5), (30, 5), (20, 9) | `recency` order: 2nd, 3rd, 1st; `fzf_scored` order: 3rd, 2nd, 1st | equal `last_run_at` -> higher `db_id` first |

### 11.2 Integration tests

| ID | Flow | Setup | Verification | Teardown |
|---|---|---|---|---|
| IT-N-01 | Relevance end to end | Temp data dir; `generate(300, 6000, seed 3, anchor fixed)`; `relevance --seeds 2 --max-test-runs 200 --json out.json` | Exit 0; JSON parses; has `splits.holdout.cells` for 5 rankers x 4 lengths x slice `all`, and fragment and `stage1_recall` cells; for every unseeded ranker `sd == 0`; `unreachable.all` between 0 and 1; `verdicts` has R1, R2, R3; a second run produces a byte-identical JSON apart from `date` | `remove_dir_all` |
| IT-N-02 | Replay state equals a fresh load | Same data; a `Replayer` with every run applied versus `CommandIndex::load` at `now` = newest run + 1 for one (cwd, repo, session) | `rank_commands(.., usize::MAX, None)` over all commands returns the same order of `db_id`s from both, for queries `""` and `g`; the `CMD_RECENCY_RANK_CAP` newest matching commands are the same from serving's candidates (both fill paths) and from `train::newest_matching`; every feature per command is equal | `remove_dir_all` |
| IT-N-03 | Agent extraction | Temp projects dir: `p1/s1.jsonl` (the TC-N-05 fixture), `p1/s1/subagents/agent-a9.jsonl` (two uses with `agentId` `a9`, timed between the main file's first two), a non-JSONL file | Runs from both files; `shell_session` values `agent:s1` and `agent:s1:a9`; rows in global time order with `prev_command_id` set inside one session only; `source = 'hook'`; the fixture files' mtimes unchanged | `remove_dir_all` |
| IT-N-04 | Two-stage check | IT-N-01's data plus `retrain`; `twostage --queries 40` with `CMD_STAGE2_K` larger than the corpus | Overlap 1.0 and top-1 agreement 1.0 (no cut applies); exit 0 | `remove_dir_all` |
| IT-N-05 | Real data dir is never written | Record size and mtime of every file in `get_default_data_dir()` (skip if it does not exist); run IT-N-01's commands | Unchanged afterwards | none |
| IT-N-06 | No text leaks from the benchmark | IT-N-01 with one command text replaced by `SENTINEL_c0ffee` | Neither stdout, the JSON nor the Markdown contains `SENTINEL_c0ffee` | `remove_dir_all` |

| IT-N-07 | Leakage controls (section 5.7) | IT-N-01's data; a three-run history | The model trained at a cut equals the one trained on the history truncated there; a test lookup is ranked before its own run is applied; shuffled labels keep the group counts and change the model | `remove_dir_all` |

Also unit-tested: `percentile` of section 6 and the key cycle of 6.3 (both in
`src/cmd/bench/perf.rs`), and that `tuning::snapshot` names every constant.
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
| E-N-15 | `bench/history.py` run with the real `HOME`, or `--work` inside the real data directory | Section 6 preamble | Message naming the problem; exit 1; nothing run | Rerun with a scratch `HOME` | stderr |

## 13. References

| Topic | Location |
|---|---|
| Every target (R1-R4, B1-B10) and the reference numbers | [00-blueprint.md section 2](00-blueprint.md#2-success-metrics) |
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
