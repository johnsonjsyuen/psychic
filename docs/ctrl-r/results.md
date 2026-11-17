# Ctrl-R history search: benchmark results log

Format: [04-bench.md section 8](04-bench.md#8-results-log-format). Newest
entry first; one entry per benchmark run used for a decision. Targets are
defined in [00-blueprint.md section 2](00-blueprint.md#2-success-metrics).
Every figure below is an aggregate; no command or query text.

## 2026-11-17 - Phase D commit (on 967023a) - first full run of the Phase D harness (baseline, kept)

The change: none to ranking. This is the reference every later row compares
with. Corpora: synthetic only (label `synthetic`); the two real corpora on
this machine are too small to decide anything (see "What the evidence is").
Seeds 5 (model seeds 1-5, one corpus seed per profile). Machine: i9-12950HX,
powersave governor, background services (k3s, a browser) on it; relevance
ran while nothing else was measured, performance after the relevance runs
finished, load averages next to each figure.

### Headline (interactive profile, 50,000 commands / 500,000 runs, holdout)

| holdout | recency | fzf_scored | heuristic | model (mean ±sd, min-max) |
|---|---|---|---|---|
| gt10min len2 top-1 | 0.236 | 0.448 | 0.184 | 0.432 ±0.015 (0.413-0.455) |
| all len0 top-1 | 0.065 | 0.065 | 0.065 | 0.154 ±0.010 (0.145-0.168) |
| all len2 top-1 | 0.415 | 0.612 | 0.380 | 0.575 ±0.012 (0.562-0.595) |
| all len2 MRR | 0.527 | 0.684 | 0.493 | 0.657 ±0.009 (0.647-0.672) |
| other_dir len2 top-1 | 0.157 | 0.354 | 0.124 | 0.319 ±0.023 (0.296-0.358) |

R1 +0.196 PASS (n=1,367) | R2 +0.080 PASS (worst all/len3) | R3 -0.262 FAIL (other_dir/len3/top10) | unreachable 0.099
B1 3.37/8.24 ms FAIL, 3.50/9.46 ms FAIL | B2 1.56/3.55, 1.73/3.84 ms PASS (paused: 2.16/4.32, 2.19/6.39 PASS) | B3 37.5/43.2 ... 45.8/56.1 ms: at the limit, 2 of 4 series FAIL | B4 6.15/7.99, 4.67/6.51 ms PASS | B6 4.6 s PASS | B7 111.2 MB PASS | B8 1.000/1.000 PASS
Synthetic guard: this row is the guard's first reference (R2 +0.080, R3 -0.262 on interactive 50k; R2 -0.030, R3 -0.230 on repetitive 50k).
Decision: baseline, kept. R1 passes on every synthetic corpus but agent; R3 fails on every corpus but agent; B1 fails; B3 is at its limit.

Pass bars of the task, on the headline corpus:

| Bar | Value | Verdict |
|---|---|---|
| Model top-1 >= recency baseline + 0.10 at length 2 on `gt10min` (R1) | +0.196 (0.432 vs 0.236, n=1,367; seeds 0.413-0.455) | PASS |
| Model never more than 0.01 below the baseline on the 8 headline cells (R2) | worst +0.080 (all/len3) | PASS (on repetitive 50k: -0.030 at all/len0, FAIL) |
| Heuristic >= baseline on all/gt10min/other_dir x 4 lengths x 4 metrics (R3) | worst -0.262 (other_dir/len3/top10) | FAIL |

The "known going in" figures, checked: on the old repetitive 50k corpus the
model is 3.0 points below recency at length 0 (0.482 vs 0.512, sd 0.004,
five seeds) and the heuristic far below it (0.282 vs 0.512): confirmed (the
earlier one-seed figures were 0.305 vs 0.492 for the heuristic). On the
interactive profile the picture at length 0 is different: recency's top-1 is
only 0.065 (immediate repeats are 6% of runs there, not 46%), the heuristic
ties it at top-1 and loses at top-3/top-10/MRR, and the model is above it at
top-1 (0.154) but below it at top-10 (0.340 vs 0.528).

### What the evidence is

- Every decided number is on synthetic histories. The generator's
  `interactive` profile is calibrated on 500 runs of one person's bash
  history; `repetitive` is the D0 shape; `agent` a flat stream. They say what
  the rankers do on histories shaped like that, not what a person will see.
- The real corpora cannot decide anything: `human, shell history import`
  (203 commands, 500 runs, all timestamps synthetic, so `gt10min` is empty;
  64 reachable test runs, every verdict INCONCLUSIVE) and `agent-run
  commands, not human` (5,690 distinct texts in 5,724 runs: too few groups
  to train at any cut, every split skipped).
- No corpus has a Ctrl-R pick (`pick episodes: 0`). Every label is "the
  command the user ran next", with a query made up as its first 0-3
  characters. Pick groups, pick features and `CMD_PICK_WEIGHT` are untested by
  this benchmark (unit and integration tests only). A real user types any
  substring, not always a prefix, and picks from what is shown: both change
  what wins.
- The made-up query is a prefix, which favours `fzf_scored`: the skim score
  rewards a match at the start of the text, and the target always has one.
  That is why `fzf_scored` is far above `recency` from length 1. It is the
  strongest baseline here, and the model is below it at lengths 1-3 on the
  headline corpus (see "Where it needs work").

### Leakage checks

| Check | Result |
|---|---|
| A model trained on the whole history cut at t equals one trained on the history with everything at or after t deleted (test `training_at_a_cut_sees_nothing_after_it`) | byte-identical model text |
| A lookup is ranked before its own run enters the index (test `a_lookup_is_ranked_before_its_own_run_is_applied`) | recency rank 2 (the run itself would make it 1) |
| The replay state equals a fresh load from SQL (IT-N-02): heuristic order and all 23 feature values per command, queries "" and "g" | equal |
| A pick before the cut's execution after it is skipped, not ranked (test `a_pick_after_the_cut_is_applied_and_its_execution_is_not_a_lookup`) | skipped and counted |
| Shuffled labels (`--shuffle-labels`, 5 seeds): the model must lose what it learned | top-1 falls to 0.022-0.036 on interactive 50k (from 0.154-0.587), 0.026-0.104 on interactive 2k; far below every baseline, so the gain is learned from labels, not leaked by the harness. (It falls below the heuristic too, not to it: the blend gives a 5,000-group model a large weight, so its noise decides the order.) |
| Determinism | two runs give byte-identical JSON apart from the date (IT-N-01); a rerun of interactive 2k with the final binary reproduced every number |

A reviewer agent also read the harness for leakage, baseline fairness,
sampling, percentile maths and what is timed; it found the relevance numbers
sound and four problems in the pty timings (fixed before the numbers below:
the first-letter full scan was hidden by the worker's trail, B2 is the echo,
the pacing stayed under the impression timer, misses were dropped).

### Performance (perf DB: the interactive 50,000 / 500,000 corpus, 40x120 pty)

`bench/history.py all` twice (series A at load 7.6 -> 4.1 while the relevance
runs' load decayed, series B at load 1.2-3.5), plus two `startup 200` series
(load 1.7 and 1.6). p50 / p95; targets from 00 section 2.

| Metric | A | B | startup 200 #1 | startup 200 #2 | Target | Verdict |
|---|---|---|---|---|---|---|
| B3 first results on screen | 41.70 / 57.90 ms (30) | 37.45 / 43.19 ms (30) | 39.46 / 49.30 ms | 45.78 / 56.05 ms | <= 50 ms p95 | at the limit: 2 of 4 series over |
| B1 worker per keystroke | 3.37 / 8.24 ms | 3.50 / 9.46 ms | | | <= 8 ms p95 | FAIL |
| B1, 0.3 s pause before each key | 3.32 / 8.92 ms | 3.43 / 10.43 ms | | | (info) | |
| B2 keystroke -> first byte (the echo) | 1.56 / 3.55 ms | 1.73 / 3.84 ms | | | <= 16 ms p95 | PASS |
| B2, 0.3 s pause | 2.16 / 4.32 ms | 2.19 / 6.39 ms | | | <= 16 ms p95 | PASS |
| keystroke -> results drawn (`cmd_key_to_results`) | 5.88 / 14.59 ms | 7.62 / 17.31 ms | | | (info) | over 16 ms in B |
| same, 0.3 s pause | 7.89 / 16.30 ms | 8.87 / 17.13 ms | | | (info) | |
| B4 track-command wall | 6.15 / 7.99 ms | 4.67 / 6.51 ms | | | <= 10 ms p95 | PASS |
| B6 retrain (3 runs) | 4.6 s, max 4.6 s | 4.5 s, max 4.5 s | | | <= 60 s | PASS |
| B7 events.db after VACUUM | 111.2 MB | 112.5 MB | | | <= 300 MB | PASS |
| B8 top-10 overlap / top-1 agreement (400 lookups; 94% have > 2,000 matches) | 1.0000 / 1.0000 | 1.0000 / 1.0000 | | | >= 0.98 / 0.995 | PASS |

Startup stages (`startup 200`, p50 / p95): index load 29.5 / 37.4 and
34.9 / 44.7 ms; first results 34.8 / 43.5 and 40.7 / 50.2 ms; first query in
the worker 4.1 / 6.0 and 4.0 / 6.3 ms; model load 1.4-1.5 / 1.8-2.4 ms (in
parallel with the index); first paint 7.5-8.2 / 11-12 ms. B3 is the index
load plus about 10 ms, and the index load's spread is B3's.

Worker stages per keystroke, the picker (series B, the 102 keys that filtered
and ranked; p50 / p95): filter 2.70-3.29 / 6.59 ms, heuristic 0.33 / 2.56,
features 0.40 / 0.89, predict 0.36 / 1.05, blend 0.32-0.34 / 0.64-0.67. The
first letter of a word (a scan of all 50,050 commands) is 8.2-9.2 ms p50 and
14.4-19.0 ms max: B1's p95 is the first letters.

The same key cycle without a terminal (`latency 300`, on the calling thread,
back to back, after a warm-up cycle; filter + rank only), mean / p50 / p95 ms:

| Stage | mean | p50 | p95 |
|---|---|---|---|
| filter | 1.51 | 1.38 | 3.33 |
| heuristic (incl. its select/sort) | 0.33 | 0.27 | 0.94 |
| sort (select + sort in heuristic, sort in blend) | 0.18 | 0.17 | 0.35 |
| features | 0.14 | 0.12 | 0.29 |
| predict | 0.15 | 0.13 | 0.26 |
| blend (incl. its sort) | 0.17 | 0.16 | 0.31 |
| total | 2.29 | 2.03 | 4.66 |

by length (total p50 / p95, matched p50): 0: 1.81 / 2.23 (50,050); 1: 3.49 /
5.61 (24,000); 2: 2.48 / 5.05 (5,277); 3: 1.85 / 4.88 (2,500); 4-6: 1.1-1.8 /
1.9-3.9. The picker's worker takes about twice this for the same work: its
stages are 2-3x slower at p50 (features 0.40 vs 0.12 ms, predict 0.36 vs
0.13 ms). The difference is the pacing: in the picker the rayon threads sleep
for 170 ms+ between keys and wake cold, here they never sleep.

Stress (repetitive 50,000 / 500,000 as the perf DB, one series, load 2-3):
B1 3.77 / 9.16 ms FAIL; B2 1.60 / 3.31 ms; key -> results 7.20 / 15.84 ms;
`latency` total 2.04 / 3.80 ms (predict 0.39 / 0.65 ms: its model has more
trees); **B8 0.9765 / 0.9300 FAIL** (85% of lookups over 2,000 matches: the
heuristic's top 2,000 misses the model's top 1 in 7% of lookups when half of
all runs repeat the previous command).

### Side by side with file search (same tool, 00 section 2 reference points)

| | File search | History search (headline) | Ratio |
|---|---|---|---|
| Top-1 on its own benchmark | 0.796 | 0.154 (all, len 0) / 0.510 (len 1) / 0.575 (len 2) / 0.587 (len 3) | worse at every length |
| MRR | 0.866 | 0.221 / 0.594 / 0.657 / 0.667 (len 0-3) | worse at every length |
| Steady-state query round trip (worker) | 1.16 ms | 3.37-3.50 ms p50, 8.2-9.5 ms p95 | ~3x at p50 |
| Keystroke -> redraw (first byte) | 2.45 ms | 1.56-1.73 ms p50 (the echo) | comparable; but results take 5.9-7.6 ms p50, 14.6-17.3 ms p95 |
| First full render | 11.55 ms | 37.5-45.8 ms p50, 43-58 ms p95 | ~3.5x |

The tasks differ (00 section 2): history search ranks 10-100x more
candidates from a 0-3 character prefix, against a user-chosen query in file
search. Plainly, history search is worse on every figure: relevance by 0.2-0.6
in top-1, the worker about 3x slower, start-up about 3.5x slower and at its
budget, and results reach the screen in ~15-17 ms p95, at the 16 ms frame
budget, against file search's few milliseconds.

### Where relevance and speed most need work (ranked, with evidence)

1. **The heuristic (R3 FAILS, -0.26).** Below recency on every corpus but
   agent: on interactive 50k, `other_dir` top-10 at len 3 is 0.359 vs 0.621
   and top-1 0.132 vs 0.251; at length 0 top-3 0.163 vs 0.311. Its fuzzy
   term is weak (len 1 top-1 0.237 vs fzf_scored 0.518) and the directory
   term pulls commands of the current directory above the target run
   elsewhere. Section 9 row 1 (`CMD_H_W_FUZZY`, `CMD_H_W_DIR`/`REPO`,
   `CMD_H_RECENCY_SCALE_SECS`) is the place to start.
2. **The model does not beat `fzf_scored` at 50k.** Interactive 50k, top-1
   model vs fzf_scored: len 1 0.510 vs 0.518, len 2 0.575 vs 0.612, len 3
   0.587 vs 0.623; gt10min len 2 0.432 vs 0.448; other_dir len 2 0.319 vs
   0.354. On the 2k corpus it beats it by 0.08-0.14. B8 rules out the
   stage-2 cut on this corpus (overlap 1.000), so the model itself scores
   the prefix match too low at 50k (more candidates sharing a prefix, the
   same 8,000 training groups). Candidates: section 9 rows 4 (query-length
   mix) and 7 (training volume), and a feature for "starts with the query".
3. **Length 0 on a repetitive history (R2 FAILS there, -0.030).** The model
   under-ranks the session's previous command: on the `repeat` slice of
   interactive 50k, model top-1 at length 0 is 0.152 vs recency 1.000
   (n=130); on repetitive 50k all/len0 0.482 vs 0.512. It learned from a
   corpus where immediate repeats are rare. Section 9 rows 3-5 (negatives,
   query mix, blend scale) are the levers; the model's top-10 at length 0
   (0.340 vs recency 0.528) is the cell to watch.
4. **B1 FAILS (8.2-9.5 ms p95), from the first letter.** The full scan at
   length 1 is 8-9 ms p50 and up to 19 ms; every other key is under 6 ms.
   The same work back to back takes half as long (latency p95 4.7 ms), so
   cold rayon threads cost about as much as the work. Levers: section 9 row
   8 (`CMD_STAGE2_K`, `CMD_PAR_FILTER_MIN`), keeping the pool warm while the
   picker is open, or precomputed first-letter buckets.
5. **B3 at its limit (43-58 ms p95).** The index load is 27-35 ms p50 and
   carries all of the spread. Section 9 row 8 (load only recent commands
   plus those of this cwd/repo first).
6. **Key -> results at the frame budget (14.6-17.3 ms p95)**, not a target
   but what a user sees: worker time plus a redraw.
7. **B8 on a repetitive history (0.977 / 0.930)**: the heuristic's top 2,000
   misses the model's choice there. It moves with item 1.
8. **Evidence.** None of this is a verdict on human behaviour until a real,
   timestamped history (or months of hook recording, with picks) exists; the
   agent profile is flat enough that a 1-3 character prefix (always `cd `)
   carries nothing, so it only shows R1 failing for a reason the
   corpus explains (+0.030).

### How to re-run (and how long it takes on this machine)

Never on a real corpus or data dir: copy the corpus first, use a scratch
`HOME`. With `D=$HOME_SCRATCH/corpus-copy`:

| What | Command | Time |
|---|---|---|
| Relevance, 5 seeds + folds | `psychic --data-dir $D internal bench-history relevance --seeds 5 --folds --json r.json --md r.md` | 50k/500k: 5.3 min (holdout alone ~1.5 min); 2k/20k: 2.5 min; agent 5k/50k: 4 min |
| Shuffled-label control | `... relevance --seeds 5 --shuffle-labels --json s.json` | 50k: 1.3 min |
| Perf DB setup | `HOME=<scratch> ./bench/history.py --work <dir on ext4> setup [--from <corpus dir>]` | 6 s with `--from` (5 s of it training), about 15 s when it generates (9 s of that) |
| B1-B4, B6-B8 | `HOME=<scratch> ./bench/history.py --work <dir> all` | 2.7 min |
| B3 with many trials | `... startup 200` | 5.2 min |
| Stage times, no pty | `... latency 300` | 2 s |
| B8 alone | `... twostage` | 6 s |

The corpora of this entry: `generate --commands 50000 --runs 500000 --seed 1
--anchor 1794873600` (profile interactive; repetitive with `--profile
repetitive`), 2,000 / 20,000 the same, agent `--commands 5000 --runs 50000
--seed 1 --anchor 1794873600 --profile agent`.

### Relevance tables (holdout, 5 model seeds; model cells mean ±sd)

#### interactive 50,000 / 500,000 (headline)

Label `synthetic`; span 1767 days; immediate-repeat share 0.058; holdout: 90,053 reachable test runs, 2,002 sampled, unreachable share 0.099, 5 model seeds.


`all` (n = 2,002, 2,002, 2,002, 1,975 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.065 | 0.220 | 0.415 | 0.507 |
| fzf_scored | top1 | 0.065 | 0.518 | 0.612 | 0.623 |
| heuristic | top1 | 0.065 | 0.237 | 0.380 | 0.427 |
| model | top1 | 0.154 ±0.010 | 0.510 ±0.013 | 0.575 ±0.012 | 0.587 ±0.014 |
| model, shuffled labels (control) | top1 | 0.022 ±0.018 | 0.026 ±0.023 | 0.034 ±0.026 | 0.036 ±0.028 |
| recency | top3 | 0.311 | 0.456 | 0.608 | 0.662 |
| fzf_scored | top3 | 0.311 | 0.685 | 0.733 | 0.734 |
| heuristic | top3 | 0.163 | 0.432 | 0.564 | 0.594 |
| model | top3 | 0.241 ±0.027 | 0.639 ±0.012 | 0.696 ±0.010 | 0.702 ±0.009 |
| recency | top10 | 0.528 | 0.638 | 0.713 | 0.754 |
| fzf_scored | top10 | 0.528 | 0.763 | 0.800 | 0.794 |
| heuristic | top10 | 0.419 | 0.617 | 0.704 | 0.742 |
| model | top10 | 0.340 ±0.038 | 0.739 ±0.004 | 0.825 ±0.006 | 0.832 ±0.006 |
| recency | mrr | 0.223 | 0.363 | 0.527 | 0.598 |
| fzf_scored | mrr | 0.223 | 0.613 | 0.684 | 0.689 |
| heuristic | mrr | 0.160 | 0.366 | 0.493 | 0.535 |
| model | mrr | 0.221 ±0.016 | 0.594 ±0.011 | 0.657 ±0.009 | 0.667 ±0.010 |

`gt10min` (n = 1,367, 1,367, 1,367, 1,346 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.008 | 0.080 | 0.236 | 0.334 |
| fzf_scored | top1 | 0.008 | 0.339 | 0.448 | 0.463 |
| heuristic | top1 | 0.025 | 0.096 | 0.184 | 0.225 |
| model | top1 | 0.114 ±0.013 | 0.372 ±0.012 | 0.432 ±0.015 | 0.442 ±0.017 |
| model, shuffled labels (control) | top1 | 0.014 ±0.009 | 0.019 ±0.012 | 0.027 ±0.015 | 0.029 ±0.017 |
| recency | top3 | 0.105 | 0.244 | 0.434 | 0.510 |
| fzf_scored | top3 | 0.105 | 0.538 | 0.609 | 0.613 |
| heuristic | top3 | 0.053 | 0.217 | 0.371 | 0.410 |
| model | top3 | 0.171 ±0.025 | 0.507 ±0.014 | 0.582 ±0.014 | 0.588 ±0.012 |
| recency | top10 | 0.314 | 0.470 | 0.579 | 0.640 |
| fzf_scored | top10 | 0.314 | 0.653 | 0.707 | 0.701 |
| heuristic | top10 | 0.184 | 0.439 | 0.567 | 0.622 |
| model | top10 | 0.247 ±0.039 | 0.634 ±0.004 | 0.749 ±0.009 | 0.761 ±0.009 |
| recency | mrr | 0.098 | 0.198 | 0.359 | 0.442 |
| fzf_scored | mrr | 0.098 | 0.457 | 0.546 | 0.553 |
| heuristic | mrr | 0.069 | 0.200 | 0.310 | 0.354 |
| model | mrr | 0.163 ±0.018 | 0.466 ±0.011 | 0.536 ±0.012 | 0.544 ±0.013 |

`other_dir` (n = 670, 670, 670, 657 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.007 | 0.055 | 0.157 | 0.251 |
| fzf_scored | top1 | 0.007 | 0.243 | 0.354 | 0.371 |
| heuristic | top1 | 0.010 | 0.084 | 0.124 | 0.132 |
| model | top1 | 0.067 ±0.012 | 0.290 ±0.022 | 0.319 ±0.023 | 0.324 ±0.023 |
| model, shuffled labels (control) | top1 | 0.012 ±0.007 | 0.014 ±0.011 | 0.021 ±0.014 | 0.025 ±0.017 |
| recency | top3 | 0.061 | 0.145 | 0.324 | 0.440 |
| fzf_scored | top3 | 0.061 | 0.457 | 0.564 | 0.575 |
| heuristic | top3 | 0.036 | 0.136 | 0.191 | 0.207 |
| model | top3 | 0.114 ±0.020 | 0.382 ±0.014 | 0.420 ±0.016 | 0.428 ±0.012 |
| recency | top10 | 0.173 | 0.349 | 0.510 | 0.621 |
| fzf_scored | top10 | 0.173 | 0.648 | 0.743 | 0.738 |
| heuristic | top10 | 0.112 | 0.225 | 0.306 | 0.359 |
| model | top10 | 0.163 ±0.030 | 0.480 ±0.005 | 0.586 ±0.015 | 0.598 ±0.013 |
| recency | mrr | 0.064 | 0.140 | 0.275 | 0.375 |
| fzf_scored | mrr | 0.064 | 0.382 | 0.488 | 0.499 |
| heuristic | mrr | 0.040 | 0.136 | 0.185 | 0.204 |
| model | mrr | 0.105 ±0.014 | 0.362 ±0.015 | 0.406 ±0.017 | 0.412 ±0.016 |

R1 +0.196 PASS (gt10min/len2, n=1,367) | R2 +0.080 PASS (all/len3, n=1,975) | R3 -0.262 FAIL (other_dir/len3/top10, n=657)

Folds, top-1 at length 2 (recency / heuristic / model mean ±sd): fold60 all 0.401 / 0.388 / 0.611 ±0.008, gt10min 0.229 / 0.219 / 0.496 ±0.015; fold70 all 0.412 / 0.378 / 0.624 ±0.010, gt10min 0.238 / 0.217 / 0.514 ±0.023; fold80 all 0.405 / 0.358 / 0.571 ±0.014, gt10min 0.232 / 0.182 / 0.427 ±0.017

#### interactive 2,000 / 20,000

Label `synthetic`; span 69 days; immediate-repeat share 0.062; holdout: 3,632 reachable test runs, 3,632 sampled, unreachable share 0.092, 5 model seeds.


`all` (n = 3,632, 3,632, 3,632, 3,562 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.067 | 0.223 | 0.459 | 0.550 |
| fzf_scored | top1 | 0.067 | 0.549 | 0.659 | 0.677 |
| heuristic | top1 | 0.076 | 0.289 | 0.496 | 0.581 |
| model | top1 | 0.155 ±0.008 | 0.617 ±0.002 | 0.740 ±0.004 | 0.755 ±0.003 |
| model, shuffled labels (control) | top1 | 0.026 ±0.017 | 0.041 ±0.016 | 0.070 ±0.028 | 0.104 ±0.047 |
| recency | top3 | 0.318 | 0.485 | 0.669 | 0.728 |
| fzf_scored | top3 | 0.318 | 0.752 | 0.808 | 0.810 |
| heuristic | top3 | 0.208 | 0.563 | 0.754 | 0.803 |
| model | top3 | 0.304 ±0.009 | 0.838 ±0.003 | 0.909 ±0.002 | 0.917 ±0.001 |
| recency | top10 | 0.569 | 0.707 | 0.791 | 0.833 |
| fzf_scored | top10 | 0.569 | 0.843 | 0.895 | 0.899 |
| heuristic | top10 | 0.526 | 0.810 | 0.898 | 0.926 |
| model | top10 | 0.552 ±0.006 | 0.935 ±0.002 | 0.971 ±0.001 | 0.975 ±0.000 |
| recency | mrr | 0.234 | 0.386 | 0.581 | 0.655 |
| fzf_scored | mrr | 0.234 | 0.665 | 0.748 | 0.759 |
| heuristic | mrr | 0.202 | 0.462 | 0.643 | 0.708 |
| model | mrr | 0.278 ±0.006 | 0.738 ±0.002 | 0.830 ±0.003 | 0.841 ±0.002 |

`gt10min` (n = 2,458, 2,458, 2,458, 2,412 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.014 | 0.093 | 0.287 | 0.394 |
| fzf_scored | top1 | 0.014 | 0.388 | 0.515 | 0.541 |
| heuristic | top1 | 0.029 | 0.154 | 0.343 | 0.448 |
| model | top1 | 0.122 ±0.007 | 0.510 ±0.006 | 0.652 ±0.006 | 0.672 ±0.005 |
| model, shuffled labels (control) | top1 | 0.018 ±0.010 | 0.032 ±0.011 | 0.062 ±0.027 | 0.096 ±0.046 |
| recency | top3 | 0.116 | 0.283 | 0.517 | 0.603 |
| fzf_scored | top3 | 0.116 | 0.635 | 0.716 | 0.723 |
| heuristic | top3 | 0.087 | 0.392 | 0.640 | 0.714 |
| model | top3 | 0.211 ±0.009 | 0.767 ±0.004 | 0.868 ±0.002 | 0.879 ±0.001 |
| recency | top10 | 0.367 | 0.568 | 0.691 | 0.753 |
| fzf_scored | top10 | 0.367 | 0.768 | 0.845 | 0.851 |
| heuristic | top10 | 0.318 | 0.719 | 0.850 | 0.891 |
| model | top10 | 0.458 ±0.010 | 0.905 ±0.003 | 0.957 ±0.002 | 0.963 ±0.000 |
| recency | mrr | 0.116 | 0.235 | 0.429 | 0.523 |
| fzf_scored | mrr | 0.116 | 0.532 | 0.638 | 0.654 |
| heuristic | mrr | 0.112 | 0.327 | 0.519 | 0.605 |
| model | mrr | 0.219 ±0.005 | 0.656 ±0.004 | 0.768 ±0.004 | 0.783 ±0.003 |

`other_dir` (n = 1,179, 1,179, 1,179, 1,149 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.003 | 0.050 | 0.170 | 0.260 |
| fzf_scored | top1 | 0.003 | 0.256 | 0.370 | 0.398 |
| heuristic | top1 | 0.006 | 0.102 | 0.205 | 0.274 |
| model | top1 | 0.075 ±0.007 | 0.394 ±0.005 | 0.521 ±0.003 | 0.537 ±0.002 |
| model, shuffled labels (control) | top1 | 0.009 ±0.006 | 0.014 ±0.009 | 0.032 ±0.022 | 0.055 ±0.038 |
| recency | top3 | 0.055 | 0.156 | 0.353 | 0.466 |
| fzf_scored | top3 | 0.055 | 0.498 | 0.622 | 0.634 |
| heuristic | top3 | 0.042 | 0.243 | 0.434 | 0.511 |
| model | top3 | 0.135 ±0.007 | 0.604 ±0.005 | 0.751 ±0.005 | 0.766 ±0.004 |
| recency | top10 | 0.182 | 0.399 | 0.582 | 0.685 |
| fzf_scored | top10 | 0.182 | 0.712 | 0.835 | 0.842 |
| heuristic | top10 | 0.141 | 0.510 | 0.707 | 0.782 |
| model | top10 | 0.308 ±0.005 | 0.809 ±0.008 | 0.912 ±0.004 | 0.923 ±0.001 |
| recency | mrr | 0.065 | 0.150 | 0.304 | 0.403 |
| fzf_scored | mrr | 0.065 | 0.412 | 0.530 | 0.550 |
| heuristic | mrr | 0.057 | 0.226 | 0.366 | 0.439 |
| model | mrr | 0.146 ±0.004 | 0.532 ±0.002 | 0.656 ±0.002 | 0.670 ±0.001 |

R1 +0.365 PASS (gt10min/len2, n=2,458) | R2 +0.088 PASS (all/len0, n=3,632) | R3 -0.110 FAIL (all/len0/top3, n=3,632)

Folds, top-1 at length 2 (recency / heuristic / model mean ±sd): fold60 all 0.469 / 0.530 / 0.756 ±0.004, gt10min 0.305 / 0.385 / 0.676 ±0.006; fold70 all 0.484 / 0.532 / 0.766 ±0.003, gt10min 0.313 / 0.379 / 0.677 ±0.003; fold80 all 0.473 / 0.516 / 0.760 ±0.005, gt10min 0.308 / 0.367 / 0.676 ±0.006

#### repetitive 50,000 / 500,000 (stress)

Label `synthetic`; span 1759 days; immediate-repeat share 0.459; holdout: 90,009 reachable test runs, 2,001 sampled, unreachable share 0.100, 5 model seeds.


`all` (n = 2,001, 2,001, 2,001, 2,001 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.512 | 0.546 | 0.572 | 0.595 |
| fzf_scored | top1 | 0.512 | 0.589 | 0.609 | 0.595 |
| heuristic | top1 | 0.282 | 0.504 | 0.548 | 0.570 |
| model | top1 | 0.482 ±0.004 | 0.591 ±0.004 | 0.628 ±0.003 | 0.631 ±0.004 |
| recency | top3 | 0.584 | 0.590 | 0.608 | 0.620 |
| fzf_scored | top3 | 0.584 | 0.620 | 0.636 | 0.623 |
| heuristic | top3 | 0.499 | 0.594 | 0.608 | 0.617 |
| model | top3 | 0.516 ±0.004 | 0.657 ±0.006 | 0.695 ±0.003 | 0.702 ±0.002 |
| recency | top10 | 0.594 | 0.605 | 0.629 | 0.658 |
| fzf_scored | top10 | 0.594 | 0.651 | 0.681 | 0.667 |
| heuristic | top10 | 0.591 | 0.625 | 0.663 | 0.682 |
| model | top10 | 0.543 ±0.004 | 0.724 ±0.003 | 0.761 ±0.005 | 0.767 ±0.005 |
| recency | mrr | 0.550 | 0.572 | 0.596 | 0.617 |
| fzf_scored | mrr | 0.550 | 0.613 | 0.633 | 0.620 |
| heuristic | mrr | 0.397 | 0.556 | 0.590 | 0.607 |
| model | mrr | 0.506 ±0.002 | 0.637 ±0.004 | 0.674 ±0.002 | 0.678 ±0.003 |

`gt10min` (n = 1,007, 1,007, 1,007, 1,007 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.133 | 0.154 | 0.173 | 0.206 |
| fzf_scored | top1 | 0.133 | 0.203 | 0.227 | 0.226 |
| heuristic | top1 | 0.068 | 0.145 | 0.168 | 0.182 |
| model | top1 | 0.107 ±0.004 | 0.230 ±0.007 | 0.288 ±0.008 | 0.293 ±0.009 |
| recency | top3 | 0.178 | 0.188 | 0.220 | 0.245 |
| fzf_scored | top3 | 0.178 | 0.245 | 0.277 | 0.277 |
| heuristic | top3 | 0.137 | 0.199 | 0.221 | 0.239 |
| model | top3 | 0.127 ±0.003 | 0.336 ±0.011 | 0.407 ±0.005 | 0.418 ±0.003 |
| recency | top10 | 0.193 | 0.214 | 0.263 | 0.320 |
| fzf_scored | top10 | 0.193 | 0.307 | 0.365 | 0.364 |
| heuristic | top10 | 0.188 | 0.255 | 0.331 | 0.367 |
| model | top10 | 0.162 ±0.007 | 0.459 ±0.006 | 0.528 ±0.009 | 0.540 ±0.010 |
| recency | mrr | 0.160 | 0.179 | 0.209 | 0.244 |
| fzf_scored | mrr | 0.160 | 0.240 | 0.273 | 0.273 |
| heuristic | mrr | 0.110 | 0.188 | 0.219 | 0.237 |
| model | mrr | 0.129 ±0.002 | 0.307 ±0.007 | 0.371 ±0.005 | 0.378 ±0.006 |

`other_dir` (n = 761, 761, 761, 761 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.122 | 0.141 | 0.158 | 0.189 |
| fzf_scored | top1 | 0.122 | 0.183 | 0.209 | 0.214 |
| heuristic | top1 | 0.083 | 0.150 | 0.156 | 0.160 |
| model | top1 | 0.091 ±0.004 | 0.181 ±0.007 | 0.223 ±0.005 | 0.229 ±0.004 |
| recency | top3 | 0.158 | 0.167 | 0.205 | 0.230 |
| fzf_scored | top3 | 0.158 | 0.231 | 0.264 | 0.269 |
| heuristic | top3 | 0.155 | 0.171 | 0.179 | 0.191 |
| model | top3 | 0.105 ±0.005 | 0.256 ±0.008 | 0.304 ±0.003 | 0.314 ±0.004 |
| recency | top10 | 0.170 | 0.197 | 0.248 | 0.317 |
| fzf_scored | top10 | 0.170 | 0.300 | 0.372 | 0.376 |
| heuristic | top10 | 0.166 | 0.198 | 0.247 | 0.268 |
| model | top10 | 0.129 ±0.009 | 0.355 ±0.007 | 0.427 ±0.012 | 0.439 ±0.012 |
| recency | mrr | 0.144 | 0.163 | 0.194 | 0.230 |
| fzf_scored | mrr | 0.144 | 0.225 | 0.260 | 0.265 |
| heuristic | mrr | 0.118 | 0.171 | 0.185 | 0.195 |
| model | mrr | 0.106 ±0.003 | 0.240 ±0.007 | 0.288 ±0.005 | 0.296 ±0.005 |

R1 +0.116 PASS (gt10min/len2, n=1,007) | R2 -0.030 FAIL (all/len0, n=2,001) | R3 -0.230 FAIL (all/len0/top1, n=2,001)

Folds, top-1 at length 2 (recency / heuristic / model mean ±sd): fold60 all 0.567 / 0.550 / 0.624 ±0.009, gt10min 0.165 / 0.163 / 0.281 ±0.014; fold70 all 0.559 / 0.531 / 0.622 ±0.007, gt10min 0.152 / 0.143 / 0.272 ±0.011; fold80 all 0.558 / 0.527 / 0.615 ±0.006, gt10min 0.130 / 0.126 / 0.252 ±0.011

#### repetitive 2,000 / 20,000 (stress)

Label `synthetic`; span 68 days; immediate-repeat share 0.471; holdout: 3,634 reachable test runs, 3,634 sampled, unreachable share 0.091, 5 model seeds.


`all` (n = 3,634, 3,634, 3,634, 3,634 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.517 | 0.556 | 0.594 | 0.624 |
| fzf_scored | top1 | 0.517 | 0.622 | 0.651 | 0.647 |
| heuristic | top1 | 0.299 | 0.527 | 0.586 | 0.630 |
| model | top1 | 0.494 ±0.005 | 0.688 ±0.004 | 0.739 ±0.004 | 0.748 ±0.002 |
| recency | top3 | 0.595 | 0.609 | 0.639 | 0.672 |
| fzf_scored | top3 | 0.595 | 0.675 | 0.708 | 0.709 |
| heuristic | top3 | 0.522 | 0.639 | 0.698 | 0.732 |
| model | top3 | 0.572 ±0.006 | 0.784 ±0.002 | 0.824 ±0.002 | 0.829 ±0.002 |
| recency | top10 | 0.614 | 0.641 | 0.694 | 0.751 |
| fzf_scored | top10 | 0.614 | 0.744 | 0.799 | 0.804 |
| heuristic | top10 | 0.625 | 0.739 | 0.803 | 0.840 |
| model | top10 | 0.670 ±0.008 | 0.849 ±0.002 | 0.895 ±0.002 | 0.899 ±0.002 |
| recency | mrr | 0.561 | 0.591 | 0.631 | 0.667 |
| fzf_scored | mrr | 0.561 | 0.664 | 0.700 | 0.699 |
| heuristic | mrr | 0.423 | 0.604 | 0.662 | 0.703 |
| model | mrr | 0.552 ±0.005 | 0.747 ±0.002 | 0.794 ±0.002 | 0.801 ±0.001 |

`gt10min` (n = 1,752, 1,752, 1,752, 1,752 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.122 | 0.149 | 0.186 | 0.229 |
| fzf_scored | top1 | 0.122 | 0.225 | 0.279 | 0.284 |
| heuristic | top1 | 0.076 | 0.170 | 0.233 | 0.292 |
| model | top1 | 0.105 ±0.004 | 0.378 ±0.009 | 0.469 ±0.009 | 0.486 ±0.004 |
| recency | top3 | 0.167 | 0.192 | 0.251 | 0.319 |
| fzf_scored | top3 | 0.167 | 0.326 | 0.394 | 0.400 |
| heuristic | top3 | 0.142 | 0.260 | 0.374 | 0.446 |
| model | top3 | 0.194 ±0.003 | 0.555 ±0.003 | 0.636 ±0.004 | 0.646 ±0.003 |
| recency | top10 | 0.199 | 0.256 | 0.366 | 0.484 |
| fzf_scored | top10 | 0.199 | 0.469 | 0.583 | 0.592 |
| heuristic | top10 | 0.222 | 0.459 | 0.591 | 0.668 |
| model | top10 | 0.345 ±0.008 | 0.687 ±0.004 | 0.783 ±0.004 | 0.791 ±0.003 |
| recency | mrr | 0.155 | 0.190 | 0.248 | 0.315 |
| fzf_scored | mrr | 0.155 | 0.309 | 0.379 | 0.385 |
| heuristic | mrr | 0.130 | 0.260 | 0.347 | 0.414 |
| model | mrr | 0.181 ±0.003 | 0.490 ±0.005 | 0.578 ±0.005 | 0.592 ±0.002 |

`other_dir` (n = 1,336, 1,336, 1,336, 1,336 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.095 | 0.115 | 0.154 | 0.195 |
| fzf_scored | top1 | 0.095 | 0.189 | 0.248 | 0.256 |
| heuristic | top1 | 0.068 | 0.140 | 0.179 | 0.219 |
| model | top1 | 0.074 ±0.004 | 0.300 ±0.007 | 0.376 ±0.005 | 0.394 ±0.003 |
| recency | top3 | 0.133 | 0.151 | 0.217 | 0.292 |
| fzf_scored | top3 | 0.133 | 0.300 | 0.378 | 0.386 |
| heuristic | top3 | 0.130 | 0.192 | 0.287 | 0.336 |
| model | top3 | 0.123 ±0.007 | 0.457 ±0.004 | 0.539 ±0.005 | 0.550 ±0.004 |
| recency | top10 | 0.155 | 0.213 | 0.344 | 0.474 |
| fzf_scored | top10 | 0.155 | 0.462 | 0.579 | 0.589 |
| heuristic | top10 | 0.162 | 0.355 | 0.491 | 0.575 |
| model | top10 | 0.240 ±0.012 | 0.598 ±0.004 | 0.715 ±0.005 | 0.726 ±0.004 |
| recency | mrr | 0.125 | 0.154 | 0.218 | 0.286 |
| fzf_scored | mrr | 0.125 | 0.280 | 0.357 | 0.365 |
| heuristic | mrr | 0.110 | 0.208 | 0.278 | 0.330 |
| model | mrr | 0.127 ±0.006 | 0.406 ±0.004 | 0.490 ±0.003 | 0.506 ±0.003 |

R1 +0.283 PASS (gt10min/len2, n=1,752) | R2 -0.023 FAIL (all/len0, n=3,634) | R3 -0.218 FAIL (all/len0/top1, n=3,634)

Folds, top-1 at length 2 (recency / heuristic / model mean ±sd): fold60 all 0.580 / 0.592 / 0.737 ±0.002, gt10min 0.178 / 0.244 / 0.483 ±0.003; fold70 all 0.604 / 0.611 / 0.744 ±0.003, gt10min 0.184 / 0.233 / 0.475 ±0.007; fold80 all 0.595 / 0.577 / 0.745 ±0.003, gt10min 0.182 / 0.234 / 0.482 ±0.009

#### agent 5,000 / 50,000

Label `synthetic`; span 169 days; immediate-repeat share 0.004; holdout: 9,023 reachable test runs, 2,256 sampled, unreachable share 0.098, 5 model seeds.


`all` (n = 2,256, 2,256, 2,256, 2,256 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf_scored | top1 | 0.000 | 0.000 | 0.000 | 0.000 |
| heuristic | top1 | 0.043 | 0.043 | 0.043 | 0.043 |
| model | top1 | 0.045 ±0.014 | 0.047 ±0.016 | 0.047 ±0.016 | 0.046 ±0.015 |
| model, shuffled labels (control) | top1 | 0.005 ±0.005 | 0.003 ±0.004 | 0.004 ±0.004 | 0.004 ±0.004 |
| recency | top3 | 0.078 | 0.078 | 0.078 | 0.078 |
| fzf_scored | top3 | 0.078 | 0.078 | 0.078 | 0.078 |
| heuristic | top3 | 0.129 | 0.129 | 0.129 | 0.129 |
| model | top3 | 0.124 ±0.027 | 0.125 ±0.028 | 0.124 ±0.026 | 0.124 ±0.026 |
| recency | top10 | 0.219 | 0.219 | 0.219 | 0.219 |
| fzf_scored | top10 | 0.219 | 0.219 | 0.219 | 0.219 |
| heuristic | top10 | 0.270 | 0.270 | 0.270 | 0.270 |
| model | top10 | 0.271 ±0.011 | 0.271 ±0.011 | 0.270 ±0.008 | 0.270 ±0.009 |
| recency | mrr | 0.069 | 0.069 | 0.069 | 0.069 |
| fzf_scored | mrr | 0.069 | 0.069 | 0.069 | 0.069 |
| heuristic | mrr | 0.120 | 0.120 | 0.120 | 0.120 |
| model | mrr | 0.119 ±0.016 | 0.120 ±0.017 | 0.119 ±0.016 | 0.119 ±0.016 |

`gt10min` (n = 1,999, 1,999, 1,999, 1,999 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf_scored | top1 | 0.000 | 0.000 | 0.000 | 0.000 |
| heuristic | top1 | 0.017 | 0.017 | 0.017 | 0.017 |
| model | top1 | 0.029 ±0.019 | 0.030 ±0.020 | 0.030 ±0.020 | 0.029 ±0.019 |
| model, shuffled labels (control) | top1 | 0.005 ±0.005 | 0.003 ±0.004 | 0.004 ±0.004 | 0.004 ±0.004 |
| recency | top3 | 0.030 | 0.030 | 0.030 | 0.030 |
| fzf_scored | top3 | 0.030 | 0.030 | 0.030 | 0.030 |
| heuristic | top3 | 0.064 | 0.064 | 0.064 | 0.064 |
| model | top3 | 0.087 ±0.038 | 0.088 ±0.038 | 0.087 ±0.037 | 0.086 ±0.036 |
| recency | top10 | 0.125 | 0.125 | 0.125 | 0.125 |
| fzf_scored | top10 | 0.125 | 0.125 | 0.125 | 0.125 |
| heuristic | top10 | 0.189 | 0.189 | 0.189 | 0.189 |
| model | top10 | 0.222 ±0.022 | 0.222 ±0.022 | 0.221 ±0.020 | 0.221 ±0.020 |
| recency | mrr | 0.040 | 0.040 | 0.040 | 0.040 |
| fzf_scored | mrr | 0.040 | 0.040 | 0.040 | 0.040 |
| heuristic | mrr | 0.075 | 0.075 | 0.075 | 0.075 |
| model | mrr | 0.091 ±0.023 | 0.092 ±0.024 | 0.092 ±0.024 | 0.092 ±0.024 |

`other_dir` (n = 878, 878, 878, 878 at lengths 0-3)

| ranker | metric | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|---|
| recency | top1 | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf_scored | top1 | 0.000 | 0.000 | 0.000 | 0.000 |
| heuristic | top1 | 0.001 | 0.001 | 0.001 | 0.001 |
| model | top1 | 0.006 ±0.004 | 0.005 ±0.004 | 0.005 ±0.004 | 0.006 ±0.004 |
| model, shuffled labels (control) | top1 | 0.001 ±0.001 | 0.000 ±0.001 | 0.000 ±0.001 | 0.001 ±0.001 |
| recency | top3 | 0.002 | 0.002 | 0.002 | 0.002 |
| fzf_scored | top3 | 0.002 | 0.002 | 0.002 | 0.002 |
| heuristic | top3 | 0.003 | 0.003 | 0.003 | 0.003 |
| model | top3 | 0.014 ±0.011 | 0.014 ±0.012 | 0.014 ±0.012 | 0.013 ±0.011 |
| recency | top10 | 0.007 | 0.007 | 0.007 | 0.007 |
| fzf_scored | top10 | 0.007 | 0.007 | 0.007 | 0.007 |
| heuristic | top10 | 0.011 | 0.011 | 0.011 | 0.011 |
| model | top10 | 0.046 ±0.019 | 0.047 ±0.020 | 0.048 ±0.021 | 0.048 ±0.021 |
| recency | mrr | 0.006 | 0.006 | 0.006 | 0.006 |
| fzf_scored | mrr | 0.006 | 0.006 | 0.006 | 0.006 |
| heuristic | mrr | 0.011 | 0.011 | 0.011 | 0.011 |
| model | mrr | 0.022 ±0.008 | 0.022 ±0.008 | 0.022 ±0.008 | 0.022 ±0.008 |

R1 +0.030 FAIL (gt10min/len2, n=1,999) | R2 +0.029 PASS (gt10min/len0, n=1,999) | R3 +0.001 PASS (other_dir/len0/top1, n=878)

Folds, top-1 at length 2 (recency / heuristic / model mean ±sd): fold60 all 0.000 / 0.054 / 0.070 ±0.009, gt10min 0.000 / 0.020 / 0.058 ±0.005; fold70 all 0.000 / 0.051 / 0.054 ±0.022, gt10min 0.000 / 0.012 / 0.038 ±0.025; fold80 all 0.000 / 0.045 / 0.050 ±0.017, gt10min 0.000 / 0.011 / 0.033 ±0.020
