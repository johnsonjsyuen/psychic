# Ctrl-R: shell history search - Blueprint (Strategic)

Document type: **Strategic** (WHAT and WHY). Implementation lives in
[01-record.md](01-record.md), [02-browse.md](02-browse.md),
[03-learn.md](03-learn.md), [04-bench.md](04-bench.md). This document holds no
anti-patterns, test cases or error matrices; section 9 points to them.

Base commit: `8b6246e` (upstream `0785be7` plus the mold linker config).

## 1. Problem

A psychic user presses Ctrl-R to re-run a past shell command. The shell's own
search, and fzf's, use the typed characters and recency only. They ignore the
three things that predict the next command best: the current directory, the git
repository, and the command that was just run
([ctrl-r-notes.md:11-15](../ctrl-r-notes.md)).

| Item | Decision |
|---|---|
| Persona | A developer who already runs `eval "$(psychic zsh\|bash\|fish)"` and uses the shell all day |
| Job | Press Ctrl-R, type 0-3 characters, press Enter, get the intended command on the prompt line for editing |
| Outcome | The intended command is row 1 more often than with newest-first fuzzy search, at the same typing effort, with no perceptible delay |
| Context | One machine, one user, local data only |

**Implementation Implication:** the feature is a second picker (`psychic history`)
with its own candidates (commands), its own event log and its own LightGBM
model, launched by a Ctrl-R shell widget, fed by a shell hook that records every
command with cwd, exit code and duration.

## 2. Success metrics

