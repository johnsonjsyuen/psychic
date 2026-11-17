# Ctrl-R: shell history search - specification index

Implementation specs for `psychic history` (Ctrl-R) in this fork, written with
the documentation-first "stream coding" method: one Strategic document says
what and why, four Implementation documents say how. Coding agents implement
straight from these files. Base commit `8b6246e`. Phases A, D0 and B are built.

## Documents

| File | Type | Phase | Owns (single source of truth for) |
|---|---|---|---|
| [00-blueprint.md](00-blueprint.md) | Strategic | - | Problem, targets R1-R3 and B1-B9, module map, complete list of upstream edits, MVP scope, non-goals, ADR-1..ADR-20 |
| [01-record.md](01-record.md) | Implementation | A | Schema, every SQL statement and `CmdStore` method, `track-command`, CLI flags of both subcommands, recording hooks, privacy, backfill |
| [02-browse.md](02-browse.md) | Implementation | B | Picker invocation, `CommandIndex`, worker protocol and filter, heuristic ranking, UI, keys, event logging, Ctrl-R widgets |
| [03-learn.md](03-learn.md) | Implementation | C | 23 features, training-set replay, `src/training` refactor, model files, two-stage rank and blend, retrain trigger |
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
| 8 | Targets B6, B7, B8, B9 and the thresholds of the keep/revert rule were chosen by the spec author, not given by the owner | [00 section 2](00-blueprint.md#2-success-metrics), [04 section 7.3](04-bench.md#73-keep-or-revert) | Open to the owner's correction |
| 9 | The latency allowances in [02 section 12](02-browse.md#12-latency-design) are design figures | 02 section 12 | Measured roughly at Phase B (a pty driver, not the Phase D harness): B3 46-52 ms p95 over three runs (at the target, one run over), B1 5.4-5.6 ms p95, B2 2.1-2.7 ms p95 on the perf DB, after splitting the start-up reads, partial sorting and reuse; filter and heuristic exceed their Phase B allowances at p95, leaving Phase C about 2.5 ms. Phase D measures for the verdict; levers are listed in [04 section 9](04-bench.md#9-tuning-candidates-in-priority-order), row 8 |
| 10 | Real-data volume on the authoring machine: `~/.bash_history` has 500 lines and no timestamps; no zsh, fish or Atuin history; Claude Code transcripts hold 5,821 Bash calls in 194 files (64 session files, 130 sub-agent files) | [04 section 7.2](04-bench.md#72-which-corpus-decides), [04 section 4.2](04-bench.md#42-corpus-2-agent-run-commands) | Measured at D0: corpus 1 has 203 distinct commands in 500 runs, all with synthetic timestamps, so it cannot decide R1. Corpus 2 has 5,690 distinct texts in 5,724 runs (first-ever share 0.994), so about 34 runs are reachable lookups: far below the 200-lookup `low n` bar, so it cannot decide R1 either and every verdict on it will be `INCONCLUSIVE`. A machine with real timestamped history (or months of hook recording) is needed for any relevance verdict; until then only corpus 3 runs end to end |
| 11 | Templating is heuristic (`sudo -u bob ls /x` has command word `-u`; `ssh host.com` becomes `ssh PATH`) | [01 section 7](01-record.md#7-templating) | Deterministic and specified; templates feed only the two sequence features |
