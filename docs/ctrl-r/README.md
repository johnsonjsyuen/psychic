# Ctrl-R: shell history search - specification index

Implementation specs for `psychic history` (Ctrl-R) in this fork, written with
the documentation-first "stream coding" method: one Strategic document says
what and why, four Implementation documents say how. Coding agents implement
straight from these files. Base commit `8b6246e`. Phases A, D0, B, C and D are built; the first full
set of results is in [results.md](results.md).

## Documents

| File | Type | Phase | Owns (single source of truth for) |
|---|---|---|---|
| [00-blueprint.md](00-blueprint.md) | Strategic | - | Problem, targets R1-R3 and B1-B10, module map, complete list of upstream edits, MVP scope, non-goals, ADR-1..ADR-21 |
| [01-record.md](01-record.md) | Implementation | A | Schema, every SQL statement and `CmdStore` method, `track-command`, CLI flags of both subcommands, recording hooks, privacy, backfill |
| [02-browse.md](02-browse.md) | Implementation | B | Picker invocation, `CommandIndex`, worker protocol and filter, heuristic ranking (fzf's Ctrl-R order, `src/cmd/fzf.rs`), UI, keys, event logging, Ctrl-R widgets |
| [03-learn.md](03-learn.md) | Implementation | C | 25 features, training-set replay, `src/training` refactor, model files, two-stage rank and blend, retrain trigger |
| [04-bench.md](04-bench.md) | Implementation | D0, D | Corpora, relevance protocol, performance benchmarks, iteration loop, results-log format, tuning candidates |
| `results.md` | Reference | D | Benchmark results log; created by the Phase D commit in the format of [04 section 8](04-bench.md#8-results-log-format) |

Every Implementation document ends with the same four sections, in this order:
Anti-patterns (DO NOT), Test case specifications, Error handling matrix,
References. The Strategic document has none of the first three; its
[section 9](00-blueprint.md#9-references) points to them.

## Where to look

| Question | Location |
|---|---|
| What number must a metric reach? | [00 section 2](00-blueprint.md#2-success-metrics) (only there) |
| Which upstream files may be edited, and how? | [00 section 4.2](00-blueprint.md#42-edits-to-upstream-files-the-complete-list-anything-else-is-a-spec-violation), then the "Files touched" section 2 of each implementation document |
| What does a table, index or query look like? | [01 section 5](01-record.md#5-schema), [01 section 6](01-record.md#6-sql-statements) |
| What is a tunable constant's value? | [01 section 3](01-record.md#3-constants-phase-a), [02 section 3](02-browse.md#3-constants-phase-b), [03 section 3](03-learn.md#3-constants-phase-c); all live in `src/cmd/tuning.rs` |
| Why was something decided? | [00 section 8](00-blueprint.md#8-architecture-decision-records) |

## Build order

| Step | Commit | Spec | Needs | Done when |
|---|---|---|---|---|
| 1 | A: Record | [01](01-record.md) | `8b6246e` | [01 section 1](01-record.md#1-scope-inputs-done-when) |
| 2 | D0: corpora (developed in parallel with B, committed first) | [04 sections 2-4](04-bench.md#2-files-touched-and-the-two-commits) | A | [04 section 1](04-bench.md#1-scope-inputs-done-when), D0 lines |
| 3 | B: Browse | [02](02-browse.md) | A | [02 section 1](02-browse.md#1-scope-inputs-done-when) |
| 4 | C: Learn | [03](03-learn.md) | A, B, D0 | [03 section 1](03-learn.md#1-scope-inputs-done-when) |
| 5 | D: Bench, then the iteration loop | [04](04-bench.md) | A, B, C | [04 section 1](04-bench.md#1-scope-inputs-done-when) and a passing row in `results.md` |

Each commit leaves `just build`, `cargo test` and `cargo clippy` clean, updates
`docs/how-it-works.md` in the same commit, and carries its benchmark table in
the commit message ([llm.md:23-62](../../llm.md)). `README.md` at the repository
root is never edited ([llm.md:36-38](../../llm.md)).

## Clarity gate

Rubric: Actionability 25%, Specificity 20%, Consistency 15%, Structure 15%,
Disambiguation 15%, Reference clarity 10%. Gate: every document >= 9.0.

How the scores were obtained: an independent reviewer (a separate agent with no
access to the author's reasoning) read all five documents, ran
`EXPLAIN QUERY PLAN` on every statement of 01 section 6 against the DDL of 01
section 5, ran the bash hook of 01 section 11.3 in an interactive bash 5.3 on a
pty, and recomputed the worked test values. A second reviewer checked 211
`path:line` citations against the source. Pass 1 found 1 blocker, 13 major and
about 40 minor issues; all were fixed. Pass 2 confirmed those fixes and found
10 more minor issues; all were fixed. The "final" column is the author's score
after those last fixes; it has **not** been graded a third time.

| Document | Pass 1 (reviewer) | Pass 2 (reviewer) | Final (author, after the pass-2 fixes) | Act. | Spec. | Cons. | Struct. | Disamb. | Ref. |
|---|---|---|---|---|---|---|---|---|---|
| 00-blueprint.md | 8.1 | 8.75 | 9.0 | 9 | 9 | 9 | 9 | 9 | 9 |
| 01-record.md | 8.1 | 8.8 | 9.0 | 9 | 9 | 9 | 9 | 9 | 9 |
| 02-browse.md | 7.9 | 8.85 | 9.0 | 9 | 9 | 9 | 9 | 9 | 9 |
| 03-learn.md | 7.9 | 8.65 | 9.0 | 9 | 9 | 9 | 9 | 9 | 9 |
| 04-bench.md | 7.7 | 8.85 | 9.0 | 9 | 9 | 9 | 9 | 9 | 9 |

13-item checklist: type identified on line 3 of each document; anti-patterns,
test cases and error matrices only in the implementation documents; deep links
in all (cross-document anchors and cited line ranges are machine-checked); no
"might / eventually / ideally" statements; each number defined once (targets in
00, constants in the three constants tables, SQL in 01).

## Residual ambiguity and unverified assumptions

An implementer who hits one of these fixes the spec first, in the same commit.

| # | Item | Where | Status |
|---|---|---|---|
| 1 | zsh behaviour: `preexec` `$1` keeps a leading space; `precmd` sees the command's `$?`; `[[ "$1" == ${~HISTORY_IGNORE} ]]` | [01 section 11.2](01-record.md#112-zsh-append-to-shellpsychiczsh) | Verified on zsh 5.9 by IT-S-zsh and an end-to-end pty session (Phase A): `$1` keeps the leading space, `precmd` sees `$?`, `HISTORY_IGNORE` matches |
| 2 | fish behaviour: `$status` inside a `fish_postexec` handler; `string collect`; default Ctrl-R binding text `history-pager` (fish >= 3.6); `--no-config`, `-C` | [01 section 11.4](01-record.md#114-fish-append-to-shellpsychicfish), [02 section 11.3](02-browse.md#113-fish) | Recording half verified on fish 4.2.1 by IT-S-fish and an end-to-end pty session (Phase A): `$status` and `$CMD_DURATION` are right in `fish_postexec`. Found: fish 4 sets `fish_private_mode` under `--no-config`, so the test launches fish without it ([01 section 15.3](01-record.md#153-shell-level-tests-testsshell_hooksrs)). Phase B, on fish 4.2.1 by IT-B-08/IT-B-09: a one-line pick lands on the command line (a multi-line pick through `string collect` is not tested in a shell); the default binding text is `history-pager`, but `bind \cr` lists the preset beside a user's own binding, so the guard asks `bind --user \cr` ([02 section 11.3](02-browse.md#113-fish)); fish 4 waits 2 s at start-up for a terminal reply a test pty never sends, so the tests wait for the first prompt |
| 3 | bash hook | [01 section 11.3](01-record.md#113-bash-append-to-shellpsychicbash) | Verified by the reviewer and again by IT-S-bash on bash 5.3 (5 expected rows, array `PROMPT_COMMAND`, multi-line entries). Not run on bash 4.0-4.4 |
| 4 | Atuin schema (assumptions A1-A4: nanosecond timestamps and durations, `-1` for unknown, `unknown` cwd, optional `deleted_at`) | [01 section 12.4](01-record.md#124-atuin) | From memory of Atuin's public migrations; no Atuin database on the authoring machine. Guarded by a column check and magnitude-based unit detection at run time |
| 5 | Known recording gaps in bash: an immediate repeat under `ignoredups`/`ignoreboth` is not recorded; exit code can be another tool's if that tool is prepended to `PROMPT_COMMAND` later and does not preserve `$?`; duration has 1-second resolution | [01 section 11.3](01-record.md#113-bash-append-to-shellpsychicbash) | Accepted (ADR-5) |
| 6 | No corpus contains Ctrl-R picks until the feature is used for real | [04 section 12](04-bench.md#12-error-handling-matrix), E-N-10 | Pick groups, pick features and `CMD_PICK_WEIGHT` are covered by unit and integration tests only; R1-R3 are measured on run-derived labels |
| 7 | "fzf baseline" is defined by the owner as newest-first among fuzzy matches; real fzf sorts by match score with recency as tie-break | [04 section 5.3](04-bench.md#53-rankers), ADR-17 | Pass bars use the owner's definition; the score-sorted order is reported beside it as `fzf_scored` |
| 8 | Targets B6, B7, B8, B9, B10 (B10 proposed by the task giver, 2026-11-17) and the thresholds of the keep/revert rule were chosen by the spec author, not given by the owner | [00 section 2](00-blueprint.md#2-success-metrics), [04 section 7.3](04-bench.md#73-keep-or-revert) | Open to the owner's correction |
| 9 | The latency allowances in [02 section 12](02-browse.md#12-latency-design) are design figures | 02 section 12 | Measured with the Phase D harness after the merge with the speed work (2026-11-17, [results.md](results.md)): B3 29.6-30.5 ms p50 / 35.7-36.5 ms p95 (was 37.5-37.9 / 45.2-46.1), B1 3.1-3.2 / 5.8-6.4 ms (was 5.3-5.5 / 10.5-11.5, FAIL), B10 4.1-4.2 / 6.8-7.3 ms (was 10.5-10.6 / 17.4-18.6). All under target on synthetic corpora; at p95 the heuristic's head selection (2.3 ms) is over its 1.5 ms allowance. Still open: a different machine, real histories, a terminal emulator instead of a pty |
| 10 | Real-data volume on the authoring machine: `~/.bash_history` has 500 lines and no timestamps; no zsh, fish or Atuin history; Claude Code transcripts hold 5,821 Bash calls in 194 files (64 session files, 130 sub-agent files) | [04 section 7.2](04-bench.md#72-which-corpus-decides), [04 section 4.2](04-bench.md#42-corpus-2-agent-run-commands) | Measured at D0: corpus 1 has 203 distinct commands in 500 runs, all with synthetic timestamps, so it cannot decide R1. Corpus 2 has 5,690 distinct texts in 5,724 runs (first-ever share 0.994), so about 34 runs are reachable lookups: far below the 200-lookup `low n` bar, so it cannot decide R1 either and every verdict on it will be `INCONCLUSIVE`. A machine with real timestamped history (or months of hook recording) is needed for any relevance verdict; until then only corpus 3 runs end to end |
| 11 | Templating is heuristic (`sudo -u bob ls /x` has command word `-u`; `ssh host.com` becomes `ssh PATH`) | [01 section 7](01-record.md#7-templating) | Deterministic and specified; templates feed only the two sequence features |
| 12 | B4 on a real disk: `track-command` skips the close-time checkpoint and truncates the log past `CMD_WAL_TRUNCATE_BYTES` | [01 section 9.1](01-record.md#91-the-write-ahead-log-of-track-command) | Measured with a Python harness (not `bench/history.py`) on ext4 NVMe at the perf DB: p95 7.2-8.8 ms over three series of 500 runs, log at most 2,047 KiB over 5,000 runs. Open: with 8 writers at once p95 is ~21.5 ms (was 12.4-13.7 ms; the log then reached 77 MB, now 2.7 MiB), because a truncating run holds the write lock through its `fsync`s; B4 is defined for sequential runs |
| 13 | The synthetic `interactive` profile is calibrated on 500 runs of one person's bash history (one session, synthetic timestamps, cwd known for 10.8% of runs) | [04 section 4.3](04-bench.md#43-corpus-3-synthetic-generator) | Immediate repeats, top-10/top-100 shares, commands run once, recurrence gaps and lengths are matched at 203 / 500 and checked at 2,000 / 20,000 and 50,000 / 500,000; how they scale, directory and session locality, timestamps and exit codes are assumptions listed there. Relevance is reported per profile (`interactive`, `repetitive`, `agent`) |
| 14 | What the Phase D numbers can and cannot say: every decided figure is on synthetic corpora; no corpus has a Ctrl-R pick; the made-up query is always a prefix (which favours `fzf_scored`); on the perf DB the picker has no repository (the `/synth` directories do not exist) | [results.md](results.md), [04 section 6](04-bench.md#6-performance-benchmarks) | Open until a real timestamped history with picks exists |
| 15 | B2 as defined (keystroke to first byte) is the echo of the query line, drawn before the worker answers | [04 section 6.3](04-bench.md#63-keystroke-n200---pause-s---b2-and-b1) | Resolved 2026-11-17: B10 (keystroke to results drawn, p95 <= 16 ms) was adopted beside B2 ([00 section 2](00-blueprint.md#2-success-metrics)); it passes at 6.8-7.3 ms p95 (9.5-9.7 ms with a 0.3 s pause) |
| 16 | Skim's score (the `fuzzy_score` feature) depends on what the same thread matched before (`fuzzy-matcher` 0.3.7 keeps cached matrices per thread) | [03 section 4](03-learn.md#4-features), feature 1 | Measured 2026-11-17: on one training set the feature's total was 3,828,450 (Skim on every candidate, before), 3,828,434 (Skim on feature rows only, after) and 3,828,374 (a fresh matcher per call); every other feature equal. Serving's rayon threads have their own histories, so training and serving can differ by a few points on a few rows. The fit is sensitive to it: early stopping chose 200 trees on one and 58 on the other. Relevance moved by at most 0.001. Not fixed: a fresh matcher per call is an allocation per row, and its `use_cache(false)` mode panics (`RefCell already borrowed`) |
| 17 | B8 (two-stage versus full scoring) depends on the trained model, not on the code | [04 section 6.5](04-bench.md#65-two-stage-check-b8) | Measured 2026-11-17 on the interactive perf DB: both builds give the same B8 with the same model, 0.977 / 1.000 with the 200-tree model and 0.932 / 0.985 with the 58-tree one; `CMD_STAGE2_K` 4000 lifts them to 0.990 and 0.957 only. B8 FAILS on the shipped (58-tree) model, as it does on the repetitive corpus for both (0.907-0.911 / 0.968). The model ranks rows outside fzf's first 2,000 above its choices; a stage-1 source beside fzf's order (frequent, or in this directory) is the lever, not K |