These IDs are the single definition of every target. Other documents cite the
ID; a number that appears in their sample output is illustration, not definition. How each is measured:
[04-bench.md section 5](04-bench.md#5-relevance-benchmark) (R) and
[section 6](04-bench.md#6-performance-benchmarks) (B).

Scale for every B metric: a database with **50,000 distinct commands and
500,000 runs** (the "perf DB", [04-bench.md section 4.3](04-bench.md#43-corpus-3-synthetic-generator)).

| ID | Metric | Target |
|---|---|---|
| R1 | Blended model top-1 minus recency-baseline top-1, query length 2, slice "target last run more than 10 minutes ago", holdout split, mean of 5 seeds | >= +0.10 |
| R2 | Blended model top-1 minus recency-baseline top-1 on every headline cell (slices `all` and `gt10min` x query lengths 0,1,2,3), mean of 5 seeds | >= -0.01 on every cell |
| R3 | Heuristic-only minus recency-baseline, every slice in {`all`, `gt10min`, `other_dir`} x query length x {top-1, top-3, top-10, MRR} | >= -0.001 on every cell |
| B1 | Per-keystroke worker time (filter + heuristic + features + predict + blend), p95 | <= 8 ms |
| B2 | Keystroke to first byte of redraw, p95 | <= 16 ms |
| B3 | Process start to first results on screen, p95 | <= 50 ms |
| B4 | `psychic track-command` wall time, p95 | <= 10 ms |
| B5 | Syscalls and SQLite calls made by command-mode render functions | 0 |
| B6 | `psychic history --retrain` wall time on the perf DB | <= 60 s |
| B7 | `events.db` size of the perf DB after `VACUUM` | <= 300 MB |
| B8 | Two-stage rank versus full scoring: mean top-10 overlap / top-1 agreement over 400 queries | >= 0.98 / >= 0.995 |
| B9 | File-mode `first results` median after Phase A, versus the parent commit (`./bench/run.py startup 6`) | <= +3% |

Reference points for "same performance and relevance characteristics as file
search" (reported beside every result, not targets):

| Reference | Value | Source |
|---|---|---|
| File search steady-state query round trip | 1.16 ms | [how-it-works.md:2834](../how-it-works.md) |
| File search keystroke to redraw | 2.45 ms | [how-it-works.md:2825](../how-it-works.md) |
| File search first full render | 11.55 ms | [how-it-works.md:2826](../how-it-works.md) |
| File search top-1 / MRR on its own benchmark | 0.7960 / 0.8656 | [how-it-works.md:1669](../how-it-works.md) |
| Seed noise of file top-1 (8 seeds) | spread 0.027, sd 0.010 | [llm.md:112-113](../../llm.md) |

Command top-1 at a 0-3 character prefix is a different task from file top-1 at
a user-chosen query: the candidate set is 10-100x larger and the query is
shorter. The comparison that decides success is R1-R3 against the recency
baseline on the same data. Absolute command numbers are reported next to the
file numbers and are never compared with them as a pass/fail test.

**Implementation Implication:** Phase D is not optional polish. No phase is
"done" for the owner until [results.md](04-bench.md#8-results-log-format) holds
a row that passes R1-R3 and B1-B9; the file candidates sit at a few hundred
rows, so B1-B3 force a design (bulk load, parallel filter, two-stage rank) that
file mode never needed.

## 3. Why this wins

| Structural advantage | Evidence in the repo |
|---|---|
| The ranking machinery exists: LambdaRank training with early stopping, time split and atomic model write | `fit` takes `&[FeatureSpec]` already ([src/training/mod.rs:369](../../src/training/mod.rs)) |
| The label volume is higher than for files: Ctrl-R is pressed more often than files are picked ([ctrl-r-notes.md:20-21](../ctrl-r-notes.md)), and this design also uses every executed command as a label | [03 section 5.2](03-learn.md#52-groups-from-executed-commands) |
| Context features (cwd, repo, previous command) are unavailable to fzf and to the shell's own search | [ctrl-r-notes.md:11-15](../ctrl-r-notes.md) |
| A recency-first heuristic guarantees parity with fzf from the first keypress; the model takes over only as data accumulates | Same tanh blend as files ([src/ranker.rs:465-474](../../src/ranker.rs)) |
| The query-to-pick memory (`episode_queries`) carries over unchanged | [src/analytics.rs:45-51](../../src/analytics.rs) |

**Implementation Implication:** reuse `training::fit`, the blend function, the
fuzzy matcher, `tty_input` and the worker protocol as they are; write new code
only for what is command-shaped (storage, features, TUI).

## 4. Architecture decision

One process per Ctrl-R press, like a file pick. Four parts:

```
shell hook --(argv, background)--> psychic track-command --> events.db: commands, command_runs
Ctrl-R widget --> psychic history --query Q --session S --cwd D
                    |-- worker thread: load CommandIndex, filter, rank      (02, 03)
                    |-- UI thread: draw, keys, impressions/picks -> command_events (02)
                    `-- detached child: psychic history --retrain -> cmd_model.txt (03)
selection --> println!(command) on stdout, exit 0 --> widget replaces the prompt buffer
```

### 4.1 Module map (new files only; every path is under `src/cmd/` unless stated)

| File | Responsibility | Phase | Spec |
|---|---|---|---|
| `mod.rs` | Entry points `run_history`, `run_track_command` (A), `run_bench` (D0); the `history` actions of [01 section 13](01-record.md#13-psychic-history-actions-built-in-this-phase); `mod` declarations | A | [01 section 2](01-record.md#2-files-touched) |
| `cli.rs` | clap structs `HistoryArgs`, `TrackCommandArgs` | A | [01 section 4](01-record.md#4-cli-surface) |
| `tuning.rs` | Every tunable constant of the feature | A, B, C | [01 section 3](01-record.md#3-constants-phase-a), [02 section 3](02-browse.md#3-constants-phase-b), [03 section 3](03-learn.md#3-constants-phase-c) |
| `store.rs` | DDL and every SQL statement on the command tables | A | [01 section 5](01-record.md#5-schema), [01 section 6](01-record.md#6-sql-statements) |
| `template.rs` | Raw word splitting, `templatize`, own-command detection | A | [01 section 7](01-record.md#7-templating) |
| `repo.rs` | `find_repo_root` | A | [01 section 8](01-record.md#8-git-repository-root) |
| `privacy.rs` | Opt-in marker, ignore globs, permissions, tombstone hash, removal of the command model on purge (the SQL of purge is `CmdStore::purge`) | A | [01 section 10](01-record.md#10-privacy) |
| `track.rs` | `psychic track-command` | A | [01 section 9](01-record.md#9-track-command) |
| `backfill.rs` | Shell-history and Atuin import | A | [01 section 12](01-record.md#12-backfill) |
| `index.rs` | `CommandIndex`: in-memory statistics, filled from SQL or by replay | B | [02 section 5](02-browse.md#5-commandindex) |
| `rank.rs` | `CommandCandidate`, heuristic, `rank_commands`, two-stage, blend | B, C | [02 section 7](02-browse.md#7-heuristic-ranking), [03 section 7](03-learn.md#7-serving-two-stage-rank-and-blend) |
| `worker.rs` | Worker thread, requests/responses, filter, narrowing, TIMING line | B | [02 section 6](02-browse.md#6-worker) |
| `events.rs` | Impression and pick logging | B | [02 section 10](02-browse.md#10-event-logging) |
| `keys.rs` | `CmdAction`, `CMD_KEYMAP` | B | [02 section 9](02-browse.md#9-keys) |
| `app.rs` | UI state, event loop, input handling | B | [02 section 8](02-browse.md#8-ui-thread) |
| `render.rs` | List, preview, debug pane, help overlay | B | [02 section 8.3](02-browse.md#83-layout-and-rows) |
| `features.rs` | `CommandFeature`, `COMMAND_FEATURE_REGISTRY`, 23 features | C | [03 section 4](03-learn.md#4-features) |
| `train.rs` | Replay, group building, fit, model files, lock, detached retrain | C | [03 section 5](03-learn.md#5-training-set-replay), [03 section 6](03-learn.md#6-fit-and-model-files) |
| `corpus.rs` | Agent-transcript extractor, synthetic generator | D0 (parallel with B) | [04 section 4](04-bench.md#4-corpora) |
| `bench.rs` | `psychic internal bench-history` | D0 (`BenchArgs` with `generate`, `extract-agent`, `stats`), D (`relevance`, `twostage`, `latency`) | [04 section 3](04-bench.md#3-cli-surface) |
| `tests/shell_hooks.rs`, `tests/support/mod.rs` (repo root) | pty-driven zsh/bash/fish hook tests; the shared pty helper | A | [01 section 15.3](01-record.md#153-shell-level-tests-testsshell_hooksrs) |
| `tests/picker.rs` (repo root) | pty-driven picker test | B | [02 section 14.2](02-browse.md#142-integration-tests) |
| `bench/history.py` (repo root) | pty benchmarks for command mode | D | [04 section 6](04-bench.md#6-performance-benchmarks) |

### 4.2 Edits to upstream files (the complete list; anything else is a spec violation)

| Upstream file | Edit | Phase | Spec |
|---|---|---|---|
| `src/main.rs` | `mod cmd;`; early `TrackCommand` dispatch before the logger; `History` and `Internal::BenchHistory` arms | A, D0 | [01 section 2](01-record.md#2-files-touched) |
| `src/cli.rs` | Variants `Commands::History`, `Commands::TrackCommand`, `InternalCommands::BenchHistory` | A, D0 | [01 section 2](01-record.md#2-files-touched) |
| `src/db.rs` | One call to `crate::cmd::store::create_schema` in `prepare_schema` | A | [01 section 5.4](01-record.md#54-where-the-schema-is-created) |
| `src/search_worker.rs` (test only) | The expected table and index lists of `test_an_empty_data_directory_gets_the_whole_schema` | A | [01 section 2](01-record.md#2-files-touched) |
| `src/import/mod.rs` | `pub(crate) mod shell_history`; one new `pub(crate) fn is_dir_within_limits` | A | [01 section 12.2](01-record.md#122-changes-to-srcimport) |
| `src/import/shell_history.rs` | Visibility of 4 items; extract `command_words` and `cwd_step`; add `replay_cwds` | A | [01 section 12.2](01-record.md#122-changes-to-srcimport) |
| `src/shell_init.rs` | One extra `println!` after the "Done." line | A | [01 section 10.2](01-record.md#102-what-psychic-init-prints) |
| `shell/psychic.zsh`, `.bash`, `.fish` | Append a recording block (A) and a Ctrl-R widget block (B) | A, B | [01 section 11](01-record.md#11-shell-hooks), [02 section 11](02-browse.md#11-shell-widgets) |
| `src/training/mod.rs` | `TrainingSet::from_parts`, `row_weights` field, `fit_seeded`, `pub(crate) atomic_write` | C | [03 section 6.1](03-learn.md#61-refactor-of-srctrainingmodrs) |
| `src/ranker.rs` | `compute_blend_weights` and `logistic` become `pub(crate)` | C | [03 section 7.3](03-learn.md#73-blend) |
| `docs/how-it-works.md` | A "Shell history search" section, updated in each phase's commit ([llm.md:30-35](../../llm.md)) | A-D | - |

Not edited: `src/keymap.rs`, `src/help.rs`, `src/render.rs`, `src/app.rs`,
`src/search_worker.rs` outside that one test, `src/analytics.rs`, `src/features.rs`,
`src/feature_defs/*`, `bench/harness.py`, `bench/run.py`, `bench/model.py`,
`train.py`, `Cargo.toml`, `README.md` ([llm.md:36-38](../../llm.md)).

**Implementation Implication:** every implementation document opens with a
"Files touched" table that must be a subset of 4.1 and 4.2.

## 5. Tech stack rationale

| Choice | Rationale |
|---|---|
| Rust, in the existing binary | One install, one data dir, shared LightGBM link; a second binary doubles release work |
| SQLite, same `events.db` | Same connection setup and pragmas ([src/db.rs:207-216](../../src/db.rs)); one file to protect, back up and purge |
| LightGBM LambdaRank, same parameters as files | Already linked and measured ([src/training/mod.rs:177-199](../../src/training/mod.rs)); parameter changes are a benchmark-driven tuning step, not a starting point |
| `fuzzy-matcher` SkimMatcherV2 | Already the file matcher ([src/search_worker.rs:808](../../src/search_worker.rs)); one definition of "matches" for filter, features, training and benchmark |
| rayon | Already a dependency; the 50,000-row filter needs it for B1 |
| No new crate | Glob matching, FNV hashing and pty test plumbing are each under 60 lines; `regex` was removed upstream in `7b99679` and is not re-added |
| Python only in `bench/history.py` | The pty harness is Python ([bench/harness.py:83-97](../../bench/harness.py)); relevance benchmarking is Rust for train/serve parity |

**Implementation Implication:** `Cargo.toml` is not edited in any phase.

## 6. MVP scope (5 features)

| # | Feature | Phase |
|---|---|---|
| 1 | Record every command (opt-in) with cwd, repo root, exit code, duration, shell session; import existing history | A |
| 2 | `psychic history` picker with heuristic ranking, scope filter, preview, delete | B |
| 3 | Ctrl-R widgets for zsh, bash, fish | B |
| 4 | Learned ranker: 23 features, replay-built training set, second model, blend | C |
| 5 | Relevance and performance benchmarks with pass bars, three corpora, results log | D |

Build order: A -> D0 -> B -> C -> D. Commit D0 ([04 section 2](04-bench.md#2-files-touched-and-the-two-commits):
the corpus extractor and the synthetic generator) depends only on the Phase A
schema; it is developed in parallel with B and committed before it. Each phase leaves `just build`,
`cargo test` and `cargo clippy` clean and is one commit with benchmark numbers
in its message ([llm.md:44-62](../../llm.md)).

**Implementation Implication:** each implementation document has an "Inputs"
list (what exists before the phase) and a "Done when" list of runnable commands.

## 7. Non-goals (not built; do not add hooks, flags or schema for them)

| Not building | Reason |
|---|---|
| Pattern-based secret detection | False confidence; the user-controlled opt-outs in [01 section 10](01-record.md#10-privacy) are the v1 answer |
| Sync between machines, any network I/O | The product promise is "your data never leaves your computer" ([README.md:65](../../README.md)) |
| Commands mixed into the file list | Different candidates, features and model; one list would need a generic TUI |
| A generic worker/App/render shared by files and commands | The existing ones are file-shaped throughout (`FileCandidate.full_path: &Path`, [src/ranker.rs:20-28](../../src/ranker.rs)); generifying them maximises fork drift |
| Executing the picked command | The command lands on the prompt line; the user presses Enter |
| Near-duplicate merging (commands differing only in arguments) | One candidate per exact text; `template` is used for sequence features only |
| Editing `~/.zsh_history`, `~/.bash_history` or the fish history | Deleting in psychic removes psychic's copy only; a tombstone stops re-import |
| A Python reference trainer for the command model | `bench/train_parity.py` guards the file model; the command model's guard is the Rust replay benchmark |
| Windows, nushell, PowerShell | Upstream supports zsh, bash, fish on macOS and Linux |
| Recording inside `psychic` file mode, or changing file ranking | Out of scope for the fork-drift rule |

**Implementation Implication:** a request for any row above is answered by
pointing here, not by code.

## 8. Architecture decision records

| ID | Decision | Alternatives rejected | Reason |
|---|---|---|---|
| ADR-1 | New identifiers use `cmd`/`command`; the CLI word is `history` | `history_*` identifiers | `src/history.rs`, `UiState.history_mode`, `Action::ToggleHistoryMode` already mean directory back/forward |
| ADR-2 | Separate slim TUI under `src/cmd/` | Generic picker | Section 7, row 4 |
| ADR-3 | Separate keymap table `CMD_KEYMAP` in `src/cmd/keys.rs` | Extending `KEYMAP` | `KEYMAP` has one row per `Action` enforced by tests ([src/keymap.rs:685](../../src/keymap.rs)) and feeds file-mode help; extending it edits two upstream files and shows command keys in file help |
| ADR-4 | Recording is opt-in through a marker file `history-enabled` in the data dir, checked by the hook with one `stat` | Config file; always-on; checking in the binary | No process is spawned per command while disabled; no config system exists upstream |
| ADR-5 | bash records from `PROMPT_COMMAND` by reading `history 1`; no DEBUG trap, no `PS0` | DEBUG trap chaining; `PS0` arithmetic trick | Cannot conflict with bash-preexec, Starship or direnv; works on bash 4.0+; `HISTCONTROL`/`HISTIGNORE` are honoured by construction |
| ADR-6 | Timestamps are Unix seconds everywhere; ties are broken by row id | Milliseconds | Matches `events.timestamp` ([src/db.rs:433](../../src/db.rs)); bash history has second resolution |
| ADR-7 | `cwd` is the shell's logical `$PWD` string, never canonicalised | `canonicalize` | The hook and the widget must produce the same string with no syscall in the hook |
| ADR-8 | Command tables are created from `prepare_schema` via one call into `src/cmd/store.rs` | DDL inline in `db.rs`; lazy creation on first command use | The decision is "created in `prepare_schema`"; one call keeps the DDL out of the upstream file |
| ADR-9 | `psychic track-command` skips the fern logger and `app.log` entirely | Logging like other subcommands | B4, and command text can never reach a log that is not opened |
| ADR-10 | Retraining runs in a detached child process (`psychic history --retrain --quiet`), at most once per hour, guarded by a lock file | Background thread as in file mode | A Ctrl-R session lasts seconds; a thread dies with the process before training finishes |
| ADR-11 | The running picker loads the model once at start and never reloads | Hot swap | The list must not reorder without user input |
| ADR-12 | One `CommandIndex` type with two fill paths (SQL aggregates for serving, replay for training and benchmark) and one read path | Separate serving and training statistics | Train/serve parity by construction; one parity test covers the loaders |
| ADR-13 | Each executed command yields exactly one synthetic-query training group, the query length drawn by a seeded hash | All four lengths per run | 4x the rows for the same information; B6 |
| ADR-14 | Blend weight uses the file formula on `blend_examples / CMD_BLEND_SCALE` | Raw positive count | Synthetic groups are weaker evidence than clicks; 60 of them must not hand ranking to the model |
| ADR-15 | The fits-registry check compares feature **names in order**, not only the count | Count only ([src/ranker.rs:142-144](../../src/ranker.rs)) | Replacing a feature keeps the count and silently repoints a column |
| ADR-16 | The relevance benchmark is a Rust subcommand calling production code | Python like `bench/model.py` | Feature and rank code must be the shipped code |
| ADR-17 | Pass bars R1-R3 are computed against "newest first among fuzzy matches"; a second baseline, fuzzy score with recency tie-break, is reported as information | Pass bars against fzf's score-sorted order | The owner fixed the baseline; real fzf sorts by score with index tie-break, so both are shown |
| ADR-18 | Shell tests drive real interactive shells through a pty and skip when a shell is missing | Calling hook functions from a non-interactive script | `preexec`, `PROMPT_COMMAND`, `history 1` and `fish_postexec` only behave truthfully in an interactive shell |
| ADR-19 | Command-mode pty benchmarks live in a new `bench/history.py` that imports `bench/harness.py` | New modes in `bench/run.py` | Zero edits to upstream benchmark files |
| ADR-20 | Deleting a command stores a 64-bit FNV-1a hash of its text as a tombstone | Storing nothing; storing the text | Without a tombstone the next import restores it; storing the text defeats deletion |

**Implementation Implication:** an implementer who wants to deviate from an ADR
edits this table first, in the same commit as the code.

## 9. References

### Where implementation content lives

| Content | Location |
|---|---|
| Anti-patterns | [01 section 14](01-record.md#14-anti-patterns-do-not), [02 section 13](02-browse.md#13-anti-patterns-do-not), [03 section 9](03-learn.md#9-anti-patterns-do-not), [04 section 10](04-bench.md#10-anti-patterns-do-not) |
| Test case specifications | [01 section 15](01-record.md#15-test-case-specifications), [02 section 14](02-browse.md#14-test-case-specifications), [03 section 10](03-learn.md#10-test-case-specifications), [04 section 11](04-bench.md#11-test-case-specifications) |
| Error handling matrices | [01 section 16](01-record.md#16-error-handling-matrix), [02 section 15](02-browse.md#15-error-handling-matrix), [03 section 11](03-learn.md#11-error-handling-matrix), [04 section 12](04-bench.md#12-error-handling-matrix) |
| Schema and SQL | [01 section 5](01-record.md#5-schema), [01 section 6](01-record.md#6-sql-statements) |
| Tunable constants | [01 section 3](01-record.md#3-constants-phase-a), [02 section 3](02-browse.md#3-constants-phase-b), [03 section 3](03-learn.md#3-constants-phase-c) |
| Clarity-gate scores, residual ambiguity | [README.md](README.md#clarity-gate) |

### Repository sources cited

| Topic | Location |
|---|---|
| Discussion notes that preceded this plan | [docs/ctrl-r-notes.md:1-66](../ctrl-r-notes.md) |
| Project rules: benchmark every commit, seeds, tests, no `eprintln!` | [llm.md:44-62](../../llm.md), [llm.md:109-125](../../llm.md), [llm.md:206-212](../../llm.md) |
| File-mode performance table | [docs/how-it-works.md:2823-2835](../how-it-works.md) |
| "No lazily initialised global in a parallel loop" | [docs/how-it-works.md:1205-1207](../how-it-works.md) |
| Blend formula | [src/ranker.rs:465-474](../../src/ranker.rs) |
| LightGBM parameters | [src/training/mod.rs:177-199](../../src/training/mod.rs) |

*This document provides the strategic overview. The implementation documents
provide the specifications.*
