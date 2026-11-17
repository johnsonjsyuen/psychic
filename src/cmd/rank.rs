//! Ranking the commands that matched (`docs/ctrl-r/02-browse.md` section 7).
//!
//! In this phase the heuristic is the whole ranking. Phase C puts a model
//! stage on top (`03-learn.md` section 7.2): it re-scores the head of the
//! heuristic order this module produces and fills `ml_score`, `ml_weight`
//! and `features`, which stay `None`, `0.0` and empty here.

use super::index::{CommandIndex, RankContext};
use super::tuning::{
    CMD_H_DIR_SATURATION, CMD_H_RECENCY_SCALE_SECS, CMD_H_W_DIR, CMD_H_W_FUZZY, CMD_H_W_RECENCY,
    CMD_H_W_REPO, CMD_PAR_FILTER_MIN,
};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::time::Instant;

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
    /// Registry order; empty when no model scored the row.
    pub features: Vec<f64>,
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
}

impl CommandRanking {
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

/// Rank `candidates` for `query`: the heuristic for each, ordered by
/// [`rank_order`], the first `head` (or all, if fewer) exactly and the rest
/// after them unsorted ([`CommandRanking::sort_rest`] sorts them). The query
/// only switches the fuzzy term on; matching happened in the filter.
///
/// A screen shows a few dozen rows, so sorting 50,000 to show them is most
/// of the cost of an empty query: the head is selected in linear time and
/// only it is sorted. What is selected and sorted is a 32-byte key per
/// candidate; the scores are built once, in that order, afterwards.
pub(super) fn rank_commands(
    query: &str,
    candidates: &[CommandCandidate],
    index: &CommandIndex,
    ctx: &RankContext,
    head: usize,
) -> CommandRanking {
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
    let sorted = head.min(keys.len());
    if sorted < keys.len() {
        if sorted > 0 {
            keys.select_nth_unstable_by(sorted - 1, compare);
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
            features: Vec::new(),
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
            heuristic_ms: started.elapsed().as_secs_f64() * 1000.0,
            ..CmdRankTimings::default()
        },
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
        rank_commands(query, c, index, ctx, usize::MAX)
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
            features: Vec::new(),
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
        let ranking = rank_commands("gs", &candidates, &index, &ctx, usize::MAX);
        let fuzzy: Vec<(u32, f64)> = ranking
            .scores
            .iter()
            .map(|s| (s.idx, s.parts.fuzzy))
            .collect();
        assert_eq!(fuzzy, [(1, 1.0), (0, 0.5)]);
        assert!(ranking.scores.iter().all(|s| s.ml_score.is_none()
            && s.ml_weight == 0.0
            && s.features.is_empty()
            && s.score == s.heuristic));
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
        let ranking = rank_commands("gs", &non_positive, &index, &ctx, usize::MAX);
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
            let mut ranking = rank_commands("", &all, &index, &ctx, head);
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
