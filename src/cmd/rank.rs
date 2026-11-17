//! Ranking the commands that matched: the heuristic
//! (`docs/ctrl-r/02-browse.md` section 7) and, when a model is loaded, the
//! model stage on top of it (`03-learn.md` section 7).
//!
//! Two stages: the heuristic orders every candidate; the model re-scores
//! only the first `CMD_STAGE2_K` of that order, blends its score with the
//! heuristic by the file-mode tanh weight, and re-sorts them. Rows past the
//! cut keep their heuristic order below the model-scored ones.

use super::features::{COMMAND_FEATURE_REGISTRY, CommandFeatureInputs, compute_all};
use super::index::{CommandIndex, RankContext};
use super::model::CmdModel;
use super::tuning::{
    CMD_H_DIR_SATURATION, CMD_H_RECENCY_SCALE_SECS, CMD_H_W_DIR, CMD_H_W_FUZZY, CMD_H_W_RECENCY,
    CMD_H_W_REPO, CMD_PAR_FILTER_MIN, CMD_STAGE2_K,
};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::time::Instant;

/// From this many stage-2 rows the features are computed on the rayon pool.
const PAR_FEATURES_MIN: usize = 256;

/// A command that matched the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandCandidate {
    pub idx: u32,
    pub fuzzy_score: i64,
}

/// The four terms of the heuristic, each in [0, 1].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct HeuristicParts {
    pub recency: f64,
    pub dir: f64,
    pub repo: f64,
    pub fuzzy: f64,
}

/// One ranked command.
#[derive(Clone)]
pub(super) struct CommandScore {
    pub idx: u32,
    /// What the list is sorted by: the heuristic until a model blends in.
    pub score: f64,
    pub heuristic: f64,
    pub parts: HeuristicParts,
    pub ml_score: Option<f64>,
    pub ml_weight: f64,
    pub fuzzy_score: i64,
}

/// Time spent in each stage. The model stages are `None` (and `stage2` 0)
/// when no model scored anything.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct CmdRankTimings {
    pub heuristic_ms: f64,
    pub features_ms: Option<f64>,
    pub predict_ms: Option<f64>,
    pub blend_ms: Option<f64>,
    pub stage2: usize,
}

/// The ranked scores and what they cost.
#[derive(Clone)]
pub(super) struct CommandRanking {
    /// `scores[..sorted]` in rank order; the rest come after them in no
    /// particular order until [`CommandRanking::sort_rest`].
    pub scores: Vec<CommandScore>,
    pub sorted: usize,
    pub timings: CmdRankTimings,
    /// The model's prediction failed and this is the heuristic order (E-L-07).
    pub model_failed: bool,
    /// The features of the model-scored rows, row-major in the order they
    /// were computed (the heuristic's), empty when no model scored anything;
    /// `feature_rows[rank]` is the row of `scores[rank]`. One buffer, not a
    /// vector per row: thousands of rows are scored per keystroke and only
    /// the few sent to the UI are ever read.
    pub features: Vec<f64>,
    pub feature_rows: Vec<u32>,
}

impl CommandRanking {
    /// The features of the row at `rank`, in registry order; empty when the
    /// model did not score it.
    pub(super) fn features_of(&self, rank: usize) -> &[f64] {
        let width = COMMAND_FEATURE_REGISTRY.len();
        if rank < self.timings.stage2 {
            let row = self.feature_rows[rank] as usize;
            &self.features[row * width..(row + 1) * width]
        } else {
            &[]
        }
    }

    /// Put the scores past `sorted` in rank order too.
    pub(super) fn sort_rest(&mut self, index: &CommandIndex) {
        let rest = &mut self.scores[self.sorted..];
        if rest.len() >= CMD_PAR_FILTER_MIN {
            rest.par_sort_unstable_by(|a, b| rank_order(index, a, b));
        } else {
            rest.sort_unstable_by(|a, b| rank_order(index, a, b));
        }
        self.sorted = self.scores.len();
    }
}

