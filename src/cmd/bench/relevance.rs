//! `bench-history relevance`: replay the history, train at a cut, and score
//! every ranker on what was run after it (`docs/ctrl-r/04-bench.md` section 5).
//!
//! Everything that decides an order is production code (ADR-16): the
//! replay and the training set (`train::build_training_set`), the fit
//! (`train::fit_command_model`), the model files and their loader
//! (`CmdModelStats::from_fit`, `CmdModel::load`), the filter
//! (`worker::filter_candidates`) and the ranking (`rank::rank_commands`).
//! The only orders defined here are the two baselines of section 5.3.
//!
//! Leakage guards, in the order a lookup meets them: the model is trained on
//! the timeline before the cut only (`BuildOptions.until`); the test replay
//! starts from the state at the cut; every lookup is ranked before its run is
//! applied (and `debug_assert`s that its target has not run at or after it);
//! a never-run-before target is counted as unreachable, never as a miss.
//!
//! No command or query text leaves this module: counts, shares and ranks.

use super::super::corpus::splitmix64;
use super::super::fuzzy;
use super::super::index::{CommandIndex, RankContext};
use super::super::model::CmdModel;
use super::super::rank::{CommandCandidate, rank_commands, stage2_rows};
use super::super::store::{CmdStore, RunRecord, Source};
use super::super::train::{
    BuildOptions, CmdModelStats, ReplayInput, Replayer, Step, Timeline, build_training_set,
    fit_command_model, fragment_query,
};
use super::super::tuning::{CMD_TRAIN_MAX_RUN_GROUPS, CMD_TRAIN_MIN_GROUPS};
use super::super::worker::{Scope, filter_candidates};
use super::CorpusStats;
use anyhow::{Context, Result};
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Targets (00-blueprint.md section 2 is their definition; copied, not tuned)
// ---------------------------------------------------------------------------

/// R1: model top-1 minus recency top-1 on (holdout, gt10min, length 2).
const R1_MIN: f64 = 0.10;
/// R2: the same difference on each of the 8 headline cells.
const R2_MIN: f64 = -0.01;
/// R3: heuristic minus recency on every cell of {all, gt10min, other_dir} x
/// 4 lengths x 4 metrics.
const R3_MIN: f64 = -0.001;
/// R4: model minus fzf's order (the cold start) on every (all, gt10min) x
/// fragment and infix length cell, top-1.
const R4_MIN: f64 = -0.01;
/// A cell with fewer lookups is printed but marked `low n` (E-N-06).
const LOW_N: usize = 200;
/// Fewer seeds that trained than this makes R1 and R2 inconclusive (E-N-12).
const MIN_SEEDS: usize = 3;
/// A history with fewer runs is not split (E-N-05).
const MIN_RUNS: usize = 100;
/// Prefix query lengths measured, in characters: the headline style.
const LENGTHS: [usize; 4] = [0, 1, 2, 3];
/// Fragment query lengths measured, in characters (section 5.2).
const FRAGMENT_LENGTHS: [usize; 4] = [1, 2, 3, 5];
/// Mixed into a run id to choose the word of its fragment queries.
const FRAGMENT_SALT: u64 = 0x5EED_F4A6_0000_0001;
/// Infix query lengths measured, in characters (section 5.2).
const INFIX_LENGTHS: [usize; 4] = [2, 3, 4, 5];
/// Mixed into a run id to choose where its infix queries start.
const INFIX_SALT: u64 = 0x1F1C_5EED_0000_0002;
/// The reference line of section 5.6.
const FILE_REFERENCE: &str = "file search on its own benchmark: top-1 0.7960, MRR 0.8656 \
                              (different task; see 00-blueprint.md section 2)";
/// What a rank is when the target did not match its own prefix.
const NOT_MATCHED: u32 = u32::MAX;
/// How `training::fit_capped` says a time split left a part empty.
const NOT_ENOUGH_FOR_A_SPLIT: &str = "every split needs rows";

// ---------------------------------------------------------------------------
// Rankers and slices
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ranker {
    Recency,
    FzfScored,
    /// fzf's own Ctrl-R order: its `--scheme=history` score, then newest.
    Fzf,
    Heuristic,
    Model,
    /// Information: the share of lookups whose target is among the rows the
    /// model scores (`rank::stage2_rows`: fzf's top `CMD_STAGE2_K` and the
    /// directory's most-run matches).
    Stage1Recall,
}

const RANKERS: [Ranker; 5] = [
    Ranker::Recency,
    Ranker::FzfScored,
    Ranker::Fzf,
    Ranker::Heuristic,
    Ranker::Model,
];

/// The information rows printed under the rankers.
const INFO_ROWS: [Ranker; 1] = [Ranker::Stage1Recall];

impl Ranker {
    fn key(self) -> &'static str {
        match self {
            Ranker::Recency => "recency",
            Ranker::FzfScored => "fzf_scored",
            Ranker::Fzf => "fzf",
            Ranker::Heuristic => "heuristic",
            Ranker::Model => "model",
            Ranker::Stage1Recall => "stage1_recall",
        }
    }
}

/// How a lookup's query is made from its target's text (section 5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QueryStyle {
    /// The first characters of the text: the headline style.
    Prefix,
    /// The first characters of a word inside the text.
    Fragment,
    /// Characters from inside a word after the first, not at its start: a
    /// rule training never uses (section 5.2).
    Infix,
}

impl QueryStyle {
    fn key(self) -> &'static str {
        match self {
            QueryStyle::Prefix => "prefix",
            QueryStyle::Fragment => "fragment",
            QueryStyle::Infix => "infix",
        }
    }

    fn lengths(self) -> [usize; 4] {
        match self {
            QueryStyle::Prefix => LENGTHS,
            QueryStyle::Fragment => FRAGMENT_LENGTHS,
            QueryStyle::Infix => INFIX_LENGTHS,
        }
    }
}

/// Every (style, length) measured, in output order.
const QUERIES: [(QueryStyle, usize); 12] = [
    (QueryStyle::Prefix, 0),
    (QueryStyle::Prefix, 1),
    (QueryStyle::Prefix, 2),
    (QueryStyle::Prefix, 3),
    (QueryStyle::Fragment, 1),
    (QueryStyle::Fragment, 2),
    (QueryStyle::Fragment, 3),
    (QueryStyle::Fragment, 5),
    (QueryStyle::Infix, 2),
    (QueryStyle::Infix, 3),
    (QueryStyle::Infix, 4),
    (QueryStyle::Infix, 5),
];

