# Ctrl-R history search: benchmark results log

Format: [04-bench.md section 8](04-bench.md#8-results-log-format). Newest
entry first; one entry per benchmark run used for a decision. Targets are
defined in [00-blueprint.md section 2](00-blueprint.md#2-success-metrics).
Every figure below is an aggregate; no command or query text.

## 2026-11-17 - be20d21 - fzf's order as the cold start; model sampler, features and query mix (kept)

The changes, each measured on its own first (variants below), then together:

The code measured is be20d21 (a3f25a5 plus the review fixes).

1. **Cold start = fzf's Ctrl-R order.** Without a model the ranking is now
   fzf's own (`--scheme=history`: fzf's match score, ties newest first;
   newest first for an empty query), from a reimplementation of fzf v0.74.5
   (`src/cmd/fzf.rs`, [02 section 7](02-browse.md#7-heuristic-ranking)). It
   replaces the hand-weighted heuristic. In stage 2 the model now scores
   fzf's top `CMD_STAGE2_K`.
2. **Model features 24-25**: `fzf_score`, beside the Skim `fuzzy_score`
   (measured in place of it too: variant B), and `recency_rank` (newer
   matching commands, capped at 15).
3. **Training negatives**: 30 uniform random matching commands, no fixed
   "15 most recent" ones (`CMD_TRAIN_RECENT_NEGATIVES` 15 -> 0,
   `CMD_TRAIN_RANDOM_NEGATIVES` 15 -> 30); for the empty query drawn from
   the `CMD_STAGE2_K` newest commands, the rows stage 1 gives the model.
4. **Training queries**: 25% of the non-empty ones are a fragment of a later
   word (`CMD_TRAIN_FRAGMENT_SHARE` 25), the rest prefixes as before.
5. **Benchmark**: a second query style (fragments of a later word, lengths
   1, 2, 3, 5), a true-fzf row, stage-1 recall, and a Bayes ceiling for the
   synthetic generator ([04 section 5](04-bench.md#5-relevance-benchmark)).
   Prefix stays the headline and the verdicts read it only.
6. A model with an older feature list is refused (heuristic ranking) and
   retrained at the next picker start instead of after the hour's interval.

Corpora: synthetic only. Tuned on generator seed 1 (interactive 50k and 2k,
repetitive 50k and 2k, agent 5k); confirmed on seed 7 (same sizes and
profiles), which was not looked at while tuning. The two small real corpora
(`cmd-real`, 500 imported runs; `cmd-agent`, agent transcripts) lived in
`/tmp`, which a reboot cleared; they could not be rebuilt and their rows
were not re-run (they decided nothing before either). "Before" is the
parent commit's ranking measured by this commit's benchmark (the benchmark
change alone, built in a separate worktree), so both columns have the same
rows, query styles and lookups. Seeds 5; the tuning corpora with `--folds`.
The relevance runs shared the machine with other benchmark jobs (load
50-150), which changes run time, not results (the benchmark is
deterministic per seed).

### What fzf does, and what was adopted

Read from fzf v0.74.5 (commit `c0a193e`): `src/algo/algo.go`
(`FuzzyMatchV2`, `FuzzyMatchV1`, `calculateScore`, `Init`), `src/pattern.go`
(`BuildPattern`, `parseTerms`, `extendedMatch`), `src/options.go`
(`parseScheme`), `src/result.go`, `src/core.go` and
`shell/key-bindings.{zsh,bash,fish}`.

| fzf | Adopted |
|---|---|
| Ctrl-R widgets: history newest first, duplicates removed keeping the newest, `--scheme=history`, the typed buffer as the query, `ctrl-r` toggles sorting | The order: the index holds distinct commands; sorted by score, ties newest first; an empty query lists newest first. The sort toggle is not a picker feature (out of scope) |
| `--scheme=history`: sort criteria `[byScore]` only; ties keep the input order | Yes: ties by `last_run_at`, then `db_id` |
| Score: `FuzzyMatchV2` (16 per character, gaps -3/-1, boundary 8, non-word 8, camel/digit 7, consecutive at least 4 and carrying the chunk's first bonus, first character's bonus doubled); history scheme makes the start of the text and a word after whitespace or `/,:;\|` all 8 (default scheme: 10 and 9) | Yes, exactly; checked against fzf's own score vectors and the binary |
| V1 fallback for long texts (`chars * pattern chars > 100 * 1024`) | Yes |
| Extended search: space-separated terms, all must match, scores summed, smart case per term, `\ ` a literal space | Yes, for fuzzy terms |
| Extended operators `'exact`, `^prefix`, `suffix$`, `!not`, `\|` | No: they change which commands match, and matching is the worker's filter (Skim), which the speed work keeps as it is. Recommendation below |
| Latin normalisation (`é` = `e`) | No, for the same reason (the filter does not normalise) |

Where Skim (the filter and the old `fzf_scored` row) and fzf differ: Skim
gives the first character of the text 16 more than any later word start
(fzf history: equal); Skim matches a multi-word query as one subsequence
with the spaces, fzf as independent terms in any order (Skim's matches are
a subset); Skim does not normalise. On prefix queries that Skim bias is
worth 0.12 top-1 at length 1 (0.518 vs fzf 0.395 on interactive 50k); on
fragment queries it costs 0.27 (0.058 vs 0.324 at length 2).

Recommendation (not done; needs the worker's filter): make the candidate
filter fzf's too (extended operators, normalisation, terms in any order).
The merge with the speed work must reconcile: `rank.rs` scores every
candidate with `FzfQuery` (one more pass of a DP matcher over the
candidates, measured below); the worker still filters with Skim and passes
`CommandCandidate { idx, fuzzy_score }` unchanged; `CommandRanking` and
`HeuristicParts` changed shape (`HeuristicParts { recency, fzf }`), which
`worker.rs` only copies, and `render.rs` shows; `rank_with` now returns
the newest candidates to the model stage internally (no worker-visible
change). If the filter switched to fzf, `rank.rs` could take the fzf score
from the filter instead of computing it again.

### Headline (interactive 50,000 / 500,000, holdout, prefix queries, top-1; model mean ±sd over 5 seeds)

| holdout | recency | fzf_scored (Skim) | fzf | heuristic before | heuristic after (= fzf) | model before | model after |
|---|---|---|---|---|---|---|---|
| gt10min len2 top-1 | 0.236 | 0.448 | 0.435 | 0.184 | 0.435 | 0.432 ±0.015 | 0.530 ±0.007 |
| all len0 top-1 | 0.065 | 0.065 | 0.065 | 0.065 | 0.065 | 0.154 ±0.010 | 0.151 ±0.009 |
| all len0 top-10 | 0.528 | 0.528 | 0.528 | 0.419 | 0.528 | 0.340 ±0.038 | 0.592 |
| all len1 top-1 | 0.220 | 0.518 | 0.395 | 0.237 | 0.395 | 0.510 ±0.013 | 0.533 ±0.011 |
| all len2 top-1 | 0.415 | 0.612 | 0.599 | 0.380 | 0.599 | 0.575 ±0.012 | 0.635 ±0.006 |
| all len3 top-1 | 0.507 | 0.623 | 0.621 | 0.427 | 0.621 | 0.587 ±0.014 | 0.648 ±0.009 |
| all len2 MRR | 0.527 | 0.684 | 0.671 | 0.493 | 0.671 | 0.657 ±0.009 | 0.736 ±0.004 |
| other_dir len2 top-1 | 0.157 | 0.354 | 0.345 | 0.124 | 0.345 | 0.319 ±0.023 | 0.447 ±0.016 |
| all, fragment len2 top-1 | 0.251 | 0.058 | 0.324 | 0.229 | 0.324 | 0.008 ±0.003 | 0.265 ±0.008 |

R1 +0.295 PASS (was +0.196) | R2 +0.085 PASS (was +0.080) | R3 +0.000 PASS (was -0.262 FAIL) | unreachable 0.099
B1 not re-measured in the pty (see "Cost"); `latency` total p95 6.9-7.8 ms (was 4.4-5.1) | B6 10.6 s PASS (was 4.6 s) | B8 0.9815 / 1.0000 PASS (was 1.0000 / 1.0000)
Synthetic guard (repetitive 50k): R2 -0.003 PASS (was -0.030 FAIL), R3 +0.000 PASS (was -0.230 FAIL).
Decision: kept. Thresholds of 04 section 7.3: the model's target cell (gt10min len2 top-1) moved +0.098 against a threshold of max(0.01, 2*sqrt(0.015^2/5 + 0.007^2/5)) = 0.015.

R3 cannot be positive any more: with an empty query the heuristic is
newest-first, which is the baseline, so R3's worst cell is 0.000 by
construction. The verdicts on every corpus:

| Corpus | R1 before -> after | R2 before -> after | R3 before -> after |
|---|---|---|---|
| interactive 50k | +0.196 PASS -> +0.295 PASS | +0.080 PASS -> +0.085 PASS | -0.262 FAIL -> +0.000 PASS |
| interactive 2k | +0.365 PASS -> +0.349 PASS | +0.088 PASS -> +0.094 PASS | -0.110 FAIL -> +0.000 PASS |
| repetitive 50k | +0.116 PASS -> +0.169 PASS | -0.030 FAIL -> -0.003 PASS | -0.230 FAIL -> +0.000 PASS |
| repetitive 2k | +0.283 PASS -> +0.244 PASS | -0.023 FAIL -> -0.004 PASS | -0.218 FAIL -> +0.000 PASS |
| agent 5k | +0.030 FAIL -> +0.058 FAIL | +0.029 PASS -> +0.053 PASS | +0.001 PASS -> +0.000 PASS |

The targets of the task, checked:

| Target | Result |
|---|---|
| Cold start >= max(recency, fzf_scored) - 1 point on every headline cell | Met against recency and true fzf everywhere (it is fzf). Not met against `fzf_scored` on prefix queries of length 1 (interactive 50k 0.395 vs 0.518; 2k 0.424 vs 0.549): that row is Skim's score, whose bonus for the first character of the text is exactly what a prefix query rewards; on fragment queries the same row is 0.27 below fzf (0.058 vs 0.324 at length 2). Pure fzf was the instruction once the user asked for fzf; each psychic signal added as a tie-break lost to it somewhere (below). On the agent corpus the old heuristic was better (top-1 0.043 vs 0.000 at every prefix length): every agent command starts `cd /synth/repoN &&`, so a 1-3 character prefix is uninformative and only the old directory term separated them |
| Model >= every baseline on every headline cell of the interactive corpora | Met on prefix `all` and `gt10min`, lengths 0-3, top-1/top-10/MRR, on interactive 50k and 2k and on both held-out interactive corpora (the closest: 50k length 1 top-1 0.533 vs `fzf_scored` 0.518, seed 7 0.529 vs 0.508) |
| R2 on the repetitive corpora | PASS: length 0 top-1 within 0.4 point of recency (50k 0.509 vs 0.512; 2k 0.513 vs 0.517; seed 7: 50k 0.508 vs 0.511, 2k -0.9 point). Recency is at the Bayes ceiling there (0.502-0.519), so tying it is the best possible |
| Length 0 on the interactive corpus: top-10 above recency | Met: 0.592 vs 0.528 (was 0.340) |
| Approach file search (top-1 0.80, MRR 0.87) | Not reached and not reachable on these corpora: the Bayes ceiling of interactive 50k at length 2 is top-1 0.72, MRR 0.80 (below) |
| Fragment queries | The model is far better than before (length 2: 0.008 -> 0.265) but still below its own cold start, fzf, at lengths 1-3 on interactive 50k (0.096 / 0.265 / 0.275 vs 0.261 / 0.324 / 0.329) and at length 1 on every corpus; equal at length 5 (0.381 vs 0.417 at 50k; 0.472 vs 0.450 at 2k). Loading a model makes short fragment queries worse than no model |

### Ceiling: how good could any ranker be on these corpora

Bayes ceiling (`src/cmd/corpus/ceiling.rs`, [04 section 5.5](04-bench.md#55-metrics-and-verdicts)):
the generator's own probability of each next command given its exact
state, ranked among commands with the query's first characters; about
1,800 reused runs of the last 20%. Prefix queries; top-1 / MRR.

| Corpus (seed 1) | len 0 | len 1 | len 2 | len 3 | model after, len 2 top-1 |
|---|---|---|---|---|---|
| interactive 50k | 0.207 / 0.344 | 0.629 / 0.723 | 0.724 / 0.804 | 0.741 / 0.816 | 0.635 |
| interactive 2k | 0.184 / 0.344 | 0.666 / 0.784 | 0.791 / 0.873 | 0.812 / 0.888 | 0.731 |
| repetitive 50k | 0.502 / 0.569 | 0.677 / 0.725 | 0.717 / 0.764 | 0.721 / 0.769 | see tables |
| repetitive 2k | 0.519 / 0.598 | 0.741 / 0.807 | 0.805 / 0.860 | 0.809 / 0.864 | 0.718 |
| agent 5k | 0.106 / 0.195 at every length | | | | 0.083 |

Seed 7 is within 0.03 of these (interactive 50k len 2: 0.700 / 0.784).
So on interactive 50k at two characters the model is 0.09 below the best
possible (0.635 vs 0.724), and the best possible is itself 0.08 below file
search's 0.80: picking the next of 50,000 commands from two characters is
a harder problem than file search's benchmark, and no ranker over this
history closes that. Other limits found:

- Stage-1 recall: the model scores only fzf's first 2,000 rows. The target
  is outside them in 13% of empty-query lookups (it was last run before the
  2,000 newest) and 5% at one character on interactive 50k (the old
  heuristic, which weighted frequency and directory, lost 0.4% there); 13%
  at one-character fragments. Every one of those is a miss for the model.
  `CMD_STAGE2_K` or a second stage-1 source (the most frequent matching
  commands) is the lever; not tried (cost, below).
- A third information row, "how often is the target's feature vector
  identical to another stage-2 row's", was 1.000 on every cell: the last
  run time alone separates all commands, so the features never tie and the
  gap is in the ranking function and the noise, not in indistinguishable
  candidates. The row was removed.

### What each change contributed (interactive 50k / 2k, model top-1, prefix unless noted)

Variants, measured on the holdout split, 5 seeds; "before" is the parent.
Columns: prefix length 0 top-1 and top-10, lengths 1-3 top-1, `gt10min`
and `other_dir` at length 2, fragments of length 2 and 5.

| variant | corpus | p0 top1 | p0 top10 | p1 | p2 | p3 | gt10min p2 | other_dir p2 | f2 | f5 |
|---|---|---|---|---|---|---|---|---|---|---|
| baselines recency / fzf_scored / fzf | i50k | 0.065/0.065/0.065 | 0.528/0.528/0.528 | 0.220/0.518/0.395 | 0.415/0.612/0.599 | 0.507/0.623/0.621 | 0.236/0.448/0.435 | 0.157/0.354/0.345 | 0.251/0.058/0.324 | 0.361/0.414/0.417 |
| before: parent | i50k | 0.154 | 0.340 | 0.510 | 0.575 | 0.587 | 0.432 | 0.319 | 0.008 | 0.096 |
| A: fzf cold start + fzf_score | i50k | 0.160 | 0.411 | 0.521 | 0.581 | 0.593 | 0.441 | 0.328 | 0.088 | 0.107 |
| B: A, Skim score zeroed | i50k | 0.152 | 0.403 | 0.520 | 0.587 | 0.598 | 0.448 | 0.336 | 0.090 | 0.111 |
| D: A + 50% fragments | i50k | 0.153 | 0.413 | 0.505 | 0.585 | 0.600 | 0.484 | 0.391 | 0.219 | 0.311 |
| J: A + recency_rank + uniform negatives | i50k | 0.118 | 0.538 | 0.524 | 0.629 | 0.649 | 0.520 | 0.442 | 0.206 | 0.319 |
| K: J + 50% fragments | i50k | 0.116 | 0.543 | 0.483 | 0.616 | 0.638 | 0.516 | 0.433 | 0.247 | 0.383 |
| L: D + hard negatives | i50k | 0.153 | 0.419 | 0.509 | 0.597 | 0.613 | 0.496 | 0.388 | 0.231 | 0.325 |
| M: J + 25% fragments | i50k | 0.121 | 0.545 | 0.513 | 0.627 | 0.646 | 0.520 | 0.440 | 0.280 | 0.408 |
| N: M + empty-query negatives from the 2,000 newest (shipped) | i50k | 0.151 | 0.592 | 0.533 | 0.635 | 0.648 | 0.530 | 0.447 | 0.265 | 0.381 |
| baselines recency / fzf_scored / fzf | ismall | 0.067/0.067/0.067 | 0.569/0.569/0.569 | 0.223/0.549/0.424 | 0.459/0.659/0.644 | 0.550/0.677/0.675 | 0.287/0.515/0.496 | 0.170/0.370/0.355 | 0.268/0.065/0.349 | 0.392/0.448/0.450 |
| before: parent | ismall | 0.155 | 0.552 | 0.617 | 0.740 | 0.755 | 0.652 | 0.521 | 0.023 | 0.199 |
| A: fzf cold start + fzf_score | ismall | 0.155 | 0.545 | 0.612 | 0.735 | 0.751 | 0.654 | 0.520 | 0.068 | 0.244 |
| B: A, Skim score zeroed | ismall | 0.155 | 0.547 | 0.613 | 0.735 | 0.751 | 0.651 | 0.514 | 0.051 | 0.241 |
| C: A + hard negatives | ismall | 0.157 | 0.543 | 0.616 | 0.736 | 0.753 | 0.650 | 0.515 | 0.043 | 0.216 |
| D: A + 50% fragments | ismall | 0.156 | 0.546 | 0.590 | 0.723 | 0.739 | 0.643 | 0.512 | 0.334 | 0.460 |
| H: A + recency_rank, recent negatives kept | ismall | 0.043 | 0.221 | 0.515 | 0.687 | 0.717 | 0.598 | 0.447 | 0.023 | 0.120 |
| I: A + uniform negatives | ismall | 0.144 | 0.617 | 0.597 | 0.721 | 0.744 | 0.625 | 0.492 | 0.166 | 0.397 |
| J: A + recency_rank + uniform negatives | ismall | 0.164 | 0.627 | 0.608 | 0.730 | 0.749 | 0.638 | 0.505 | 0.101 | 0.334 |
| K: J + 50% fragments | ismall | 0.160 | 0.630 | 0.583 | 0.725 | 0.742 | 0.633 | 0.501 | 0.361 | 0.476 |
| L: D + hard negatives | ismall | 0.147 | 0.545 | 0.593 | 0.724 | 0.742 | 0.643 | 0.501 | 0.342 | 0.466 |
| M: J + 25% fragments | ismall | 0.159 | 0.626 | 0.598 | 0.725 | 0.742 | 0.631 | 0.499 | 0.330 | 0.471 |
| N: M + empty-query negatives from the 2,000 newest (shipped) | ismall | 0.161 | 0.631 | 0.606 | 0.731 | 0.748 | 0.636 | 0.509 | 0.348 | 0.472 |

- **A (fzf cold start + `fzf_score`)**: the cold start passes R3
  everywhere; the model hardly moves (it is still trained on the biased
  sampler). **B** (fzf score in place of Skim's): no difference beyond
  noise (0.587 vs 0.581 at length 2), so `fzf_score` sits beside
  `fuzzy_score` and both stay.
- **C, L (hard negatives: the best fzf scores among recent matches)**: no
  gain; rejected.
- **H** exposed the real problem. With the 15 newest matching commands as
  fixed negatives, `recency_rank` 0-14 was almost always a negative and
  15-19 almost never (pos/neg histogram of 3,202 empty-query groups: rank 1
  559/2,643, ranks 16-19 30-39 positive vs 33-54 negative), and the model
  ranked those commands first: top-1 0.043. The same skew, smoothed, is in
  every recency feature, which is why the model underrated recent commands
  at 50,000 and lost to `fzf_scored`.
- **I, J (uniform negatives)**: length-0 top-10 0.41 -> 0.54-0.62, lengths
  2-3 +0.05 at 50k, `gt10min` +0.08, `other_dir` +0.11; length-0 top-1
  fell (0.160 -> 0.118), fixed by **N**: for the empty query the negatives
  come from the 2,000 newest, the rows stage 1 actually hands the model
  (top-1 0.151, top-10 0.592, MRR 0.288; lengths 1-2 +0.01-0.02 too).
- **D, K, M (fragment training queries, 50 / 50 / 25%)**: fragments of
  length 2 0.09 -> 0.22-0.28 at 50k; at 50% prefix length 1 lost 0.04
  (0.483 vs 0.524), at 25% it is within noise of none. 25% shipped.
- Rejected cold-start tie-breaks inside equal fzf scores (harness over all
  five corpora, both styles, every cell of `all`/`gt10min`/`other_dir` x
  lengths x 4 metrics, against pure fzf): all-time runs (weights 0.05, 0.2:
  -0.09 / -0.17 at length 0 on repetitive 50k), runs in 30 days (-0.16),
  directory share (-0.07 on interactive 50k), recency in this directory
  (-0.006, the mildest), recency scale 900 s (-0.04). None ships.

### Held-out generator seed (7; not looked at while tuning; holdout, 5 seeds, top-1)

#### interactive 50,000 / 500,000, generator seed 7 (held out)

| ranker | p0 | p1 | p2 | p3 | gt10min p2 | other_dir p2 | f1 | f2 | f3 | f5 |
|---|---|---|---|---|---|---|---|---|---|---|
| recency | 0.062 | 0.217 | 0.408 | 0.489 | 0.243 | 0.131 | 0.165 | 0.255 | 0.304 | 0.359 |
| fzf_scored (Skim) | 0.062 | 0.508 | 0.594 | 0.602 | 0.444 | 0.323 | 0.056 | 0.064 | 0.243 | 0.397 |
| fzf = heuristic after | 0.062 | 0.389 | 0.581 | 0.608 | 0.431 | 0.307 | 0.250 | 0.329 | 0.334 | 0.397 |
| heuristic before | 0.054 | 0.218 | 0.348 | 0.398 | 0.180 | 0.083 | 0.143 | 0.234 | 0.258 | 0.301 |
| model before | 0.166 ±0.006 | 0.529 ±0.005 | 0.597 ±0.005 | 0.613 ±0.005 | 0.482 ±0.006 | 0.372 ±0.009 | 0.027 ±0.001 | 0.021 ±0.009 | 0.061 ±0.013 | 0.135 ±0.021 |
| model after | 0.149 ±0.006 | 0.529 ±0.011 | 0.634 ±0.009 | 0.645 ±0.009 | 0.541 ±0.010 | 0.438 ±0.016 | 0.125 ±0.015 | 0.285 ±0.021 | 0.281 ±0.026 | 0.356 ±0.043 |

top-1. Before: R1 +0.239 PASS | R2 +0.104 PASS | R3 -0.269 FAIL. After: R1 +0.298 PASS | R2 +0.087 PASS | R3 +0.000 PASS.

#### interactive 2,000 / 20,000, generator seed 7 (held out)

| ranker | p0 | p1 | p2 | p3 | gt10min p2 | other_dir p2 | f1 | f2 | f3 | f5 |
|---|---|---|---|---|---|---|---|---|---|---|
| recency | 0.069 | 0.230 | 0.451 | 0.553 | 0.275 | 0.161 | 0.187 | 0.280 | 0.321 | 0.401 |
| fzf_scored (Skim) | 0.069 | 0.559 | 0.662 | 0.674 | 0.518 | 0.350 | 0.053 | 0.066 | 0.278 | 0.459 |
| fzf = heuristic after | 0.069 | 0.429 | 0.642 | 0.676 | 0.494 | 0.330 | 0.273 | 0.363 | 0.370 | 0.459 |
| heuristic before | 0.072 | 0.292 | 0.478 | 0.572 | 0.326 | 0.204 | 0.200 | 0.285 | 0.334 | 0.420 |
| model before | 0.175 ±0.004 | 0.622 ±0.005 | 0.737 ±0.005 | 0.760 ±0.003 | 0.643 ±0.006 | 0.478 ±0.003 | 0.032 ±0.001 | 0.019 ±0.004 | 0.092 ±0.012 | 0.220 ±0.012 |
| model after | 0.164 ±0.008 | 0.608 ±0.007 | 0.736 ±0.008 | 0.758 ±0.008 | 0.644 ±0.012 | 0.490 ±0.020 | 0.149 ±0.018 | 0.365 ±0.017 | 0.382 ±0.012 | 0.471 ±0.009 |

top-1. Before: R1 +0.369 PASS | R2 +0.106 PASS | R3 -0.127 FAIL. After: R1 +0.370 PASS | R2 +0.094 PASS | R3 +0.000 PASS.

#### repetitive 50,000 / 500,000, generator seed 7 (held out)

| ranker | p0 | p1 | p2 | p3 | gt10min p2 | other_dir p2 | f1 | f2 | f3 | f5 |
|---|---|---|---|---|---|---|---|---|---|---|
| recency | 0.511 | 0.544 | 0.569 | 0.583 | 0.180 | 0.149 | 0.527 | 0.542 | 0.550 | 0.566 |
| fzf_scored (Skim) | 0.511 | 0.579 | 0.592 | 0.581 | 0.215 | 0.184 | 0.051 | 0.074 | 0.451 | 0.577 |
| fzf = heuristic after | 0.511 | 0.568 | 0.589 | 0.596 | 0.211 | 0.180 | 0.537 | 0.551 | 0.552 | 0.577 |
| heuristic before | 0.286 | 0.489 | 0.538 | 0.561 | 0.170 | 0.138 | 0.331 | 0.413 | 0.434 | 0.509 |
| model before | 0.480 ±0.002 | 0.592 ±0.006 | 0.621 ±0.005 | 0.624 ±0.006 | 0.291 ±0.009 | 0.201 ±0.006 | 0.038 ±0.001 | 0.011 ±0.006 | 0.212 ±0.021 | 0.336 ±0.005 |
| model after | 0.508 ±0.002 | 0.595 ±0.007 | 0.644 ±0.003 | 0.652 ±0.002 | 0.333 ±0.010 | 0.280 ±0.003 | 0.522 ±0.018 | 0.563 ±0.003 | 0.567 ±0.002 | 0.615 ±0.007 |

top-1. Before: R1 +0.110 PASS | R2 -0.031 FAIL | R3 -0.224 FAIL. After: R1 +0.153 PASS | R2 -0.003 PASS | R3 +0.000 PASS.

#### repetitive 2,000 / 20,000, generator seed 7 (held out)

| ranker | p0 | p1 | p2 | p3 | gt10min p2 | other_dir p2 | f1 | f2 | f3 | f5 |
|---|---|---|---|---|---|---|---|---|---|---|
| recency | 0.507 | 0.554 | 0.594 | 0.624 | 0.200 | 0.163 | 0.525 | 0.541 | 0.559 | 0.595 |
| fzf_scored (Skim) | 0.507 | 0.619 | 0.648 | 0.635 | 0.286 | 0.257 | 0.065 | 0.054 | 0.435 | 0.616 |
| fzf = heuristic after | 0.507 | 0.595 | 0.645 | 0.656 | 0.282 | 0.252 | 0.543 | 0.562 | 0.564 | 0.616 |
| heuristic before | 0.304 | 0.532 | 0.610 | 0.653 | 0.264 | 0.209 | 0.363 | 0.440 | 0.467 | 0.566 |
| model before | 0.489 ±0.003 | 0.686 ±0.006 | 0.743 ±0.003 | 0.754 ±0.004 | 0.492 ±0.005 | 0.399 ±0.009 | 0.056 ±0.001 | 0.137 ±0.029 | 0.408 ±0.019 | 0.504 ±0.012 |
| model after | 0.493 ±0.013 | 0.652 ±0.020 | 0.729 ±0.017 | 0.752 ±0.007 | 0.462 ±0.033 | 0.394 ±0.028 | 0.528 ±0.013 | 0.576 ±0.008 | 0.591 ±0.003 | 0.666 ±0.001 |

top-1. Before: R1 +0.293 PASS | R2 -0.018 FAIL | R3 -0.203 FAIL. After: R1 +0.262 PASS | R2 -0.014 FAIL | R3 +0.000 PASS.

#### agent 5,000 / 50,000, generator seed 7 (held out)

| ranker | p0 | p1 | p2 | p3 | gt10min p2 | other_dir p2 | f1 | f2 | f3 | f5 |
|---|---|---|---|---|---|---|---|---|---|---|
| recency | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.003 | 0.009 | 0.004 | 0.019 |
| fzf_scored (Skim) | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.011 | 0.037 | 0.023 | 0.050 |
| fzf = heuristic after | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.009 | 0.036 | 0.023 | 0.050 |
| heuristic before | 0.046 | 0.046 | 0.046 | 0.046 | 0.017 | 0.000 | 0.051 | 0.068 | 0.056 | 0.081 |
| model before | 0.050 ±0.014 | 0.051 ±0.015 | 0.051 ±0.015 | 0.047 ±0.020 | 0.040 ±0.014 | 0.005 ±0.003 | 0.052 ±0.015 | 0.059 ±0.017 | 0.053 ±0.021 | 0.066 ±0.024 |
| model after | 0.085 ±0.005 | 0.086 ±0.004 | 0.084 ±0.005 | 0.084 ±0.005 | 0.059 ±0.008 | 0.004 ±0.003 | 0.095 ±0.004 | 0.113 ±0.006 | 0.112 ±0.006 | 0.123 ±0.006 |

top-1. Before: R1 +0.040 FAIL | R2 +0.037 PASS | R3 -0.005 FAIL. After: R1 +0.059 FAIL | R2 +0.059 PASS | R3 +0.000 PASS.

The held-out corpora agree with the tuning ones in every direction: on
interactive 50k the model is above every baseline at prefix lengths 1-3
(length 2: 0.634 vs `fzf_scored` 0.594), length 0 top-1 is a little lower
than before (0.149 vs 0.166), fragments are far better than before and
still below fzf at lengths 1-3.

### Leakage controls

| Check | Result |
|---|---|
| Shuffled labels, interactive 50k and 2k, 5 seeds | model top-1 0.012-0.025 at prefix lengths 0-3 on 50k (0.018-0.062 on 2k), far below every baseline: the gain is learned from labels |
| Train-before-cut, own-run-not-in-index, replay = fresh load (IT-N-02, now also for `recency_rank`'s newest list and every feature) | pass (`cargo test`) |
| Folds, interactive 50k, length 2 top-1, all / gt10min (recency, fzf_scored, heuristic, model) | fold60 0.401, 0.589, 0.568, 0.631 ±0.009 / 0.229, 0.433, 0.412, 0.529 ±0.003; fold70 0.412, 0.617, 0.602, 0.648 ±0.009 / 0.238, 0.470, 0.452, 0.558 ±0.009; fold80 0.405, 0.593, 0.577, 0.624 ±0.006 / 0.232, 0.434, 0.416, 0.512 ±0.004 (before: model 0.611 / 0.496, 0.624 / 0.514, 0.571 / 0.427) |

### Cost (perf DB = interactive 50k, model pinned by `setup`; `latency 300`, twice each, alternating; machine load 20-28 from other jobs)

| Stage, mean / p95 ms | before #1 | before #2 | after #1 | after #2 |
|---|---|---|---|---|
| filter | 1.54 / 3.03 | 1.62 / 3.59 | 1.69 / 3.75 | 1.86 / 4.19 |
| heuristic (fzf scoring + select) | 0.32 / 0.92 | 0.35 / 0.96 | 0.80 / 1.65 | 0.89 / 2.07 |
| features | 0.15 / 0.26 | 0.15 / 0.30 | 0.18 / 0.32 | 0.18 / 0.33 |
| predict | 0.16 / 0.31 | 0.15 / 0.31 | 0.79 / 1.63 | 0.89 / 1.72 |
| blend | 0.17 / 0.31 | 0.19 / 0.36 | 0.19 / 0.39 | 0.21 / 0.52 |
| total (filter + rank) | 2.33 / 4.36 | 2.47 / 5.07 | 3.65 / 6.90 | 4.03 / 7.79 |
| by length, total p50: len 0 / 1 / 2 | 1.70 / 3.67 / 2.56 | 1.67 / 3.53 / 2.50 | 2.97 / 5.41 / 4.11 | 3.58 / 6.42 / 4.37 |

The rank stages cost about 1.3-1.5 ms more at the mean and 2.5-2.7 ms
more at p95. Two causes: fzf's score over every candidate (+0.5 ms mean,
+0.7-1.1 ms p95; a second matcher pass after the filter's), and the
model, which grew from 40 to 200 trees once the sampler stopped handing
it easy negatives (early stopping keeps finding gains): predict +0.65 ms
mean, +1.3-1.4 ms p95. The `latency` mode runs back to back on a warm
pool; in the picker the worker took about twice these figures (Phase D),
so B1 (8 ms p95, already 8.2-9.5 ms) is now further over and was not
re-measured here (the speed work is changing the worker in parallel).
Levers, not tried: a cap on the command model's trees (the relevance gain
at 100 trees is unmeasured), `CMD_STAGE2_K` 1,000, fzf's one-character
fast path and a byte prefilter before decoding a text, or one matcher pass
if the filter becomes fzf's (recommendation above). Training: B6 4.6 ->
10.6 s (budget 60 s), mostly `recency_rank`'s walk of the recency order
for rare queries.

### Review

A reviewer agent (opus) read the diff of a3f25a5 and the evidence without
being told the conclusions. Found and fixed: fzf sorts on the score as an
unsigned 16-bit value, so negative V1 totals (long texts, long gaps) tie at
0; the port now clamps the same way (test against the binary's order).
The length-0 top-1 regression at 50k (0.154 -> 0.121): fixed by drawing
empty-query negatives from the rows stage 1 hands the model (0.151).
Pick groups recomputed the newest-matching walk per impression; once per
query now. A stale comment. Found and left, stated in the specs:
`recency_rank` is counted among the candidates, which the picker's
Directory/Repository/Session scopes narrow, while training counts all
matching commands (the worker would have to pass the scope or the
all-scope count: a merge item for `worker.rs`); non-ASCII case-class
edges of the port. Found and reported here: the fragment training
queries use the same fragment rule as the benchmark (another salt), so
the fragment cells measure the query distribution trained on, not
generalisation to how people really type; `CMD_TRAIN_FRAGMENT_SHARE` was
chosen on the holdout split (the held-out seed confirms the direction);
the 2k corpora lost about 0.02 top-1 at prefix length 1 (interactive 0.617
-> 0.606, repetitive 0.688 -> 0.641, the latter with sd 0.028); training
queries are 0-3 characters, so `fzf_score` values of longer queries are
outside what the trees saw.

### What remains unproven

- Every number is on synthetic histories. The generator's process is known,
  which is what makes the Bayes ceiling possible and also what a model can
  learn too well; the held-out seed checks overfitting to one draw, not to
  the generator. The real sample (500 imported lines) and the agent
  transcripts could not be re-run (cleared with `/tmp`).
- No corpus has a Ctrl-R pick: pick groups, pick features and
  `CMD_PICK_WEIGHT` are untested by the benchmark, and every label is "the
  command run next" with a made-up query.
- Real query style is unknown: prefix and word-start fragments are two
  guesses; a real user also types substrings inside words and several words.
- The fzf cold start matches fzf's own Ctrl-R order, which is a well-used
  default, but no real user has compared it with the old heuristic.
- B1/B2/B3 in the pty were not re-measured; the rank stages are slower.

### How to re-run

As in the first entry, plus: the fragment style and the `fzf` and
`stage1_recall` rows are in every relevance run; the Bayes ceiling is
`CEILING_PROFILE=interactive CEILING_COMMANDS=50000 CEILING_RUNS=500000
CEILING_SEED=1 cargo test --release cmd::corpus::ceiling::bayes_ceiling --
--ignored --nocapture` (seconds); the fzf check against the binary is the
ignored test `cmd::fzf::tests::bulk_order_for_comparison_with_the_binary`
(02 section 7.1). Times on this machine under load 50-150: interactive 50k
holdout 9-14 min, with folds 33 min; 2k 4-9 min.

### Relevance tables, tuning corpora (generator seed 1, holdout, 5 seeds; "before" = parent commit's ranking, same benchmark)

#### interactive 50,000 / 500,000 (headline)

50,000 commands, 500,000 runs; holdout 2,002 sampled of 90,053 reachable; seeds 5.

`all`, prefix, top1 (n = 2002, 2002, 2002, 1975)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.065 | 0.220 | 0.415 | 0.507 |
| fzf_scored (Skim) | 0.065 | 0.518 | 0.612 | 0.623 |
| fzf (history scheme) | 0.065 | 0.395 | 0.599 | 0.621 |
| heuristic before | 0.065 | 0.237 | 0.380 | 0.427 |
| heuristic after (= fzf) | 0.065 | 0.395 | 0.599 | 0.621 |
| model before | 0.154 ±0.010 | 0.510 ±0.013 | 0.575 ±0.012 | 0.587 ±0.014 |
| model after | 0.151 ±0.009 | 0.533 ±0.011 | 0.635 ±0.006 | 0.648 ±0.009 |
| stage-1 recall before | 0.883 | 0.996 | 1.000 | 1.000 |
| stage-1 recall after | 0.867 | 0.946 | 0.995 | 0.997 |

`all`, prefix, top10 (n = 2002, 2002, 2002, 1975)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.528 | 0.638 | 0.713 | 0.754 |
| fzf_scored (Skim) | 0.528 | 0.763 | 0.800 | 0.795 |
| fzf (history scheme) | 0.528 | 0.715 | 0.789 | 0.793 |
| heuristic before | 0.419 | 0.617 | 0.704 | 0.742 |
| heuristic after (= fzf) | 0.528 | 0.715 | 0.789 | 0.793 |
| model before | 0.340 ±0.038 | 0.739 ±0.004 | 0.825 ±0.006 | 0.832 ±0.006 |
| model after | 0.592 ±0.004 | 0.843 ±0.003 | 0.911 ±0.001 | 0.913 ±0.003 |

`all`, prefix, mrr (n = 2002, 2002, 2002, 1975)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.223 | 0.363 | 0.527 | 0.598 |
| fzf_scored (Skim) | 0.223 | 0.613 | 0.684 | 0.689 |
| fzf (history scheme) | 0.223 | 0.517 | 0.671 | 0.686 |
| heuristic before | 0.160 | 0.366 | 0.493 | 0.535 |
| heuristic after (= fzf) | 0.223 | 0.517 | 0.671 | 0.686 |
| model before | 0.221 ±0.016 | 0.594 ±0.011 | 0.657 ±0.009 | 0.667 ±0.010 |
| model after | 0.288 ±0.007 | 0.644 ±0.005 | 0.736 ±0.004 | 0.744 ±0.006 |

`gt10min`, prefix, top1 (n = 1367, 1367, 1367, 1346)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.008 | 0.080 | 0.236 | 0.334 |
| fzf_scored (Skim) | 0.008 | 0.339 | 0.448 | 0.463 |
| fzf (history scheme) | 0.008 | 0.224 | 0.435 | 0.458 |
| heuristic before | 0.025 | 0.096 | 0.184 | 0.225 |
| heuristic after (= fzf) | 0.008 | 0.224 | 0.435 | 0.458 |
| model before | 0.114 ±0.013 | 0.372 ±0.012 | 0.432 ±0.015 | 0.442 ±0.017 |
| model after | 0.093 ±0.004 | 0.422 ±0.013 | 0.530 ±0.007 | 0.542 ±0.012 |
| stage-1 recall before | 0.828 | 0.993 | 1.000 | 1.000 |
| stage-1 recall after | 0.805 | 0.920 | 0.993 | 0.996 |

`other_dir`, prefix, top1 (n = 670, 670, 670, 657)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.007 | 0.055 | 0.157 | 0.251 |
| fzf_scored (Skim) | 0.007 | 0.243 | 0.354 | 0.371 |
| fzf (history scheme) | 0.007 | 0.163 | 0.345 | 0.367 |
| heuristic before | 0.010 | 0.084 | 0.124 | 0.132 |
| heuristic after (= fzf) | 0.007 | 0.163 | 0.345 | 0.367 |
| model before | 0.067 ±0.012 | 0.290 ±0.022 | 0.319 ±0.023 | 0.324 ±0.023 |
| model after | 0.047 ±0.006 | 0.355 ±0.005 | 0.447 ±0.016 | 0.455 ±0.020 |
| stage-1 recall before | 0.652 | 0.987 | 1.000 | 1.000 |
| stage-1 recall after | 0.857 | 0.955 | 0.999 | 0.998 |

`all`, fragment, top1 (n = 1222, 1157, 1157, 756)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.182 | 0.251 | 0.299 | 0.361 |
| fzf_scored (Skim) | 0.048 | 0.058 | 0.241 | 0.414 |
| fzf (history scheme) | 0.261 | 0.324 | 0.329 | 0.417 |
| heuristic before | 0.158 | 0.229 | 0.264 | 0.317 |
| heuristic after (= fzf) | 0.261 | 0.324 | 0.329 | 0.417 |
| model before | 0.015 ±0.002 | 0.008 ±0.003 | 0.028 ±0.006 | 0.096 ±0.025 |
| model after | 0.096 ±0.015 | 0.265 ±0.008 | 0.275 ±0.012 | 0.381 ±0.020 |
| stage-1 recall before | 0.857 | 0.957 | 0.968 | 0.991 |
| stage-1 recall after | 0.870 | 0.915 | 0.920 | 0.955 |

`all`, fragment, top10 (n = 1222, 1157, 1157, 756)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.478 | 0.500 | 0.519 | 0.581 |
| fzf_scored (Skim) | 0.078 | 0.086 | 0.406 | 0.626 |
| fzf (history scheme) | 0.509 | 0.540 | 0.547 | 0.626 |
| heuristic before | 0.429 | 0.506 | 0.528 | 0.575 |
| heuristic after (= fzf) | 0.509 | 0.540 | 0.547 | 0.626 |
| model before | 0.032 ±0.001 | 0.038 ±0.008 | 0.123 ±0.019 | 0.288 ±0.038 |
| model after | 0.486 ±0.027 | 0.644 ±0.008 | 0.654 ±0.008 | 0.763 ±0.006 |

`all`, fragment, mrr (n = 1222, 1157, 1157, 756)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.294 | 0.346 | 0.383 | 0.439 |
| fzf_scored (Skim) | 0.059 | 0.075 | 0.307 | 0.491 |
| fzf (history scheme) | 0.358 | 0.406 | 0.411 | 0.492 |
| heuristic before | 0.252 | 0.326 | 0.358 | 0.409 |
| heuristic after (= fzf) | 0.358 | 0.406 | 0.411 | 0.492 |
| model before | 0.022 ±0.002 | 0.024 ±0.005 | 0.069 ±0.010 | 0.163 ±0.029 |
| model after | 0.224 ±0.021 | 0.396 ±0.007 | 0.405 ±0.008 | 0.515 ±0.012 |

Before: R1 +0.196 PASS (gt10min/len2) | R2 +0.080 PASS (all/len3) | R3 -0.262 FAIL (other_dir/len3/top10)  
After: R1 +0.295 PASS (gt10min/len2) | R2 +0.085 PASS (all/len0) | R3 +0.000 PASS (all/len0/top1)

#### interactive 2,000 / 20,000

2,000 commands, 20,000 runs; holdout 3,632 sampled of 3,632 reachable; seeds 5.

`all`, prefix, top1 (n = 3632, 3632, 3632, 3562)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.067 | 0.223 | 0.459 | 0.550 |
| fzf_scored (Skim) | 0.067 | 0.549 | 0.659 | 0.677 |
| fzf (history scheme) | 0.067 | 0.424 | 0.644 | 0.675 |
| heuristic before | 0.076 | 0.289 | 0.496 | 0.581 |
| heuristic after (= fzf) | 0.067 | 0.424 | 0.644 | 0.675 |
| model before | 0.155 ±0.008 | 0.617 ±0.002 | 0.740 ±0.004 | 0.755 ±0.003 |
| model after | 0.161 ±0.005 | 0.606 ±0.004 | 0.731 ±0.005 | 0.748 ±0.003 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`all`, prefix, top10 (n = 3632, 3632, 3632, 3562)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.569 | 0.707 | 0.791 | 0.833 |
| fzf_scored (Skim) | 0.569 | 0.843 | 0.895 | 0.899 |
| fzf (history scheme) | 0.569 | 0.788 | 0.879 | 0.892 |
| heuristic before | 0.526 | 0.810 | 0.898 | 0.926 |
| heuristic after (= fzf) | 0.569 | 0.788 | 0.879 | 0.892 |
| model before | 0.552 ±0.006 | 0.935 ±0.002 | 0.971 ±0.001 | 0.975 ±0.000 |
| model after | 0.631 ±0.003 | 0.921 ±0.002 | 0.969 ±0.002 | 0.975 ±0.001 |

`all`, prefix, mrr (n = 3632, 3632, 3632, 3562)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.234 | 0.386 | 0.581 | 0.655 |
| fzf_scored (Skim) | 0.234 | 0.665 | 0.748 | 0.759 |
| fzf (history scheme) | 0.234 | 0.563 | 0.733 | 0.756 |
| heuristic before | 0.202 | 0.462 | 0.643 | 0.708 |
| heuristic after (= fzf) | 0.234 | 0.563 | 0.733 | 0.756 |
| model before | 0.278 ±0.006 | 0.738 ±0.002 | 0.830 ±0.003 | 0.841 ±0.002 |
| model after | 0.306 ±0.004 | 0.723 ±0.004 | 0.823 ±0.004 | 0.838 ±0.002 |

`gt10min`, prefix, top1 (n = 2458, 2458, 2458, 2412)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.014 | 0.093 | 0.287 | 0.394 |
| fzf_scored (Skim) | 0.014 | 0.388 | 0.515 | 0.541 |
| fzf (history scheme) | 0.014 | 0.268 | 0.496 | 0.536 |
| heuristic before | 0.029 | 0.154 | 0.343 | 0.448 |
| heuristic after (= fzf) | 0.014 | 0.268 | 0.496 | 0.536 |
| model before | 0.122 ±0.007 | 0.510 ±0.006 | 0.652 ±0.006 | 0.672 ±0.005 |
| model after | 0.108 ±0.015 | 0.493 ±0.009 | 0.636 ±0.007 | 0.659 ±0.004 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`other_dir`, prefix, top1 (n = 1179, 1179, 1179, 1149)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.003 | 0.050 | 0.170 | 0.260 |
| fzf_scored (Skim) | 0.003 | 0.256 | 0.370 | 0.398 |
| fzf (history scheme) | 0.003 | 0.180 | 0.355 | 0.391 |
| heuristic before | 0.006 | 0.102 | 0.205 | 0.274 |
| heuristic after (= fzf) | 0.003 | 0.180 | 0.355 | 0.391 |
| model before | 0.075 ±0.007 | 0.394 ±0.005 | 0.521 ±0.003 | 0.537 ±0.002 |
| model after | 0.057 ±0.013 | 0.380 ±0.012 | 0.509 ±0.008 | 0.535 ±0.008 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`all`, fragment, top1 (n = 2140, 2061, 2061, 1261)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.188 | 0.268 | 0.312 | 0.392 |
| fzf_scored (Skim) | 0.058 | 0.065 | 0.265 | 0.448 |
| fzf (history scheme) | 0.268 | 0.349 | 0.353 | 0.450 |
| heuristic before | 0.199 | 0.276 | 0.320 | 0.408 |
| heuristic after (= fzf) | 0.268 | 0.349 | 0.353 | 0.450 |
| model before | 0.032 ±0.001 | 0.023 ±0.002 | 0.076 ±0.009 | 0.199 ±0.012 |
| model after | 0.168 ±0.012 | 0.348 ±0.005 | 0.366 ±0.003 | 0.472 ±0.009 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`all`, fragment, top10 (n = 2140, 2061, 2061, 1261)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.551 | 0.589 | 0.631 | 0.713 |
| fzf_scored (Skim) | 0.095 | 0.425 | 0.619 | 0.768 |
| fzf (history scheme) | 0.605 | 0.659 | 0.670 | 0.768 |
| heuristic before | 0.628 | 0.710 | 0.744 | 0.830 |
| heuristic after (= fzf) | 0.605 | 0.659 | 0.670 | 0.768 |
| model before | 0.080 ±0.003 | 0.082 ±0.036 | 0.314 ±0.059 | 0.557 ±0.041 |
| model after | 0.642 ±0.008 | 0.776 ±0.008 | 0.795 ±0.004 | 0.874 ±0.003 |

`all`, fragment, mrr (n = 2140, 2061, 2061, 1261)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.323 | 0.393 | 0.435 | 0.505 |
| fzf_scored (Skim) | 0.079 | 0.156 | 0.386 | 0.563 |
| fzf (history scheme) | 0.395 | 0.465 | 0.471 | 0.564 |
| heuristic before | 0.339 | 0.426 | 0.472 | 0.557 |
| heuristic after (= fzf) | 0.395 | 0.465 | 0.471 | 0.564 |
| model before | 0.053 ±0.002 | 0.053 ±0.012 | 0.155 ±0.023 | 0.324 ±0.018 |
| model after | 0.323 ±0.011 | 0.498 ±0.004 | 0.518 ±0.001 | 0.612 ±0.007 |

Before: R1 +0.365 PASS (gt10min/len2) | R2 +0.088 PASS (all/len0) | R3 -0.110 FAIL (all/len0/top3)  
After: R1 +0.349 PASS (gt10min/len2) | R2 +0.094 PASS (gt10min/len0) | R3 +0.000 PASS (all/len0/top1)

#### repetitive 50,000 / 500,000

50,000 commands, 500,000 runs; holdout 2,001 sampled of 90,009 reachable; seeds 5.

`all`, prefix, top1 (n = 2001, 2001, 2001, 2001)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.512 | 0.546 | 0.572 | 0.595 |
| fzf_scored (Skim) | 0.512 | 0.589 | 0.609 | 0.595 |
| fzf (history scheme) | 0.512 | 0.573 | 0.607 | 0.611 |
| heuristic before | 0.282 | 0.504 | 0.548 | 0.570 |
| heuristic after (= fzf) | 0.512 | 0.573 | 0.607 | 0.611 |
| model before | 0.482 ±0.004 | 0.591 ±0.004 | 0.628 ±0.003 | 0.631 ±0.004 |
| model after | 0.509 ±0.003 | 0.608 ±0.013 | 0.654 ±0.010 | 0.664 ±0.012 |
| stage-1 recall before | 0.766 | 0.957 | 0.996 | 0.999 |
| stage-1 recall after | 0.767 | 0.901 | 0.989 | 0.999 |

`all`, prefix, top10 (n = 2001, 2001, 2001, 2001)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.594 | 0.605 | 0.629 | 0.658 |
| fzf_scored (Skim) | 0.594 | 0.651 | 0.681 | 0.667 |
| fzf (history scheme) | 0.594 | 0.637 | 0.680 | 0.685 |
| heuristic before | 0.591 | 0.625 | 0.663 | 0.682 |
| heuristic after (= fzf) | 0.594 | 0.637 | 0.680 | 0.685 |
| model before | 0.543 ±0.004 | 0.724 ±0.003 | 0.761 ±0.005 | 0.767 ±0.005 |
| model after | 0.647 ±0.002 | 0.755 ±0.004 | 0.800 ±0.002 | 0.806 ±0.003 |

`all`, prefix, mrr (n = 2001, 2001, 2001, 2001)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.550 | 0.572 | 0.596 | 0.617 |
| fzf_scored (Skim) | 0.550 | 0.613 | 0.633 | 0.620 |
| fzf (history scheme) | 0.550 | 0.598 | 0.631 | 0.636 |
| heuristic before | 0.397 | 0.556 | 0.590 | 0.607 |
| heuristic after (= fzf) | 0.550 | 0.598 | 0.631 | 0.636 |
| model before | 0.506 ±0.002 | 0.637 ±0.004 | 0.674 ±0.002 | 0.678 ±0.003 |
| model after | 0.561 ±0.002 | 0.662 ±0.010 | 0.708 ±0.006 | 0.716 ±0.007 |

`gt10min`, prefix, top1 (n = 1007, 1007, 1007, 1007)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.133 | 0.154 | 0.173 | 0.206 |
| fzf_scored (Skim) | 0.133 | 0.203 | 0.227 | 0.226 |
| fzf (history scheme) | 0.133 | 0.185 | 0.224 | 0.232 |
| heuristic before | 0.068 | 0.145 | 0.168 | 0.182 |
| heuristic after (= fzf) | 0.133 | 0.185 | 0.224 | 0.232 |
| model before | 0.107 ±0.004 | 0.230 ±0.007 | 0.288 ±0.008 | 0.293 ±0.009 |
| model after | 0.132 ±0.000 | 0.272 ±0.009 | 0.342 ±0.011 | 0.351 ±0.016 |
| stage-1 recall before | 0.534 | 0.915 | 0.992 | 0.997 |
| stage-1 recall after | 0.537 | 0.802 | 0.977 | 0.997 |

`other_dir`, prefix, top1 (n = 761, 761, 761, 761)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.122 | 0.141 | 0.158 | 0.189 |
| fzf_scored (Skim) | 0.122 | 0.183 | 0.209 | 0.214 |
| fzf (history scheme) | 0.122 | 0.167 | 0.209 | 0.216 |
| heuristic before | 0.083 | 0.150 | 0.156 | 0.160 |
| heuristic after (= fzf) | 0.122 | 0.167 | 0.209 | 0.216 |
| model before | 0.091 ±0.004 | 0.181 ±0.007 | 0.223 ±0.005 | 0.229 ±0.004 |
| model after | 0.115 ±0.008 | 0.231 ±0.033 | 0.293 ±0.028 | 0.310 ±0.029 |
| stage-1 recall before | 0.414 | 0.887 | 0.989 | 0.996 |
| stage-1 recall after | 0.545 | 0.795 | 0.972 | 0.996 |

`all`, fragment, top1 (n = 2001, 2001, 2001, 1486)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.526 | 0.539 | 0.551 | 0.559 |
| fzf_scored (Skim) | 0.063 | 0.074 | 0.435 | 0.570 |
| fzf (history scheme) | 0.541 | 0.554 | 0.555 | 0.570 |
| heuristic before | 0.344 | 0.426 | 0.443 | 0.510 |
| heuristic after (= fzf) | 0.541 | 0.554 | 0.555 | 0.570 |
| model before | 0.042 ±0.001 | 0.035 ±0.034 | 0.215 ±0.056 | 0.327 ±0.022 |
| model after | 0.523 ±0.023 | 0.573 ±0.006 | 0.575 ±0.006 | 0.606 ±0.001 |
| stage-1 recall before | 0.786 | 0.877 | 0.890 | 0.960 |
| stage-1 recall after | 0.831 | 0.893 | 0.899 | 0.962 |

`all`, fragment, top10 (n = 2001, 2001, 2001, 1486)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.596 | 0.601 | 0.605 | 0.624 |
| fzf_scored (Skim) | 0.066 | 0.076 | 0.473 | 0.656 |
| fzf (history scheme) | 0.601 | 0.615 | 0.615 | 0.656 |
| heuristic before | 0.593 | 0.610 | 0.619 | 0.631 |
| heuristic after (= fzf) | 0.601 | 0.615 | 0.615 | 0.656 |
| model before | 0.054 ±0.001 | 0.163 ±0.093 | 0.455 ±0.039 | 0.509 ±0.047 |
| model after | 0.657 ±0.010 | 0.709 ±0.002 | 0.713 ±0.002 | 0.757 ±0.002 |

`all`, fragment, mrr (n = 2001, 2001, 2001, 1486)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.559 | 0.567 | 0.576 | 0.584 |
| fzf_scored (Skim) | 0.065 | 0.080 | 0.457 | 0.602 |
| fzf (history scheme) | 0.569 | 0.580 | 0.582 | 0.602 |
| heuristic before | 0.445 | 0.502 | 0.516 | 0.558 |
| heuristic after (= fzf) | 0.569 | 0.580 | 0.582 | 0.602 |
| model before | 0.047 ±0.001 | 0.079 ±0.048 | 0.298 ±0.054 | 0.384 ±0.030 |
| model after | 0.571 ±0.017 | 0.622 ±0.004 | 0.625 ±0.004 | 0.660 ±0.002 |

Before: R1 +0.116 PASS (gt10min/len2) | R2 -0.030 FAIL (all/len0) | R3 -0.230 FAIL (all/len0/top1)  
After: R1 +0.169 PASS (gt10min/len2) | R2 -0.003 PASS (all/len0) | R3 +0.000 PASS (all/len0/top1)

#### repetitive 2,000 / 20,000

2,000 commands, 20,000 runs; holdout 3,634 sampled of 3,634 reachable; seeds 5.

`all`, prefix, top1 (n = 3634, 3634, 3634, 3634)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.517 | 0.556 | 0.594 | 0.624 |
| fzf_scored (Skim) | 0.517 | 0.622 | 0.651 | 0.647 |
| fzf (history scheme) | 0.517 | 0.600 | 0.646 | 0.655 |
| heuristic before | 0.299 | 0.527 | 0.586 | 0.630 |
| heuristic after (= fzf) | 0.517 | 0.600 | 0.646 | 0.655 |
| model before | 0.494 ±0.005 | 0.688 ±0.004 | 0.739 ±0.004 | 0.748 ±0.002 |
| model after | 0.513 ±0.003 | 0.641 ±0.028 | 0.718 ±0.021 | 0.746 ±0.010 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`all`, prefix, top10 (n = 3634, 3634, 3634, 3634)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.614 | 0.641 | 0.694 | 0.751 |
| fzf_scored (Skim) | 0.614 | 0.744 | 0.799 | 0.804 |
| fzf (history scheme) | 0.614 | 0.709 | 0.789 | 0.804 |
| heuristic before | 0.625 | 0.739 | 0.803 | 0.840 |
| heuristic after (= fzf) | 0.614 | 0.709 | 0.789 | 0.804 |
| model before | 0.670 ±0.008 | 0.849 ±0.002 | 0.895 ±0.002 | 0.899 ±0.002 |
| model after | 0.713 ±0.002 | 0.833 ±0.013 | 0.887 ±0.008 | 0.896 ±0.002 |

`all`, prefix, mrr (n = 3634, 3634, 3634, 3634)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.561 | 0.591 | 0.631 | 0.667 |
| fzf_scored (Skim) | 0.561 | 0.664 | 0.700 | 0.699 |
| fzf (history scheme) | 0.561 | 0.638 | 0.694 | 0.704 |
| heuristic before | 0.423 | 0.604 | 0.662 | 0.703 |
| heuristic after (= fzf) | 0.561 | 0.638 | 0.694 | 0.704 |
| model before | 0.552 ±0.005 | 0.747 ±0.002 | 0.794 ±0.002 | 0.801 ±0.001 |
| model after | 0.581 ±0.002 | 0.712 ±0.021 | 0.779 ±0.016 | 0.798 ±0.008 |

`gt10min`, prefix, top1 (n = 1752, 1752, 1752, 1752)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.122 | 0.149 | 0.186 | 0.229 |
| fzf_scored (Skim) | 0.122 | 0.225 | 0.279 | 0.284 |
| fzf (history scheme) | 0.122 | 0.197 | 0.272 | 0.287 |
| heuristic before | 0.076 | 0.170 | 0.233 | 0.292 |
| heuristic after (= fzf) | 0.122 | 0.197 | 0.272 | 0.287 |
| model before | 0.105 ±0.004 | 0.378 ±0.009 | 0.469 ±0.009 | 0.486 ±0.004 |
| model after | 0.121 ±0.001 | 0.304 ±0.044 | 0.430 ±0.037 | 0.481 ±0.018 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`other_dir`, prefix, top1 (n = 1336, 1336, 1336, 1336)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.095 | 0.115 | 0.154 | 0.195 |
| fzf_scored (Skim) | 0.095 | 0.189 | 0.248 | 0.256 |
| fzf (history scheme) | 0.095 | 0.165 | 0.244 | 0.257 |
| heuristic before | 0.068 | 0.140 | 0.179 | 0.219 |
| heuristic after (= fzf) | 0.095 | 0.165 | 0.244 | 0.257 |
| model before | 0.074 ±0.004 | 0.300 ±0.007 | 0.376 ±0.005 | 0.394 ±0.003 |
| model after | 0.089 ±0.004 | 0.256 ±0.041 | 0.373 ±0.030 | 0.420 ±0.013 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`all`, fragment, top1 (n = 3634, 3634, 3634, 2785)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.533 | 0.545 | 0.560 | 0.605 |
| fzf_scored (Skim) | 0.054 | 0.051 | 0.431 | 0.631 |
| fzf (history scheme) | 0.553 | 0.567 | 0.567 | 0.631 |
| heuristic before | 0.362 | 0.432 | 0.460 | 0.578 |
| heuristic after (= fzf) | 0.553 | 0.567 | 0.567 | 0.631 |
| model before | 0.046 ±0.000 | 0.185 ±0.093 | 0.412 ±0.050 | 0.523 ±0.031 |
| model after | 0.542 ±0.004 | 0.575 ±0.010 | 0.591 ±0.006 | 0.683 ±0.006 |
| stage-1 recall before | 1.000 | 1.000 | 1.000 | 1.000 |
| stage-1 recall after | 1.000 | 1.000 | 1.000 | 1.000 |

`all`, fragment, top10 (n = 3634, 3634, 3634, 2785)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.620 | 0.633 | 0.649 | 0.728 |
| fzf_scored (Skim) | 0.060 | 0.550 | 0.641 | 0.787 |
| fzf (history scheme) | 0.631 | 0.657 | 0.662 | 0.787 |
| heuristic before | 0.646 | 0.687 | 0.703 | 0.799 |
| heuristic after (= fzf) | 0.631 | 0.657 | 0.662 | 0.787 |
| model before | 0.059 ±0.001 | 0.464 ±0.058 | 0.640 ±0.027 | 0.734 ±0.020 |
| model after | 0.727 ±0.005 | 0.770 ±0.009 | 0.778 ±0.003 | 0.862 ±0.002 |

`all`, fragment, mrr (n = 3634, 3634, 3634, 2785)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.572 | 0.583 | 0.597 | 0.648 |
| fzf_scored (Skim) | 0.062 | 0.140 | 0.491 | 0.682 |
| fzf (history scheme) | 0.587 | 0.603 | 0.605 | 0.682 |
| heuristic before | 0.475 | 0.532 | 0.554 | 0.658 |
| heuristic after (= fzf) | 0.587 | 0.603 | 0.605 | 0.682 |
| model before | 0.055 ±0.001 | 0.280 ±0.084 | 0.491 ±0.044 | 0.598 ±0.027 |
| model after | 0.605 ±0.005 | 0.643 ±0.009 | 0.657 ±0.005 | 0.742 ±0.005 |

Before: R1 +0.283 PASS (gt10min/len2) | R2 -0.023 FAIL (all/len0) | R3 -0.218 FAIL (all/len0/top1)  
After: R1 +0.244 PASS (gt10min/len2) | R2 -0.004 PASS (all/len0) | R3 +0.000 PASS (all/len0/top1)

#### agent 5,000 / 50,000

5,000 commands, 50,000 runs; holdout 2,256 sampled of 9,023 reachable; seeds 5.

`all`, prefix, top1 (n = 2256, 2256, 2256, 2256)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf_scored (Skim) | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf (history scheme) | 0.000 | 0.000 | 0.000 | 0.000 |
| heuristic before | 0.043 | 0.043 | 0.043 | 0.043 |
| heuristic after (= fzf) | 0.000 | 0.000 | 0.000 | 0.000 |
| model before | 0.045 ±0.014 | 0.047 ±0.016 | 0.047 ±0.016 | 0.046 ±0.015 |
| model after | 0.080 ±0.002 | 0.083 ±0.003 | 0.083 ±0.003 | 0.082 ±0.002 |
| stage-1 recall before | 0.891 | 0.891 | 0.891 | 0.891 |
| stage-1 recall after | 0.766 | 0.766 | 0.766 | 0.766 |

`all`, prefix, top10 (n = 2256, 2256, 2256, 2256)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.219 | 0.219 | 0.219 | 0.219 |
| fzf_scored (Skim) | 0.219 | 0.219 | 0.219 | 0.219 |
| fzf (history scheme) | 0.219 | 0.219 | 0.219 | 0.219 |
| heuristic before | 0.270 | 0.270 | 0.270 | 0.270 |
| heuristic after (= fzf) | 0.219 | 0.219 | 0.219 | 0.219 |
| model before | 0.271 ±0.011 | 0.271 ±0.011 | 0.270 ±0.008 | 0.270 ±0.009 |
| model after | 0.325 ±0.003 | 0.324 ±0.002 | 0.323 ±0.003 | 0.324 ±0.004 |

`all`, prefix, mrr (n = 2256, 2256, 2256, 2256)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.069 | 0.069 | 0.069 | 0.069 |
| fzf_scored (Skim) | 0.069 | 0.069 | 0.069 | 0.069 |
| fzf (history scheme) | 0.069 | 0.069 | 0.069 | 0.069 |
| heuristic before | 0.120 | 0.120 | 0.120 | 0.120 |
| heuristic after (= fzf) | 0.069 | 0.069 | 0.069 | 0.069 |
| model before | 0.119 ±0.016 | 0.120 ±0.017 | 0.119 ±0.016 | 0.119 ±0.016 |
| model after | 0.162 ±0.002 | 0.164 ±0.002 | 0.163 ±0.002 | 0.163 ±0.001 |

`gt10min`, prefix, top1 (n = 1999, 1999, 1999, 1999)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf_scored (Skim) | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf (history scheme) | 0.000 | 0.000 | 0.000 | 0.000 |
| heuristic before | 0.017 | 0.017 | 0.017 | 0.017 |
| heuristic after (= fzf) | 0.000 | 0.000 | 0.000 | 0.000 |
| model before | 0.029 ±0.019 | 0.030 ±0.020 | 0.030 ±0.020 | 0.029 ±0.019 |
| model after | 0.053 ±0.003 | 0.059 ±0.003 | 0.058 ±0.003 | 0.055 ±0.004 |
| stage-1 recall before | 0.877 | 0.877 | 0.877 | 0.877 |
| stage-1 recall after | 0.736 | 0.736 | 0.736 | 0.736 |

`other_dir`, prefix, top1 (n = 878, 878, 878, 878)

| ranker | len 0 | len 1 | len 2 | len 3 |
|---|---|---|---|---|
| recency | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf_scored (Skim) | 0.000 | 0.000 | 0.000 | 0.000 |
| fzf (history scheme) | 0.000 | 0.000 | 0.000 | 0.000 |
| heuristic before | 0.001 | 0.001 | 0.001 | 0.001 |
| heuristic after (= fzf) | 0.000 | 0.000 | 0.000 | 0.000 |
| model before | 0.006 ±0.004 | 0.005 ±0.004 | 0.005 ±0.004 | 0.006 ±0.004 |
| model after | 0.007 ±0.001 | 0.007 ±0.001 | 0.006 ±0.001 | 0.006 ±0.001 |
| stage-1 recall before | 0.720 | 0.720 | 0.720 | 0.720 |
| stage-1 recall after | 0.583 | 0.583 | 0.583 | 0.583 |

`all`, fragment, top1 (n = 2256, 2197, 2119, 1120)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.002 | 0.004 | 0.003 | 0.015 |
| fzf_scored (Skim) | 0.008 | 0.030 | 0.020 | 0.048 |
| fzf (history scheme) | 0.008 | 0.029 | 0.020 | 0.046 |
| heuristic before | 0.050 | 0.065 | 0.059 | 0.081 |
| heuristic after (= fzf) | 0.008 | 0.029 | 0.020 | 0.046 |
| model before | 0.049 ±0.015 | 0.053 ±0.015 | 0.050 ±0.014 | 0.062 ±0.016 |
| model after | 0.089 ±0.006 | 0.106 ±0.007 | 0.104 ±0.004 | 0.114 ±0.009 |
| stage-1 recall before | 0.902 | 0.921 | 0.921 | 0.929 |
| stage-1 recall after | 0.797 | 0.847 | 0.844 | 0.861 |

`all`, fragment, top10 (n = 2256, 2197, 2119, 1120)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.224 | 0.230 | 0.231 | 0.266 |
| fzf_scored (Skim) | 0.224 | 0.269 | 0.263 | 0.310 |
| fzf (history scheme) | 0.236 | 0.269 | 0.265 | 0.311 |
| heuristic before | 0.283 | 0.302 | 0.300 | 0.352 |
| heuristic after (= fzf) | 0.236 | 0.269 | 0.265 | 0.311 |
| model before | 0.277 ±0.009 | 0.285 ±0.008 | 0.286 ±0.008 | 0.319 ±0.009 |
| model after | 0.341 ±0.004 | 0.379 ±0.006 | 0.372 ±0.005 | 0.407 ±0.008 |

`all`, fragment, mrr (n = 2256, 2197, 2119, 1120)

| ranker | len 1 | len 2 | len 3 | len 5 |
|---|---|---|---|---|
| recency | 0.072 | 0.076 | 0.075 | 0.093 |
| fzf_scored (Skim) | 0.077 | 0.106 | 0.097 | 0.131 |
| fzf (history scheme) | 0.081 | 0.107 | 0.098 | 0.132 |
| heuristic before | 0.129 | 0.147 | 0.142 | 0.169 |
| heuristic after (= fzf) | 0.081 | 0.107 | 0.098 | 0.132 |
| model before | 0.123 ±0.016 | 0.129 ±0.016 | 0.126 ±0.015 | 0.146 ±0.018 |
| model after | 0.173 ±0.003 | 0.196 ±0.006 | 0.191 ±0.004 | 0.210 ±0.008 |

Before: R1 +0.030 FAIL (gt10min/len2) | R2 +0.029 PASS (gt10min/len0) | R3 +0.001 PASS (other_dir/len0/top1)  
After: R1 +0.058 FAIL (gt10min/len2) | R2 +0.053 PASS (gt10min/len0) | R3 +0.000 PASS (all/len0/top1)

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