/// `(runs here / runs) * min(1, ln(1 + runs here) / ln(1 + saturation))`:
/// the share of the command's runs made here, damped while there are few.
fn place_term(runs_here: u32, run_count: u32) -> f64 {
    if run_count == 0 || runs_here == 0 {
        return 0.0;
    }
    let share = f64::from(runs_here) / f64::from(run_count);
    let confidence = (f64::from(runs_here).ln_1p() / CMD_H_DIR_SATURATION.ln_1p()).min(1.0);
    share.min(1.0) * confidence
}

/// The heuristic terms of one candidate. `max_fuzzy` is the largest
/// `fuzzy_score` among the candidates of this ranking; 0 means "no query".
pub(super) fn heuristic(
    index: &CommandIndex,
    ctx: &RankContext,
    c: &CommandCandidate,
    max_fuzzy: i64,
) -> HeuristicParts {
    let entry = &index.entries[c.idx as usize];
    let age = (ctx.now - entry.last_run_at).max(0) as f64;
    let runs_here = index.dir_stat(c.idx, ctx.dir).map_or(0, |d| d.runs);
    HeuristicParts {
        recency: 1.0 / (1.0 + age / CMD_H_RECENCY_SCALE_SECS),
        dir: place_term(runs_here, entry.run_count),
        repo: place_term(index.runs_in_repo(c.idx, ctx.repo), entry.run_count),
        fuzzy: if max_fuzzy > 0 {
            c.fuzzy_score as f64 / max_fuzzy as f64
        } else {
            0.0
        },
    }
}

/// The weighted mean of the four terms.
pub(super) fn combine(parts: &HeuristicParts) -> f64 {
    (CMD_H_W_RECENCY * parts.recency
        + CMD_H_W_DIR * parts.dir
        + CMD_H_W_REPO * parts.repo
        + CMD_H_W_FUZZY * parts.fuzzy)
        / (CMD_H_W_RECENCY + CMD_H_W_DIR + CMD_H_W_REPO + CMD_H_W_FUZZY)
}

/// What the one total order of every ranking compares: `score`
/// descending, then the newer last run, then the larger database id.
#[derive(Clone, Copy)]
struct SortKey {
    score: f64,
    last_run_at: i64,
    db_id: i64,
    /// Position in the candidate list.
    at: u32,
}

fn compare(a: &SortKey, b: &SortKey) -> Ordering {
    b.score
        .total_cmp(&a.score)
        .then(b.last_run_at.cmp(&a.last_run_at))
        .then(b.db_id.cmp(&a.db_id))
}

fn sort_key(index: &CommandIndex, score: f64, idx: u32, at: u32) -> SortKey {
    let entry = &index.entries[idx as usize];
    SortKey {
        score,
        last_run_at: entry.last_run_at,
        db_id: entry.db_id,
        at,
    }
}

/// The one total order of every ranking: `score` descending, then the newer
/// last run, then the larger database id.
pub(super) fn rank_order(index: &CommandIndex, a: &CommandScore, b: &CommandScore) -> Ordering {
    compare(
        &sort_key(index, a.score, a.idx, 0),
        &sort_key(index, b.score, b.idx, 0),
    )
}

// ---------------------------------------------------------------------------
// Ranking
// ---------------------------------------------------------------------------

/// `(flat row-major features, feature count)` -> one raw score per row.
pub(super) type Predict<'a> = &'a dyn Fn(&[f64], usize) -> anyhow::Result<Vec<f64>>;

/// The model stage of [`rank_with`].
pub(super) struct Stage2<'a> {
    /// Rows, from the top of the heuristic order, that the model scores.
    pub k: usize,
    pub ml_weight: f64,
    pub predict: Predict<'a>,
}