/// The query of `style` and `len` for a target, or `None` when the text
/// is too short for it.
fn make_query(style: QueryStyle, text: &str, len: usize, run_id: i64) -> Option<String> {
    match style {
        QueryStyle::Prefix => {
            (text.chars().count() >= len).then(|| text.chars().take(len).collect())
        }
        QueryStyle::Fragment => {
            fragment_query(text, len, splitmix64(run_id as u64 ^ FRAGMENT_SALT))
        }
        QueryStyle::Infix => infix_query(text, len, splitmix64(run_id as u64 ^ INFIX_SALT)),
    }
}

/// An infix query (section 5.2): `len` characters of a word after the first,
/// starting at a letter or digit that is not the word's first letter or
/// digit (inside the word, or after a `/`, `.`, `-` in it). `pick` chooses
/// among every such start in the text; `None` when there is none. Training's
/// queries never start there ([`fragment_query`] is the word's start), so
/// this measures a query rule the model was not trained on.
pub(super) fn infix_query(text: &str, len: usize, pick: u64) -> Option<String> {
    let mut starts: Vec<&str> = Vec::new();
    for word in text.split_whitespace().skip(1) {
        let Some(first) = word.find(|c: char| c.is_alphanumeric()) else {
            continue;
        };
        let word = &word[first..];
        for (at, c) in word.char_indices().skip(1) {
            if c.is_alphanumeric() && word[at..].chars().count() >= len {
                starts.push(&word[at..]);
            }
        }
    }
    if starts.is_empty() {
        return None;
    }
    let start = starts[(pick % starts.len() as u64) as usize];
    Some(start.chars().take(len).collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Slice {
    All,
    Gt10min,
    OtherDir,
    Hook,
    Import,
    Repeat,
}

const SLICES: [Slice; 6] = [
    Slice::All,
    Slice::Gt10min,
    Slice::OtherDir,
    Slice::Hook,
    Slice::Import,
    Slice::Repeat,
];

impl Slice {
    fn key(self) -> &'static str {
        match self {
            Slice::All => "all",
            Slice::Gt10min => "gt10min",
            Slice::OtherDir => "other_dir",
            Slice::Hook => "hook",
            Slice::Import => "import",
            Slice::Repeat => "repeat",
        }
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// What decides a lookup's slices, all as of just before its run.
#[derive(Clone, Copy)]
pub(super) struct LookupFacts {
    pub started_at: i64,
    /// The target's newest run.
    pub target_last_run_at: i64,
    /// The run's session starts with `import-synth:` (fake timestamps).
    pub synthetic_time: bool,
    /// The run's cwd is known (non-empty).
    pub cwd_known: bool,
    /// The dir id of the run's cwd, if that directory was seen before.
    pub run_dir: Option<u32>,
    /// The dir id of the target's newest run with a known cwd.
    pub target_last_dir: Option<u32>,
    pub source: Source,
    /// The target is the newest command of the run's shell session.
    pub session_repeat: bool,
}

/// The slices of section 5.4 a lookup belongs to, as a bit set.
pub(super) fn slices_of(facts: &LookupFacts) -> u8 {
    let mut bits = Slice::All.bit();
    if !facts.synthetic_time && facts.target_last_run_at < facts.started_at - 600 {
        bits |= Slice::Gt10min.bit();
    }
    // A cwd never seen before differs from any directory the target ran in.
    if facts.cwd_known
        && let Some(last) = facts.target_last_dir
        && facts.run_dir != Some(last)
    {
        bits |= Slice::OtherDir.bit();
    }
    bits |= match facts.source {
        Source::Hook => Slice::Hook.bit(),
        Source::Import => Slice::Import.bit(),
    };
    if facts.session_repeat {
        bits |= Slice::Repeat.bit();
    }
    bits
}

// ---------------------------------------------------------------------------
// The two baselines (section 5.3)
// ---------------------------------------------------------------------------

/// Something ranked by a baseline: it names a command.
pub(super) trait Ranked {
    fn idx(&self) -> u32;
}

impl Ranked for CommandCandidate {
    fn idx(&self) -> u32 {
        self.idx
    }
}

/// A candidate with Skim's score, for the `fzf_scored` row.
#[derive(Clone, Copy, Debug)]
pub(super) struct SkimScored {
    pub idx: u32,
    /// `fuzzy::skim_score` (every candidate matches by Skim's rule).
    pub skim: i64,
}

impl Ranked for SkimScored {
    fn idx(&self) -> u32 {
        self.idx
    }
}

/// `recency`: newest last run first, then the larger database id.
pub(super) fn recency_order<T: Ranked>(index: &CommandIndex, a: &T, b: &T) -> Ordering {
    let (ea, eb) = (
        &index.entries[a.idx() as usize],
        &index.entries[b.idx() as usize],
    );
    eb.last_run_at
        .cmp(&ea.last_run_at)
        .then(eb.db_id.cmp(&ea.db_id))
}

/// `fzf_scored`: the higher Skim score first, then as `recency`.
pub(super) fn fzf_scored_order(index: &CommandIndex, a: &SkimScored, b: &SkimScored) -> Ordering {
    b.skim.cmp(&a.skim).then_with(|| recency_order(index, a, b))
}

/// `fzf`: fzf's score under `--scheme=history` first (the filter's, which
/// every candidate carries), then as `recency`. 1-based rank of `target`.
fn fzf_rank(index: &CommandIndex, candidates: &[CommandCandidate], target: u32) -> u32 {
    rank_by(candidates, target, |a, b| {
        b.fzf.cmp(&a.fzf).then_with(|| recency_order(index, a, b))
    })
}

/// 1-based rank of `target` among `candidates` under a total `order`: one
/// more than the number that come before it. `NOT_MATCHED` if it is absent.
fn rank_by<T: Ranked>(candidates: &[T], target: u32, order: impl Fn(&T, &T) -> Ordering) -> u32 {
    let Some(t) = candidates.iter().find(|c| c.idx() == target) else {
        return NOT_MATCHED;
    };
    1 + candidates
        .iter()
        .filter(|c| order(c, t) == Ordering::Less)
        .count() as u32
}

// ---------------------------------------------------------------------------
// Metrics (section 5.5)
// ---------------------------------------------------------------------------

/// The four metrics of one list of ranks; NaN when there are none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Metrics {
    pub n: usize,
    pub top1: f64,
    pub top3: f64,
    pub top10: f64,
    pub mrr: f64,
}

impl Metrics {
    /// `ranks` are 1-based; `NOT_MATCHED` counts as a miss everywhere.
    pub(super) fn of(ranks: &[u32]) -> Self {
        let n = ranks.len();
        if n == 0 {
            return Metrics {
                n,
                top1: f64::NAN,
                top3: f64::NAN,
                top10: f64::NAN,
                mrr: f64::NAN,
            };
        }
        let share = |k: u32| ranks.iter().filter(|&&r| r <= k).count() as f64 / n as f64;
        let mrr = ranks
            .iter()
            .map(|&r| {
                if r == NOT_MATCHED {
                    0.0
                } else {
                    1.0 / f64::from(r)
                }
            })
            .sum::<f64>()
            / n as f64;
        Metrics {
            n,
            top1: share(1),
            top3: share(3),
            top10: share(10),
            mrr,
        }
    }

    fn get(&self, metric: Metric) -> f64 {
        match metric {
            Metric::Top1 => self.top1,
            Metric::Top3 => self.top3,
            Metric::Top10 => self.top10,
            Metric::Mrr => self.mrr,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Metric {
    Top1,
    Top3,
    Top10,
    Mrr,
}

const METRICS: [Metric; 4] = [Metric::Top1, Metric::Top3, Metric::Top10, Metric::Mrr];

impl Metric {
    fn key(self) -> &'static str {
        match self {
            Metric::Top1 => "top1",
            Metric::Top3 => "top3",
            Metric::Top10 => "top10",
            Metric::Mrr => "mrr",
        }
    }
}

/// One metric over seeds: mean, sample standard deviation, min, max.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Spread {
    pub mean: f64,
    pub sd: f64,
    pub min: f64,
    pub max: f64,
}

impl Spread {
    /// One seed (or an unseeded ranker) has `sd` 0. No values, or a NaN
    /// among them (a cell without lookups), give NaN throughout.
    pub(super) fn of(values: &[f64]) -> Self {
        if values.is_empty() || values.iter().any(|v| v.is_nan()) {
            return Spread {
                mean: f64::NAN,
                sd: f64::NAN,
                min: f64::NAN,
                max: f64::NAN,
            };
        }
        let n = values.len() as f64;
        let mean = values.iter().sum::<f64>() / n;
        let sd = if values.len() < 2 {
            0.0
        } else {
            (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
        };
        Spread {
            mean,
            sd,
            min: values.iter().copied().fold(f64::INFINITY, f64::min),
            max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        }
    }

    fn to_json(self) -> Value {
        json!({ "mean": round6(self.mean), "sd": round6(self.sd),
                "min": round6(self.min), "max": round6(self.max) })
    }
}

/// JSON has no NaN: an empty cell's values are `null`.
fn round6(value: f64) -> Value {
    if value.is_finite() {
        json!((value * 1e6).round() / 1e6)
    } else {
        Value::Null
    }
}

/// One (ranker, slice, length) cell of a split.
#[derive(Clone, Debug)]
pub(super) struct Cell {
    pub ranker: Ranker,
    pub slice: Slice,
    pub style: QueryStyle,
    pub len: usize,
    pub n: usize,
    /// In `METRICS` order.
    pub values: [Spread; 4],
}

impl Cell {
    fn mean(&self, metric: Metric) -> f64 {
        self.values[metric as usize].mean
    }

    fn to_json(&self) -> Value {
        let mut cell = json!({ "ranker": self.ranker.key(), "slice": self.slice.key(),
                               "style": self.style.key(), "len": self.len, "n": self.n });
        for metric in METRICS {
            cell[metric.key()] = self.values[metric as usize].to_json();
        }
        cell
    }
}

// ---------------------------------------------------------------------------
// Verdicts (section 5.5)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Pass,
    Fail,
    Inconclusive,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Inconclusive => "INCONCLUSIVE",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Verdict {
    pub value: f64,
    /// `slice/lenN` (and `/metric` for R3) of the deciding cell.
    pub cell: String,
    pub n: usize,
    pub status: Status,
}

/// The worst of `diffs` (cell name, n, model or heuristic minus recency)
/// against `bar`. A cell with `n >= LOW_N` that fails decides FAIL; else a
/// low-n or empty cell makes the verdict INCONCLUSIVE; else PASS.
fn worst_verdict(diffs: &[(String, usize, f64)], bar: f64, enough_seeds: bool) -> Verdict {
    let comparable = |n: usize, d: f64| n > 0 && d.is_finite();
    let worst = diffs
        .iter()
        .filter(|(_, n, d)| comparable(*n, *d))
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let failing_solid = diffs
        .iter()
        .filter(|(_, n, d)| *n >= LOW_N && d.is_finite() && *d < bar)
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let any_weak = diffs.iter().any(|(_, n, d)| *n < LOW_N || !d.is_finite());
    let (cell, n, value) = match failing_solid.or(worst) {
        Some((cell, n, value)) => (cell.clone(), *n, *value),
        None => (String::from("-"), 0, f64::NAN),
    };
    let status = if !enough_seeds {
        Status::Inconclusive
    } else if failing_solid.is_some() {
        Status::Fail
    } else if any_weak || worst.is_none() {
        Status::Inconclusive
    } else {
        Status::Pass
    };
    Verdict {
        value,
        cell,
        n,
        status,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Verdicts {
    pub r1: Verdict,
    pub r2: Verdict,
    pub r3: Verdict,
    pub r4: Verdict,
}

impl Verdicts {
    pub(super) fn all_pass(&self) -> bool {
        [&self.r1, &self.r2, &self.r3, &self.r4]
            .iter()
            .all(|v| v.status == Status::Pass)
    }
}

/// R1-R3 from the holdout cells; `seeds` is how many trained.
pub(super) fn verdicts(cells: &[Cell], seeds: usize) -> Verdicts {
    let find = |ranker: Ranker, slice: Slice, len: usize| {
        cells.iter().find(|c| {
            c.ranker == ranker && c.slice == slice && c.style == QueryStyle::Prefix && c.len == len
        })
    };
    let diff = |ranker: Ranker, slice: Slice, len: usize, metric: Metric| -> (usize, f64) {
        match (find(ranker, slice, len), find(Ranker::Recency, slice, len)) {
            (Some(a), Some(b)) => (a.n, a.mean(metric) - b.mean(metric)),
            _ => (0, f64::NAN),
        }
    };
    let enough_seeds = seeds >= MIN_SEEDS;

    let (n, value) = diff(Ranker::Model, Slice::Gt10min, 2, Metric::Top1);
    let r1 = worst_verdict(&[("gt10min/len2".into(), n, value)], R1_MIN, enough_seeds);

    let headline: Vec<(String, usize, f64)> = [Slice::All, Slice::Gt10min]
        .iter()
        .flat_map(|&slice| LENGTHS.iter().map(move |&len| (slice, len)))
        .map(|(slice, len)| {
            let (n, d) = diff(Ranker::Model, slice, len, Metric::Top1);
            (format!("{}/len{}", slice.key(), len), n, d)
        })
        .collect();
    let r2 = worst_verdict(&headline, R2_MIN, enough_seeds);

    let mut r3_cells = Vec::new();
    for slice in [Slice::All, Slice::Gt10min, Slice::OtherDir] {
        for len in LENGTHS {
            for metric in METRICS {
                let (n, d) = diff(Ranker::Heuristic, slice, len, metric);
                r3_cells.push((format!("{}/len{}/{}", slice.key(), len, metric.key()), n, d));
            }
        }
    }
    // The heuristic has no seeds.
    let r3 = worst_verdict(&r3_cells, R3_MIN, true);

    // R4: no query style is worse with the model than without it.
    let other_styles: Vec<(String, usize, f64)> = cells
        .iter()
        .filter(|c| {
            c.ranker == Ranker::Model
                && c.style != QueryStyle::Prefix
                && matches!(c.slice, Slice::All | Slice::Gt10min)
        })
        .map(|model| {
            let fzf = cells.iter().find(|c| {
                c.ranker == Ranker::Fzf
                    && c.slice == model.slice
                    && c.style == model.style
                    && c.len == model.len
            });
            let d = fzf.map_or(f64::NAN, |fzf| {
                model.mean(Metric::Top1) - fzf.mean(Metric::Top1)
            });
            let name = format!("{}/{}{}", model.slice.key(), model.style.key(), model.len);
            (name, model.n, d)
        })
        .collect();
    let r4 = worst_verdict(&other_styles, R4_MIN, enough_seeds);
    Verdicts { r1, r2, r3, r4 }
}

// ---------------------------------------------------------------------------
// Splits and sampling (sections 5.1, 5.2)
// ---------------------------------------------------------------------------

/// A training cut and the test window after it.
#[derive(Clone, Debug)]
pub(super) struct Split {
    pub name: &'static str,
    pub cut: f64,
    /// `None`: to the end of the history.
    pub end: Option<f64>,
}

fn splits(folds: bool) -> Vec<Split> {
    let mut splits = vec![Split {
        name: "holdout",
        cut: 0.80,
        end: None,
    }];
    if folds {
        for (name, cut) in [("fold60", 0.60), ("fold70", 0.70), ("fold80", 0.80)] {
            splits.push(Split {
                name,
                cut,
                end: Some(cut + 0.10),
            });
        }
    }
    splits
}

/// `started_at` of `runs[floor(f * len)]` (the last run for `f >= 1`).
fn time_at(runs: &[RunRecord], f: f64) -> i64 {
    let at = ((f * runs.len() as f64).floor() as usize).min(runs.len() - 1);
    runs[at].started_at
}

/// Every `stride`-th reachable run, `stride = max(1, reachable / max)`.
pub(super) fn stride(reachable: usize, max_test_runs: usize) -> usize {
    (reachable / max_test_runs.max(1)).max(1)
}

/// What the test window holds, by the pre-pass of step 3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reach {
    reachable: usize,
    unreachable_hook: usize,
    unreachable_import: usize,
    reachable_hook: usize,
    reachable_import: usize,
}

impl Reach {
    fn shares(&self) -> [f64; 3] {
        let share = |u: usize, r: usize| {
            if u + r == 0 {
                f64::NAN
            } else {
                u as f64 / (u + r) as f64
            }
        };
        [
            share(
                self.unreachable_hook + self.unreachable_import,
                self.reachable,
            ),
            share(self.unreachable_hook, self.reachable_hook),
            share(self.unreachable_import, self.reachable_import),
        ]
    }
}

/// The pre-pass: which test runs are reachable (their command ran before,
/// in the history or earlier in the window), and which of those are sampled.
fn sample_window(
    input: &ReplayInput,
    known: &FxHashSet<i64>,
    cut: i64,
    end: Option<i64>,
    max_test_runs: usize,
) -> (Reach, FxHashSet<i64>) {
    let mut seen: FxHashSet<i64> = input
        .runs
        .iter()
        .take_while(|run| run.started_at < cut)
        .map(|run| run.command_id)
        .filter(|id| known.contains(id))
        .collect();
    let window: Vec<&RunRecord> = input
        .runs
        .iter()
        .filter(|run| run.started_at >= cut && end.is_none_or(|end| run.started_at < end))
        .collect();
    let mut reach = Reach::default();
    let mut reachable_ids = Vec::new();
    for run in window {
        let reachable = seen.contains(&run.command_id);
        match (reachable, run.source) {
            (true, Source::Hook) => reach.reachable_hook += 1,
            (true, Source::Import) => reach.reachable_import += 1,
            (false, Source::Hook) => reach.unreachable_hook += 1,
            (false, Source::Import) => reach.unreachable_import += 1,
        }
        if reachable {
            reachable_ids.push(run.id);
        }
        if known.contains(&run.command_id) {
            seen.insert(run.command_id);
        }
    }
    reach.reachable = reachable_ids.len();
    let stride = stride(reach.reachable, max_test_runs);
    let sampled = reachable_ids.into_iter().step_by(stride).collect();
    (reach, sampled)
}

// ---------------------------------------------------------------------------
// Training at a cut
// ---------------------------------------------------------------------------

/// The trained models of one split, and the replay state at its cut.
struct Trained {
    models: Vec<CmdModel>,
    replayer: Replayer,
}

/// What training one seed produced.
struct SeedOutcome {
    seed: u64,
    /// `Ok(None)`: not enough data to train at this cut.
    fit: Result<Option<(String, CmdModelStats)>>,
    /// Seed 1's replay state at the cut (the same for every seed).
    replayer: Option<Replayer>,
}

/// Train seed `seed` on everything before `cut`.
fn train_seed(input: &ReplayInput, cut: i64, seed: u64, shuffle_labels: bool) -> SeedOutcome {
    let started = Instant::now();
    let built = build_training_set(
        input,
        &BuildOptions {
            seed,
            max_run_groups: CMD_TRAIN_MAX_RUN_GROUPS,
            until: Some(cut),
            shuffle_labels,
        },
    );
    let fit = if built.counts.groups() < CMD_TRAIN_MIN_GROUPS {
        Ok(None)
    } else {
        match fit_command_model(&built.set, seed) {
            Ok(fitted) => {
                let stats = CmdModelStats::from_fit(
                    &fitted,
                    &built.counts,
                    seed,
                    started.elapsed().as_secs_f64(),
                    jiff::Timestamp::now(),
                );
                Ok(Some((fitted.model, stats)))
            }
            Err(error) if error.to_string().starts_with(NOT_ENOUGH_FOR_A_SPLIT) => Ok(None),
            Err(error) => Err(error),
        }
    };
    SeedOutcome {
        seed,
        fit,
        replayer: (seed == 1).then_some(built.replayer),
    }
}

/// Steps 1-2: a model per seed, trained on everything before `cut`, written
/// to `bench_dir/<split>-seed<s>/` and loaded back as production would.
/// `None` when there is not enough to train on at this cut.
fn train_at(
    input: &ReplayInput,
    cut: i64,
    split: &str,
    seeds: u64,
    bench_dir: &Path,
    shuffle_labels: bool,
    log: &mut dyn Write,
) -> Result<Option<Trained>> {
    // Training is single-threaded (LightGBM without OpenMP): the seeds train
    // side by side. Each result depends on its seed only.
    let results: Vec<SeedOutcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = (1..=seeds)
            .map(|seed| scope.spawn(move || train_seed(input, cut, seed, shuffle_labels)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });

    let mut replayer = None;
    let mut models = Vec::new();
    for SeedOutcome {
        seed,
        fit,
        replayer: state,
    } in results
    {
        if state.is_some() {
            replayer = state;
        }
        match fit {
            Ok(None) => return Ok(None),
            Ok(Some((model_text, stats))) => {
                let dir = bench_dir.join(format!("{split}-seed{seed}"));
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("could not create {}", dir.display()))?;
                std::fs::write(dir.join("cmd_model.txt"), model_text)?;
                std::fs::write(
                    dir.join("cmd_model_stats.json"),
                    serde_json::to_string_pretty(&stats)?,
                )?;
                match CmdModel::load(&dir) {
                    Some(model) => models.push(model),
                    None => writeln!(log, "{split} seed {seed}: model did not load; seed skipped")?,
                }
            }
            Err(error) => writeln!(
                log,
                "{split} seed {seed}: training failed ({error:#}); seed skipped"
            )?,
        }
    }
    let replayer = replayer.expect("seed 1 always returns its replay state");
    Ok(Some(Trained { models, replayer }))
}

// ---------------------------------------------------------------------------
// The test replay (step 4)
// ---------------------------------------------------------------------------

/// The ranks of every sampled lookup of one split, per query of `QUERIES`.
#[derive(Default)]
struct Ranks {
    /// Per position in `QUERIES`.
    lookups: [Vec<Lookup>; QUERIES.len()],
    /// Lookups whose target did not match its own prefix.
    not_matched: usize,
    /// Sampled runs skipped because they execute a pick.
    after_pick: usize,
}

struct Lookup {
    slices: u8,
    in_stage2: bool,
    recency: u32,
    fzf_scored: u32,
    fzf: u32,
    heuristic: u32,
    model: Vec<u32>,
}

/// What `rank_lookup` measured.
struct LookupRanks {
    /// The target is among the rows the model scores.
    in_stage2: bool,
    recency: u32,
    fzf_scored: u32,
    fzf: u32,
    heuristic: u32,
    model: Vec<u32>,
}

/// Position (1-based) of `target` in a fully sorted ranking.
fn position(scores: &[super::super::rank::CommandScore], target: u32) -> u32 {
    scores
        .iter()
        .position(|s| s.idx == target)
        .map_or(NOT_MATCHED, |at| at as u32 + 1)
}

/// Rank one lookup with every ranker.
fn rank_lookup(
    replayer: &Replayer,
    models: &[CmdModel],
    ctx: &RankContext,
    query: &str,
    target: u32,
) -> LookupRanks {
    let index = &replayer.index;
    let candidates = filter_candidates(index, ctx, query, Scope::All, None);
    let recency = rank_by(&candidates, target, |a, b| recency_order(index, a, b));
    let skim: Vec<SkimScored> = candidates
        .iter()
        .map(|c| SkimScored {
            idx: c.idx,
            skim: fuzzy::skim_score(fuzzy::skim(), query, &index.entries[c.idx as usize].text),
        })
        .collect();
    let fzf_scored = rank_by(&skim, target, |a, b| fzf_scored_order(index, a, b));
    let fzf = fzf_rank(index, &candidates, target);
    let heuristic = position(
        &rank_commands(query, &candidates, index, ctx, usize::MAX, None).scores,
        target,
    );
    let model = models
        .iter()
        .map(|model| {
            position(
                &rank_commands(query, &candidates, index, ctx, usize::MAX, Some(model)).scores,
                target,
            )
        })
        .collect();
    let in_stage2 = stage2_rows(&candidates, index, ctx)
        .iter()
        .any(|c| c.idx == target);
    LookupRanks {
        in_stage2,
        recency,
        fzf_scored,
        fzf,
        heuristic,
        model,
    }
}

/// Walk the test window from the state at the cut: rank each sampled run's
/// command before applying the run.
fn replay_window(
    input: &ReplayInput,
    mut replayer: Replayer,
    models: &[CmdModel],
    cut: i64,
    end: Option<i64>,
    sampled: &FxHashSet<i64>,
) -> Ranks {
    let mut ranks = Ranks::default();
    // shell session -> its newest command id, from the runs before the cut.
    let mut session_last: FxHashMap<&str, i64> = FxHashMap::default();
    for run in input.runs.iter().take_while(|run| run.started_at < cut) {
        session_last.insert(&run.shell_session, run.command_id);
    }
    for step in Timeline::window(input, Some(cut), end) {
        let Step::Run(run) = step else {
            replayer.apply_step(step);
            continue;
        };
        if sampled.contains(&run.id) {
            if replayer.follows_pick(run) {
                ranks.after_pick += 1;
            } else {
                let target = replayer
                    .knows(run.command_id)
                    .expect("a sampled run's command ran before");
                let entry = &replayer.index.entries[target as usize];
                debug_assert!(
                    entry.last_run_at <= run.started_at,
                    "the target's own run is not in the index yet"
                );
                let facts = LookupFacts {
                    started_at: run.started_at,
                    target_last_run_at: entry.last_run_at,
                    synthetic_time: run.shell_session.starts_with("import-synth:"),
                    cwd_known: run.cwd.as_deref().is_some_and(|cwd| !cwd.is_empty()),
                    run_dir: run
                        .cwd
                        .as_deref()
                        .and_then(|cwd| replayer.index.dirs.get(cwd)),
                    target_last_dir: replayer.last_cwd(target),
                    source: run.source,
                    session_repeat: session_last.get(run.shell_session.as_str())
                        == Some(&run.command_id),
                };
                let slices = slices_of(&facts);
                let ctx = replayer.context(
                    run.started_at,
                    run.cwd.as_deref(),
                    run.repo_root.as_deref(),
                    Some(&run.shell_session),
                );
                let text = std::sync::Arc::clone(&entry.text);
                for (at, &(style, len)) in QUERIES.iter().enumerate() {
                    let Some(query) = make_query(style, &text, len, run.id) else {
                        continue;
                    };
                    let measured = rank_lookup(&replayer, models, &ctx, &query, target);
                    if measured.recency == NOT_MATCHED {
                        ranks.not_matched += 1;
                    }
                    ranks.lookups[at].push(Lookup {
                        slices,
                        in_stage2: measured.in_stage2,
                        recency: measured.recency,
                        fzf_scored: measured.fzf_scored,
                        fzf: measured.fzf,
                        heuristic: measured.heuristic,
                        model: measured.model,
                    });
                }
            }
        }
        replayer.apply_run(run);
        session_last.insert(&run.shell_session, run.command_id);
    }
    ranks
}

/// The cells of one split from its ranks.
fn cells_of(ranks: &Ranks, seeds: usize) -> Vec<Cell> {
    let mut cells = Vec::new();
    for slice in SLICES {
        for (at, &(style, len)) in QUERIES.iter().enumerate() {
            let lookups: Vec<&Lookup> = ranks.lookups[at]
                .iter()
                .filter(|l| l.slices & slice.bit() != 0)
                .collect();
            let n = lookups.len();
            let spread = |metrics: Metrics| METRICS.map(|m| Spread::of(&[metrics.get(m)]));
            let unseeded = |pick: fn(&Lookup) -> u32| {
                spread(Metrics::of(
                    &lookups.iter().map(|l| pick(l)).collect::<Vec<_>>(),
                ))
            };
            let mut push = |ranker, values| {
                cells.push(Cell {
                    ranker,
                    slice,
                    style,
                    len,
                    n,
                    values,
                })
            };
            push(Ranker::Recency, unseeded(|l| l.recency));
            push(Ranker::FzfScored, unseeded(|l| l.fzf_scored));
            push(Ranker::Fzf, unseeded(|l| l.fzf));
            push(Ranker::Heuristic, unseeded(|l| l.heuristic));
            let per_seed: Vec<Metrics> = (0..seeds)
                .map(|s| Metrics::of(&lookups.iter().map(|l| l.model[s]).collect::<Vec<_>>()))
                .collect();
            let values = METRICS.map(|m| {
                let values: Vec<f64> = per_seed.iter().map(|metrics| metrics.get(m)).collect();
                Spread::of(&values)
            });
            push(Ranker::Model, values);
            // In stage 2 counts as rank 1: all four metrics are the recall.
            let in_stage2 = |l: &&Lookup| if l.in_stage2 { 1 } else { NOT_MATCHED };
            push(
                Ranker::Stage1Recall,
                spread(Metrics::of(
                    &lookups.iter().map(in_stage2).collect::<Vec<_>>(),
                )),
            );
        }
    }
    cells
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

pub(super) struct Options {
    pub seeds: u64,
    pub max_test_runs: usize,
    pub folds: bool,
    pub json: Option<PathBuf>,
    pub md: Option<PathBuf>,
    pub label: String,
    pub strict: bool,
    pub shuffle_labels: bool,
}

/// The outcome of one split.
struct SplitResult {
    split: Split,
    /// `Some(reason)` when the split was not measured.
    skipped: Option<String>,
    seeds: usize,
    reach: Reach,
    sampled: usize,
    not_matched: usize,
    after_pick: usize,
    cells: Vec<Cell>,
}

/// The label printed on every result (section 4), from the history itself
/// unless `--label` gives one.
fn corpus_label(given: &str, runs: &[RunRecord]) -> String {
    if !given.is_empty() {
        return given.to_string();
    }
    let all =
        |prefix: &str| !runs.is_empty() && runs.iter().all(|r| r.shell_session.starts_with(prefix));
    if all("synth:") {
        "synthetic".into()
    } else if all("agent:") {
        "agent-run commands, not human".into()
    } else if runs.iter().any(|r| r.source == Source::Import) {
        "human, shell history import".into()
    } else {
        "unlabelled".into()
    }
}

fn git_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Run the relevance benchmark on `data_dir`. Progress goes to `log`, the
/// report to `out`. Returns whether all three verdicts passed.
pub(super) fn run(
    data_dir: &Path,
    options: &Options,
    today: &str,
    out: &mut dyn Write,
    log: &mut dyn Write,
) -> Result<bool> {
    let started = Instant::now();
    let store = CmdStore::open(data_dir)?;
    let input = ReplayInput::read(&store)?;
    let stats = CorpusStats::compute(&store.counts()?, &input.runs, &input.events);
    drop(store);
    let label = corpus_label(&options.label, &input.runs);
    let known: FxHashSet<i64> = input.commands.iter().map(|c| c.id).collect();
    let bench_dir = data_dir.join("bench");

    let mut results = Vec::new();
    for split in splits(options.folds) {
        let mut result = SplitResult {
            split: split.clone(),
            skipped: None,
            seeds: 0,
            reach: Reach::default(),
            sampled: 0,
            not_matched: 0,
            after_pick: 0,
            cells: Vec::new(),
        };
        if input.runs.len() < MIN_RUNS {
            result.skipped = Some(format!("not enough data ({} runs)", input.runs.len()));
            results.push(result);
            continue;
        }
        let cut = time_at(&input.runs, split.cut);
        let end = split.end.map(|f| time_at(&input.runs, f));
        let training = train_at(
            &input,
            cut,
            split.name,
            options.seeds,
            &bench_dir,
            options.shuffle_labels,
            log,
        )?;
        let Some(trained) = training else {
            result.skipped = Some("not enough training groups".into());
            results.push(result);
            continue;
        };
        writeln!(
            log,
            "{}: {} of {} seeds trained ({:.1} s)",
            split.name,
            trained.models.len(),
            options.seeds,
            started.elapsed().as_secs_f64()
        )?;
        let (reach, sampled) = sample_window(&input, &known, cut, end, options.max_test_runs);
        let ranks = replay_window(
            &input,
            trained.replayer,
            &trained.models,
            cut,
            end,
            &sampled,
        );
        writeln!(
            log,
            "{}: {} lookups ranked ({:.1} s)",
            split.name,
            ranks.lookups.iter().map(Vec::len).sum::<usize>(),
            started.elapsed().as_secs_f64()
        )?;
        result.seeds = trained.models.len();
        result.reach = reach;
        result.sampled = sampled.len();
        result.not_matched = ranks.not_matched;
        result.after_pick = ranks.after_pick;
        result.cells = cells_of(&ranks, trained.models.len());
        results.push(result);
    }

    let holdout = &results[0];
    let verdicts = verdicts(&holdout.cells, holdout.seeds);
    let report = markdown(&label, &stats, &results, &verdicts, options);
    out.write_all(report.as_bytes())?;
    if let Some(path) = &options.md {
        std::fs::write(path, &report)
            .with_context(|| format!("could not write {}", path.display()))?;
    }
    if let Some(path) = &options.json {
        let value = to_json(&label, today, &stats, &results, &verdicts);
        std::fs::write(path, serde_json::to_string_pretty(&value)? + "\n")
            .with_context(|| format!("could not write {}", path.display()))?;
    }
    writeln!(
        log,
        "relevance took {:.1} s",
        started.elapsed().as_secs_f64()
    )?;
    Ok(verdicts.all_pass())
}

// ---------------------------------------------------------------------------
// Output (section 5.6)
// ---------------------------------------------------------------------------

fn fmt3(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.3}")
    } else {
        "-".into()
    }
}

fn signed3(value: f64) -> String {
    if value.is_finite() {
        format!("{value:+.3}")
    } else {
        "-".into()
    }
}

/// `1234567` -> `1,234,567`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn verdict_line(name: &str, verdict: &Verdict) -> String {
    format!(
        "{name} {} {} ({}, n={})",
        signed3(verdict.value),
        verdict.status.label(),
        verdict.cell,
        thousands(verdict.n)
    )
}

fn markdown(
    label: &str,
    stats: &CorpusStats,
    results: &[SplitResult],
    verdicts: &Verdicts,
    options: &Options,
) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Relevance: {label}\n");
    if options.shuffle_labels {
        let _ = writeln!(
            md,
            "**Control run: training labels shuffled within each group. Not a result.**\n"
        );
    }
    let _ = writeln!(md, "Corpus: {label}.\n");
    for line in stats.lines() {
        let _ = writeln!(md, "- {line}");
    }
    if stats.pick_episodes == 0 {
        let _ = writeln!(
            md,
            "\nno Ctrl-R picks in this corpus: pick features and CMD_PICK_WEIGHT are untested here"
        );
    }
    for result in results {
        let _ = writeln!(md, "\n## Split {} ({label})\n", result.split.name);
        if let Some(reason) = &result.skipped {
            let _ = writeln!(md, "skipped: {reason}");
            continue;
        }
        let [all, hook, import] = result.reach.shares();
        let _ = writeln!(
            md,
            "Seeds trained: {} of {}. Unreachable share: all {}, hook {}, import {}. \
             Reachable test runs {}, sampled {}, skipped as pick executions {}, \
             lookups whose target did not match its own prefix {}.\n",
            result.seeds,
            options.seeds,
            fmt3(all),
            fmt3(hook),
            fmt3(import),
            thousands(result.reach.reachable),
            thousands(result.sampled),
            result.after_pick,
            result.not_matched
        );
        if result.split.name != "holdout" {
            continue;
        }
        for slice in SLICES {
            write_slice_table(&mut md, label, result, slice, QueryStyle::Prefix);
        }
        let _ = writeln!(
            md,
            "\nFragment queries: the first characters of a word inside the command \
             (section 5.2), information beside the prefix headline."
        );
        for slice in [Slice::All, Slice::Gt10min, Slice::OtherDir] {
            write_slice_table(&mut md, label, result, slice, QueryStyle::Fragment);
        }
        let _ = writeln!(
            md,
            "\nInfix queries: characters from inside a word after the first, not at \
             its start (section 5.2); training never makes such a query."
        );
        for slice in [Slice::All, Slice::Gt10min, Slice::OtherDir] {
            write_slice_table(&mut md, label, result, slice, QueryStyle::Infix);
        }
    }
    let _ = writeln!(md, "\n## Verdicts ({label}, holdout)\n");
    let _ = writeln!(md, "- {}", verdict_line("R1", &verdicts.r1));
    let _ = writeln!(md, "- {}", verdict_line("R2", &verdicts.r2));
    let _ = writeln!(md, "- {}", verdict_line("R3", &verdicts.r3));
    let _ = writeln!(md, "- {}", verdict_line("R4", &verdicts.r4));
    if results.len() > 1 {
        write_fold_table(&mut md, label, results);
    }
    let _ = writeln!(md, "\n{FILE_REFERENCE}");
    md
}

fn write_slice_table(
    md: &mut String,
    label: &str,
    result: &SplitResult,
    slice: Slice,
    style: QueryStyle,
) {
    let cell = |ranker: Ranker, len: usize| {
        result
            .cells
            .iter()
            .find(|c| c.ranker == ranker && c.slice == slice && c.style == style && c.len == len)
            .expect("every cell is computed")
    };
    let lengths = style.lengths();
    let ns: Vec<String> = lengths
        .iter()
        .map(|&len| {
            let n = cell(Ranker::Recency, len).n;
            let low = if n < LOW_N { " low n" } else { "" };
            format!("len {len}: {}{low}", thousands(n))
        })
        .collect();
    let _ = writeln!(
        md,
        "\n### slice {}, {} queries ({label}; n by length: {})\n",
        slice.key(),
        style.key(),
        ns.join(", ")
    );
    let mut header = String::from("| ranker |");
    let mut rule = String::from("|---|");
    for len in lengths {
        for metric in METRICS {
            let _ = write!(header, " L{len} {} |", metric.key());
            rule.push_str("---|");
        }
    }
    let _ = writeln!(md, "{header}\n{rule}");
    for ranker in RANKERS {
        let name = match ranker {
            Ranker::Model => format!("model ({} seeds)", result.seeds),
            Ranker::FzfScored => "fzf_scored (skim approximation, info)".into(),
            Ranker::Fzf => "fzf (--scheme=history, info)".into(),
            Ranker::Heuristic => "heuristic (cold start)".into(),
            _ => ranker.key().into(),
        };
        let mut row = format!("| {name} |");
        for len in lengths {
            for metric in METRICS {
                let _ = write!(row, " {} |", fmt3(cell(ranker, len).mean(metric)));
            }
        }
        let _ = writeln!(md, "{row}");
    }
    let mut row = String::from("| model +- sd |");
    for len in lengths {
        for metric in METRICS {
            let _ = write!(
                row,
                " {} |",
                fmt3(cell(Ranker::Model, len).values[metric as usize].sd)
            );
        }
    }
    let _ = writeln!(md, "{row}");
    for info in INFO_ROWS {
        let mut row = format!("| {} (info) |", info.key());
        for len in lengths {
            for metric in METRICS {
                let _ = write!(row, " {} |", fmt3(cell(info, len).mean(metric)));
            }
        }
        let _ = writeln!(md, "{row}");
    }
}

fn write_fold_table(md: &mut String, label: &str, results: &[SplitResult]) {
    let _ = writeln!(md, "\n## Folds: top-1 at length 2 ({label})\n");
    let _ = writeln!(
        md,
        "| split | slice | n | recency | fzf_scored | fzf | heuristic | model mean | model sd |\n|---|---|---|---|---|---|---|---|---|"
    );
    for result in results {
        if result.skipped.is_some() {
            let _ = writeln!(md, "| {} | - | skipped | | | | | | |", result.split.name);
            continue;
        }
        for slice in [Slice::All, Slice::Gt10min] {
            let cell = |ranker: Ranker| {
                result
                    .cells
                    .iter()
                    .find(|c| {
                        c.ranker == ranker
                            && c.slice == slice
                            && c.style == QueryStyle::Prefix
                            && c.len == 2
                    })
                    .expect("every cell is computed")
            };
            let model = cell(Ranker::Model).values[Metric::Top1 as usize];
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                result.split.name,
                slice.key(),
                thousands(cell(Ranker::Recency).n),
                fmt3(cell(Ranker::Recency).mean(Metric::Top1)),
                fmt3(cell(Ranker::FzfScored).mean(Metric::Top1)),
                fmt3(cell(Ranker::Fzf).mean(Metric::Top1)),
                fmt3(cell(Ranker::Heuristic).mean(Metric::Top1)),
                fmt3(model.mean),
                fmt3(model.sd)
            );
        }
    }
}

fn verdict_json(verdict: &Verdict) -> Value {
    json!({ "value": round6(verdict.value), "cell": verdict.cell, "n": verdict.n,
            "status": verdict.status.label() })
}

fn to_json(
    label: &str,
    today: &str,
    stats: &CorpusStats,
    results: &[SplitResult],
    verdicts: &Verdicts,
) -> Value {
    let [all, hook, import] = results[0].reach.shares();
    let mut splits = serde_json::Map::new();
    for result in results {
        let mut split = json!({ "seeds": result.seeds,
                                "sampled": result.sampled,
                                "reachable": result.reach.reachable,
                                "not_matched": result.not_matched,
                                "after_pick": result.after_pick,
                                "cells": result.cells.iter().map(Cell::to_json).collect::<Vec<_>>() });
        if let Some(reason) = &result.skipped {
            split["skipped"] = json!(reason);
        }
        splits.insert(result.split.name.into(), split);
    }
    json!({
        "label": label,
        "commit": git_commit(),
        "date": today,
        "tuning": super::super::tuning::snapshot(),
        "stats": {
            "commands": stats.commands, "runs": stats.runs, "hook_runs": stats.hook_runs,
            "import_runs": stats.import_runs, "span_days": round6(stats.span_days),
            "runs_last_30_days": stats.runs_last_30_days, "with_cwd": round6(stats.with_cwd),
            "with_exit_code": round6(stats.with_exit_code),
            "synthetic_timestamps": round6(stats.synthetic_timestamps), "sessions": stats.sessions,
            "immediate_repeat_share": round6(stats.immediate_repeat_share),
            "first_ever_share": round6(stats.first_ever_share), "pick_episodes": stats.pick_episodes,
        },
        "unreachable": { "all": round6(all), "hook": round6(hook), "import": round6(import) },
        "splits": splits,
        "verdicts": { "R1": verdict_json(&verdicts.r1), "R2": verdict_json(&verdicts.r2),
                      "R3": verdict_json(&verdicts.r3), "R4": verdict_json(&verdicts.r4) },
    })
}

#[cfg(test)]
mod tests;