/// Rank `candidates` for `query` (section 7.2 of `03-learn.md`): the
/// heuristic, then, with a `model`, its stage on the first `CMD_STAGE2_K`
/// rows. Only `scores[..sorted]` are in rank order, and `sorted` is at least
/// `head` (or every row, if fewer).
pub(super) fn rank_commands(
    query: &str,
    candidates: &[CommandCandidate],
    index: &CommandIndex,
    ctx: &RankContext,
    head: usize,
    model: Option<&CmdModel>,
) -> CommandRanking {
    match model {
        None => rank_with(query, candidates, index, ctx, head, None),
        Some(model) => {
            let predict = |flat: &[f64], num_features: usize| model.predict(flat, num_features);
            let stage2 = Stage2 {
                k: CMD_STAGE2_K,
                ml_weight: model.ml_weight,
                predict: &predict,
            };
            rank_with(query, candidates, index, ctx, head, Some(stage2))
        }
    }
}

/// [`rank_commands`] with the model stage passed in, so tests can stub the
/// prediction and the two-stage check can set `k`.
pub(super) fn rank_with(
    query: &str,
    candidates: &[CommandCandidate],
    index: &CommandIndex,
    ctx: &RankContext,
    head: usize,
    stage2: Option<Stage2<'_>>,
) -> CommandRanking {
    // Stage 2 takes the heuristic's top k as a set: it sorts them itself.
    let select = stage2.as_ref().map_or(head, |s| head.max(s.k));
    let mut ranking = rank_heuristic(query, candidates, index, ctx, select, head);
    if let Some(stage2) = stage2 {
        apply_model(query, index, ctx, &stage2, &mut ranking);
    }
    ranking
}

/// Steps 3-8 of section 7.2: score the first `k` rows with the model, blend,
/// and re-sort them. On a failed prediction the ranking is left as the
/// heuristic made it, with `model_failed` set.
fn apply_model(
    query: &str,
    index: &CommandIndex,
    ctx: &RankContext,
    stage2: &Stage2<'_>,
    ranking: &mut CommandRanking,
) {
    // `rank_heuristic` selected at least this many from the top.
    let k = stage2.k.min(ranking.scores.len());
    if k == 0 {
        return;
    }
    let num_features = COMMAND_FEATURE_REGISTRY.len();

    let started = Instant::now();
    let query_lower = query.to_lowercase();
    let mut flat = vec![0.0; k * num_features];
    let fill = |(row, score): (&mut [f64], &CommandScore)| {
        let inputs = CommandFeatureInputs {
            query,
            query_lower: &query_lower,
            idx: score.idx,
            entry: &index.entries[score.idx as usize],
            fuzzy_score: score.fuzzy_score,
            index,
            ctx,
        };
        compute_all(&inputs, row);
    };
    let head = &ranking.scores[..k];
    if k >= PAR_FEATURES_MIN {
        flat.par_chunks_mut(num_features)
            .zip(head.par_iter())
            .for_each(fill);
    } else {
        flat.chunks_mut(num_features).zip(head).for_each(fill);
    }
    let features_ms = ms_since(started);

    let started = Instant::now();
    let raw = match (stage2.predict)(&flat, num_features) {
        Ok(raw) if raw.len() == k => raw,
        _ => {
            ranking.model_failed = true;
            return;
        }
    };
    let predict_ms = ms_since(started);

    let started = Instant::now();
    let w = stage2.ml_weight;
    let head = &mut ranking.scores[..k];
    let mut keys: Vec<SortKey> = Vec::with_capacity(k);
    for (at, (score, raw)) in head.iter_mut().zip(raw).enumerate() {
        let ml = crate::ranker::logistic(raw);
        score.score = (1.0 - w) * score.heuristic + w * ml;
        score.ml_score = Some(ml);
        score.ml_weight = w;
        keys.push(sort_key(index, score.score, score.idx, at as u32));
    }
    keys.sort_unstable_by(compare);
    let order: Vec<usize> = keys.iter().map(|key| key.at as usize).collect();
    permute(head, &order);
    ranking.feature_rows = order.iter().map(|&at| at as u32).collect();
    ranking.features = flat;
    ranking.sorted = ranking.sorted.max(k);

    ranking.timings.features_ms = Some(features_ms);
    ranking.timings.predict_ms = Some(predict_ms);
    ranking.timings.blend_ms = Some(ms_since(started));
    ranking.timings.stage2 = k;
}

/// Reorder `items` in place so that `items[i]` becomes the old
/// `items[order[i]]`; `order` is a permutation of `0..items.len()`.
fn permute<T>(items: &mut [T], order: &[usize]) {
    assert_eq!(items.len(), order.len(), "one position per item");
    let mut placed = vec![false; items.len()];
    for start in 0..items.len() {
        let mut current = start;
        while !placed[current] {
            placed[current] = true;
            let next = order[current];
            if next == start {
                break;
            }
            items.swap(current, next);
            current = next;
        }
    }
}

fn ms_since(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Rank `candidates` for `query` by the heuristic alone: ordered by
/// [`rank_order`], the first `head` (or all, if fewer) exactly and the rest
/// after them unsorted ([`CommandRanking::sort_rest`] sorts them). The query
/// only switches the fuzzy term on; matching happened in the filter.
///
/// A screen shows a few dozen rows, so sorting 50,000 to show them is most
/// of the cost of an empty query: the head is selected in linear time and
/// only it is sorted. What is selected and sorted is a 32-byte key per
/// candidate; the scores are built once, in that order, afterwards.
///
/// `select` (at least `head`) is how many rows from the top are guaranteed
/// to be `scores[..select]` as a set: the model stage takes them from there.
fn rank_heuristic(
    query: &str,
    candidates: &[CommandCandidate],
    index: &CommandIndex,
    ctx: &RankContext,
    select: usize,
    head: usize,
) -> CommandRanking {
    assert!(select >= head, "the sorted head lies inside the selection");
    assert!(
        u32::try_from(candidates.len()).is_ok(),
        "fewer than 2^32 candidates"
    );
    let started = Instant::now();
    let max_fuzzy = if query.is_empty() {
        0
    } else {
        candidates.iter().map(|c| c.fuzzy_score).max().unwrap_or(0)
    };
    let parallel = candidates.len() >= CMD_PAR_FILTER_MIN;
    let score = |(at, c): (usize, &CommandCandidate)| {
        let parts = heuristic(index, ctx, c, max_fuzzy);
        (sort_key(index, combine(&parts), c.idx, at as u32), parts)
    };
    let (mut keys, parts): (Vec<SortKey>, Vec<HeuristicParts>) = if parallel {
        candidates.par_iter().enumerate().map(score).unzip()
    } else {
        candidates.iter().enumerate().map(score).unzip()
    };
    let selected = select.min(keys.len());
    let sorted = head.min(keys.len());
    if sorted < keys.len() {
        if selected < keys.len() && selected > 0 {
            keys.select_nth_unstable_by(selected - 1, compare);
        }
        if sorted > 0 && sorted < selected {
            keys[..selected].select_nth_unstable_by(sorted - 1, compare);
        }
        keys[..sorted].sort_unstable_by(compare);
    } else if parallel {
        keys.par_sort_unstable_by(compare);
    } else {
        keys.sort_unstable_by(compare);
    }
    let build = |key: &SortKey| {
        let at = key.at as usize;
        CommandScore {
            idx: candidates[at].idx,
            score: key.score,
            heuristic: key.score,
            parts: parts[at],
            ml_score: None,
            ml_weight: 0.0,
            fuzzy_score: candidates[at].fuzzy_score,
        }
    };
    let scores: Vec<CommandScore> = if parallel {
        keys.par_iter().map(build).collect()
    } else {
        keys.iter().map(build).collect()
    };
    CommandRanking {
        scores,
        sorted,
        timings: CmdRankTimings {
            heuristic_ms: ms_since(started),
            ..CmdRankTimings::default()
        },
        model_failed: false,
        features: Vec::new(),
        feature_rows: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::index::test_support::{context, index_of};

    const NOW: i64 = 1_000_000;

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() <= 1e-5
    }

    fn candidate(idx: u32) -> CommandCandidate {
        CommandCandidate {
            idx,
            fuzzy_score: 0,
        }
    }

    fn order(
        index: &CommandIndex,
        ctx: &RankContext,
        query: &str,
        c: &[CommandCandidate],
    ) -> Vec<i64> {
        rank_commands(query, c, index, ctx, usize::MAX, None)
            .scores
            .iter()
            .map(|s| index.entries[s.idx as usize].db_id)
            .collect()
    }

    #[test]
    fn tc_b_01_recency() {
        let mut index = index_of(&["a", "b"]);
        index.record_run(0, NOW - 10, None, None, Some(0), None);
        index.record_run(1, NOW + 50, None, None, Some(0), None);
        let ctx = context(NOW);
        let parts = heuristic(&index, &ctx, &candidate(0), 0);
        assert!(close(parts.recency, 0.99723), "{parts:?}");
        assert!(close(combine(&parts), 0.43358), "{}", combine(&parts));
        // A run in the future (clock skew) counts as now.
        assert_eq!(heuristic(&index, &ctx, &candidate(1), 0).recency, 1.0);
    }

    /// B of TC-B-02: 10 runs, all in `dir`, the newest 172,800 s ago.
    fn directory_regular(index: &mut CommandIndex, idx: u32, dir: u32) {
        for i in 0..10 {
            index.record_run(idx, NOW - 172_800 - i * 100, Some(dir), None, Some(0), None);
        }
    }

    #[test]
    fn tc_b_02_directory() {
        let mut index = index_of(&["b", "c"]);
        let dir = index.dirs.intern("/w");
        directory_regular(&mut index, 0, dir);
        let mut ctx = context(NOW);
        ctx.dir = Some(dir);
        let parts = heuristic(&index, &ctx, &candidate(0), 0);
        assert!(close(parts.recency, 0.02041), "{parts:?}");
        assert_eq!(parts.dir, 1.0);
        assert!(close(combine(&parts), 0.22626), "{}", combine(&parts));

        // 1 of 10 runs here.
        index.record_run(1, NOW - 50, Some(dir), None, Some(0), None);
        for _ in 0..9 {
            index.record_run(1, NOW - 60, None, None, Some(0), None);
        }
        assert!(close(
            heuristic(&index, &ctx, &candidate(1), 0).dir,
            0.02891
        ));

        ctx.dir = None;
        assert_eq!(heuristic(&index, &ctx, &candidate(0), 0).dir, 0.0);
    }

    #[test]
    fn tc_b_03_an_old_directory_regular_beats_a_command_run_here_never_once_it_is_an_hour_old() {
        let mut index = index_of(&["a", "b", "a2"]);
        let dir = index.dirs.intern("/w");
        index.record_run(0, NOW - 10, None, None, Some(0), None);
        directory_regular(&mut index, 1, dir);
        index.record_run(2, NOW - 7200, None, None, Some(0), None);
        let mut ctx = context(NOW);
        ctx.dir = Some(dir);
        assert_eq!(
            order(&index, &ctx, "", &[candidate(0), candidate(1)]),
            [1, 2]
        );
        assert_eq!(
            order(&index, &ctx, "", &[candidate(1), candidate(2)]),
            [2, 3]
        );
    }

    #[test]
    fn tc_b_04_ties_go_to_the_newer_run_then_the_larger_id() {
        let mut index = CommandIndex::default();
        index.add_command(5, "x");
        index.add_command(9, "y");
        let ctx = context(NOW);
        // Never run: identical stats.
        assert_eq!(
            order(&index, &ctx, "", &[candidate(0), candidate(1)]),
            [9, 5]
        );

        // Equal scores but different last runs: both long enough ago that
        // recency rounds to the same f64 is not guaranteed, so compare the
        // order function directly with equal scores.
        index.record_run(0, NOW - 20, None, None, Some(0), None);
        index.record_run(1, NOW - 30, None, None, Some(0), None);
        let score = |idx| CommandScore {
            idx,
            score: 0.5,
            heuristic: 0.5,
            parts: HeuristicParts::default(),
            ml_score: None,
            ml_weight: 0.0,
            fuzzy_score: 0,
        };
        assert_eq!(rank_order(&index, &score(0), &score(1)), Ordering::Less);
    }

    #[test]
    fn tc_b_05_fuzzy_term_is_relative_to_the_best_match() {
        let mut index = index_of(&["git status", "git stash"]);
        index.record_run(0, NOW - 100, None, None, Some(0), None);
        index.record_run(1, NOW - 100, None, None, Some(0), None);
        let ctx = context(NOW);
        let candidates = [
            CommandCandidate {
                idx: 0,
                fuzzy_score: 40,
            },
            CommandCandidate {
                idx: 1,
                fuzzy_score: 80,
            },
        ];
        let ranking = rank_commands("gs", &candidates, &index, &ctx, usize::MAX, None);
        let fuzzy: Vec<(u32, f64)> = ranking
            .scores
            .iter()
            .map(|s| (s.idx, s.parts.fuzzy))
            .collect();
        assert_eq!(fuzzy, [(1, 1.0), (0, 0.5)]);
        assert!(
            ranking
                .scores
                .iter()
                .all(|s| s.ml_score.is_none() && s.ml_weight == 0.0 && s.score == s.heuristic)
        );
        assert!(ranking.features.is_empty() && ranking.features_of(0).is_empty());
        assert_eq!(ranking.timings.stage2, 0);
        assert_eq!(ranking.timings.features_ms, None);

        let non_positive = [
            CommandCandidate {
                idx: 0,
                fuzzy_score: 0,
            },
            CommandCandidate {
                idx: 1,
                fuzzy_score: -3,
            },
        ];
        let ranking = rank_commands("gs", &non_positive, &index, &ctx, usize::MAX, None);
        assert!(ranking.scores.iter().all(|s| s.parts.fuzzy == 0.0));
    }

    #[test]
    fn a_sorted_head_is_the_head_of_the_full_order_and_the_rest_sorts_later() {
        let texts: Vec<String> = (0..CMD_PAR_FILTER_MIN + 500)
            .map(|i| format!("c{i}"))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut index = index_of(&refs);
        for i in 0..refs.len() as u32 {
            index.record_run(
                i,
                NOW - i64::from((i * 7919) % 1000) * 30,
                None,
                None,
                Some(0),
                None,
            );
        }
        let ctx = context(NOW);
        let all: Vec<CommandCandidate> = (0..refs.len() as u32).map(candidate).collect();
        let full = order(&index, &ctx, "", &all);
        for head in [0, 1, 200, refs.len() - 1] {
            let mut ranking = rank_commands("", &all, &index, &ctx, head, None);
            assert_eq!(ranking.sorted, head);
            let ids = |r: &CommandRanking| -> Vec<i64> {
                r.scores
                    .iter()
                    .map(|s| index.entries[s.idx as usize].db_id)
                    .collect()
            };
            assert_eq!(ids(&ranking)[..head], full[..head], "head {head}");
            let mut rest = ids(&ranking)[head..].to_vec();
            let mut expected = full[head..].to_vec();
            rest.sort();
            expected.sort();
            assert_eq!(rest, expected, "the rest holds the rest");
            ranking.sort_rest(&index);
            assert_eq!(ranking.sorted, refs.len());
            assert_eq!(ids(&ranking), full);
        }
    }

    /// Six commands whose heuristic order is idx 0..6.
    fn six() -> (CommandIndex, RankContext, Vec<CommandCandidate>) {
        let mut index = index_of(&["a0", "a1", "a2", "a3", "a4", "a5"]);
        for i in 0..6u32 {
            index.record_run(i, NOW - i64::from(i + 1) * 1000, None, None, Some(0), None);
        }
        (index, context(NOW), (0..6).map(candidate).collect())
    }

    fn idxs(ranking: &CommandRanking) -> Vec<u32> {
        ranking.scores.iter().map(|s| s.idx).collect()
    }

    /// Raw scores rising with the row: the model reverses what it is given.
    fn reversing(flat: &[f64], features: usize) -> anyhow::Result<Vec<f64>> {
        assert_eq!(features, COMMAND_FEATURE_REGISTRY.len());
        Ok((0..flat.len() / features).map(|row| row as f64).collect())
    }

    #[test]
    fn permute_moves_each_item_to_its_new_place() {
        let mut items = ['a', 'b', 'c', 'd', 'e'];
        permute(&mut items, &[3, 0, 4, 1, 2]);
        assert_eq!(items, ['d', 'a', 'e', 'b', 'c']);
        let mut same = [1, 2, 3];
        permute(&mut same, &[0, 1, 2]);
        assert_eq!(same, [1, 2, 3]);
        let mut none: [u8; 0] = [];
        permute(&mut none, &[]);
    }

    #[test]
    fn tc_l_17_the_model_reorders_only_its_stage() {
        let (index, ctx, all) = six();
        let heuristic = rank_with("", &all, &index, &ctx, usize::MAX, None);
        assert_eq!(idxs(&heuristic), [0, 1, 2, 3, 4, 5]);

        let stage2 = Stage2 {
            k: 3,
            ml_weight: 1.0,
            predict: &reversing,
        };
        let ranking = rank_with("", &all, &index, &ctx, usize::MAX, Some(stage2));
        assert_eq!(idxs(&ranking), [2, 1, 0, 3, 4, 5]);
        assert!(
            ranking.scores[..3]
                .iter()
                .all(|s| s.ml_score.is_some() && s.ml_weight == 1.0)
        );
        assert!(
            ranking.scores[3..]
                .iter()
                .all(|s| s.ml_score.is_none() && s.score == s.heuristic)
        );
        let width = COMMAND_FEATURE_REGISTRY.len();
        assert!((0..3).all(|rank| ranking.features_of(rank).len() == width));
        assert!(ranking.features_of(3).is_empty());
        // The features moved with their rows: command_length of "aN" is 2,
        // and the log age of row 0 (idx 2, run 3,000 s ago) is ln(3001).
        let age = crate::cmd::features::command_feature_names()
            .iter()
            .position(|n| *n == "log_seconds_since_last_run")
            .unwrap();
        assert_eq!(ranking.features_of(0)[age], 3001f64.ln());
        assert_eq!(ranking.features_of(2)[age], 1001f64.ln());
        assert_eq!(ranking.scores[0].score, crate::ranker::logistic(2.0));
        assert_eq!(ranking.timings.stage2, 3);
        assert!(ranking.timings.features_ms.is_some() && ranking.timings.predict_ms.is_some());
        assert!(!ranking.model_failed);

        let everything = Stage2 {
            k: usize::MAX,
            ml_weight: 1.0,
            predict: &reversing,
        };
        let ranking = rank_with("", &all, &index, &ctx, usize::MAX, Some(everything));
        assert_eq!(idxs(&ranking), [5, 4, 3, 2, 1, 0]);
        assert!(ranking.scores.iter().all(|s| s.ml_score.is_some()));
    }

    #[test]
    fn stage_two_takes_its_rows_from_a_sorted_head_and_keeps_the_head_size() {
        let (index, ctx, all) = six();
        let stage2 = Stage2 {
            k: 4,
            ml_weight: 0.5,
            predict: &reversing,
        };
        let ranking = rank_with("", &all, &index, &ctx, 2, Some(stage2));
        assert_eq!(ranking.sorted, 4, "max(head, k)");
        let mut head = idxs(&ranking)[..4].to_vec();
        head.sort_unstable();
        assert_eq!(head, [0, 1, 2, 3], "the heuristic top 4 were scored");
        let blended = &ranking.scores[0];
        let expected = 0.5 * blended.heuristic + 0.5 * blended.ml_score.unwrap();
        assert_eq!(blended.score, expected);
    }

    #[test]
    fn a_failed_stage_two_still_sorts_the_head_and_the_rest_later() {
        let (index, ctx, all) = six();
        let failing =
            |_: &[f64], _: usize| -> anyhow::Result<Vec<f64>> { Err(anyhow::anyhow!("no")) };
        let stage2 = Stage2 {
            k: 4,
            ml_weight: 1.0,
            predict: &failing,
        };
        let mut ranking = rank_with("", &all, &index, &ctx, 2, Some(stage2));
        assert!(ranking.model_failed);
        assert_eq!(ranking.sorted, 2);
        assert_eq!(idxs(&ranking)[..2], [0, 1]);
        ranking.sort_rest(&index);
        assert_eq!(idxs(&ranking), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn e_l_07_a_failed_prediction_leaves_the_heuristic_order() {
        let (index, ctx, all) = six();
        let failing = |_: &[f64], _: usize| -> anyhow::Result<Vec<f64>> {
            Err(anyhow::anyhow!("LightGBM said no"))
        };
        let short = |_: &[f64], _: usize| -> anyhow::Result<Vec<f64>> { Ok(vec![1.0]) };
        for predict in [&failing as Predict, &short] {
            let stage2 = Stage2 {
                k: 3,
                ml_weight: 1.0,
                predict,
            };
            let ranking = rank_with("", &all, &index, &ctx, usize::MAX, Some(stage2));
            assert!(ranking.model_failed);
            assert_eq!(idxs(&ranking), [0, 1, 2, 3, 4, 5]);
            assert!(
                ranking
                    .scores
                    .iter()
                    .all(|s| s.ml_score.is_none() && s.score == s.heuristic)
            );
            assert!(ranking.features.is_empty());
            assert_eq!(ranking.timings.stage2, 0);
            assert_eq!(ranking.timings.predict_ms, None);
        }
    }

    #[test]
    fn the_parallel_path_orders_like_the_sequential_one() {
        let texts: Vec<String> = (0..CMD_PAR_FILTER_MIN + 10)
            .map(|i| format!("c{i}"))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut index = index_of(&refs);
        for i in 0..refs.len() as u32 {
            // Many equal last runs, so the tie-breaks decide.
            index.record_run(i, NOW - i64::from(i % 7) * 60, None, None, Some(0), None);
        }
        let ctx = context(NOW);
        let all: Vec<CommandCandidate> = (0..refs.len() as u32).map(candidate).collect();
        let parallel = order(&index, &ctx, "", &all);
        let few = order(&index, &ctx, "", &all[..50]);
        let mut expected: Vec<(i64, i64)> = (0..refs.len())
            .map(|i| (-(i as i64 % 7), i as i64 + 1))
            .collect();
        expected.sort_by(|a, b| b.cmp(a));
        assert_eq!(parallel, expected.iter().map(|e| e.1).collect::<Vec<_>>());
        let mut expected_few: Vec<(i64, i64)> =
            expected.iter().copied().filter(|e| e.1 <= 50).collect();
        expected_few.sort_by(|a, b| b.cmp(a));
        assert_eq!(few, expected_few.iter().map(|e| e.1).collect::<Vec<_>>());
    }
}
