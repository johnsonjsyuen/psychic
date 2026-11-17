//! The command features (`docs/ctrl-r/03-learn.md` section 4).
//!
//! One feature path, two fill paths (ADR-12): serving computes features over
//! the [`CommandIndex`] loaded from SQL, training over the one a replay
//! filled, and both call [`compute_all`]. Every feature is infallible
//! arithmetic over the index's accessors and the already-resolved inputs, so
//! it can run inside a rayon loop.
//!
//! The registry is append-only: a position is a model column. A feature is
//! never removed or reordered, only replaced in place under a new name, which
//! makes every model trained before it fail the name check of `CmdModel::load`.

use super::index::{CommandEntry, CommandIndex, RankContext};
use super::template;
use super::tuning::{CMD_EVENT_WINDOW_SECS, CMD_FAIL_RECENT_SECS, CMD_NEVER_SECS_LOG};
use crate::feature_defs::Monotonicity;
use crate::feature_defs::schema::FeatureType;

/// Everything one feature vector is computed from.
pub(super) struct CommandFeatureInputs<'a> {
    pub query: &'a str,
    /// `query.to_lowercase()`, computed once per ranking call.
    pub query_lower: &'a str,
    pub idx: u32,
    pub entry: &'a CommandEntry,
    /// The candidate's fuzzy score for `query`; 0 for the empty query.
    pub fuzzy_score: i64,
    /// fzf's score for `query` (02 section 7): 0 for the empty query,
    /// `rank::NO_FZF_MATCH` when fzf does not match.
    pub fzf_score: i32,
    /// How many commands matching `query` ran more recently than this one,
    /// at most `CMD_RECENCY_RANK_CAP` (03 section 4).
    pub recency_rank: u32,
    pub index: &'a CommandIndex,
    pub ctx: &'a RankContext,
}

pub(super) trait CommandFeature: Send + Sync {
    fn name(&self) -> &'static str;
    fn feature_type(&self) -> FeatureType;
    fn monotonicity(&self) -> Option<Monotonicity>;
    /// Infallible arithmetic over already-resolved inputs.
    fn compute(&self, inputs: &CommandFeatureInputs) -> f64;
}

const INCREASING: Option<Monotonicity> = Some(Monotonicity::Increasing);
const DECREASING: Option<Monotonicity> = Some(Monotonicity::Decreasing);
const NONE: Option<Monotonicity> = None;

/// Define a unit struct implementing [`CommandFeature`].
macro_rules! command_feature {
    ($type:ident, $name:literal, $kind:ident, $monotonicity:expr, |$inputs:ident| $body:expr) => {
        struct $type;

        impl CommandFeature for $type {
            fn name(&self) -> &'static str {
                $name
            }

            fn feature_type(&self) -> FeatureType {
                FeatureType::$kind
            }

            fn monotonicity(&self) -> Option<Monotonicity> {
                $monotonicity
            }

            fn compute(&self, $inputs: &CommandFeatureInputs) -> f64 {
                $body
            }
        }
    };
}

/// `ln(1 + seconds from at to now)`; an `at` in the future counts as now.
fn log_seconds_since(now: i64, at: i64) -> f64 {
    ((now - at).max(0) as f64).ln_1p()
}

/// `part / runs`, or 0 for a command that never ran.
fn share_of_runs(part: u32, entry: &CommandEntry) -> f64 {
    if entry.run_count == 0 {
        0.0
    } else {
        f64::from(part) / f64::from(entry.run_count)
    }
}

fn flag(on: bool) -> f64 {
    if on { 1.0 } else { 0.0 }
}

fn runs_within(i: &CommandFeatureInputs, secs: i64) -> f64 {
    f64::from(i.index.runs_in_window(i.idx, i.ctx.now, secs))
}

/// `text.to_lowercase().starts_with(query_lower)`, without lower-casing an
/// ASCII text: its lower case is ASCII, and an ASCII byte equals a byte of
/// `query_lower` (lower case already) exactly when they are equal ignoring
/// ASCII case. The model reads this for its rows on every keystroke; the
/// index keeps no lower-case copy of every text.
fn lower_starts_with(text: &str, query_lower: &str) -> bool {
    if text.is_ascii() {
        let (text, query) = (text.as_bytes(), query_lower.as_bytes());
        text.len() >= query.len() && text[..query.len()].eq_ignore_ascii_case(query)
    } else {
        text.to_lowercase().starts_with(query_lower)
    }
}

/// Whether a whitespace-separated word of `text.to_lowercase()` starts with
/// `query_lower` (lower-casing never makes or removes whitespace).
fn a_word_starts_with(text: &str, query_lower: &str) -> bool {
    if text.is_ascii() {
        text.split_whitespace()
            .any(|word| lower_starts_with(word, query_lower))
    } else {
        text.to_lowercase()
            .split_whitespace()
            .any(|word| word.starts_with(query_lower))
    }
}

// --- Query ---------------------------------------------------------------------

command_feature!(FuzzyScore, "fuzzy_score", Numeric, INCREASING, |i| {
    if i.query.is_empty() {
        0.0
    } else {
        i.fuzzy_score as f64
    }
});

command_feature!(
    CommandStartsWithQuery,
    "command_starts_with_query",
    Binary,
    NONE,
    |i| flag(!i.query.is_empty() && lower_starts_with(&i.entry.text, i.query_lower))
);

command_feature!(
    WordStartsWithQuery,
    "word_starts_with_query",
    Binary,
    NONE,
    |i| flag(!i.query.is_empty() && a_word_starts_with(&i.entry.text, i.query_lower))
);

command_feature!(
    PicksForThisQuery,
    "picks_for_this_query",
    Numeric,
    INCREASING,
    |i| f64::from(
        i.index
            .picks_for_query(i.query, i.idx, i.ctx.now, CMD_EVENT_WINDOW_SECS)
    )
);

command_feature!(
    PicksInEpisodeWithQuery,
    "picks_in_episode_with_query",
    Numeric,
    INCREASING,
    |i| f64::from(i.index.episode_picks_for_query(
        i.query,
        i.idx,
        i.ctx.now,
        CMD_EVENT_WINDOW_SECS
    ))
);

// --- Recency and frequency -----------------------------------------------------

command_feature!(
    LogSecondsSinceLastRun,
    "log_seconds_since_last_run",
    Numeric,
    DECREASING,
    |i| if i.entry.run_count == 0 {
        CMD_NEVER_SECS_LOG
    } else {
        log_seconds_since(i.ctx.now, i.entry.last_run_at)
    }
);

command_feature!(RunsLastHour, "runs_last_hour", Numeric, INCREASING, |i| {
    runs_within(i, 3600)
});

command_feature!(RunsLastDay, "runs_last_day", Numeric, INCREASING, |i| {
    runs_within(i, 86_400)
});

command_feature!(
    RunsLast7Days,
    "runs_last_7_days",
    Numeric,
    INCREASING,
    |i| runs_within(i, 604_800)
);

command_feature!(
    RunsLast30Days,
    "runs_last_30_days",
    Numeric,
    INCREASING,
    |i| runs_within(i, 2_592_000)
);

command_feature!(LogTotalRuns, "log_total_runs", Numeric, INCREASING, |i| {
    f64::from(i.entry.run_count).ln_1p()
});

command_feature!(
    PicksLast30Days,
    "picks_last_30_days",
    Numeric,
    INCREASING,
    |i| f64::from(
        i.index
            .picks_in_window(i.idx, i.ctx.now, CMD_EVENT_WINDOW_SECS)
    )
);

// --- Place -----------------------------------------------------------------------

command_feature!(CwdRunShare, "cwd_run_share", Numeric, INCREASING, |i| {
    i.index
        .dir_stat(i.idx, i.ctx.dir)
        .map_or(0.0, |d| share_of_runs(d.runs, i.entry))
});

command_feature!(RepoRunShare, "repo_run_share", Numeric, INCREASING, |i| {
    if i.ctx.repo.is_none() {
        0.0
    } else {
        share_of_runs(i.index.runs_in_repo(i.idx, i.ctx.repo), i.entry)
    }
});

command_feature!(
    LogSecondsSinceLastRunInCwd,
    "log_seconds_since_last_run_in_cwd",
    Numeric,
    DECREASING,
    |i| i
        .index
        .dir_stat(i.idx, i.ctx.dir)
        .map_or(CMD_NEVER_SECS_LOG, |d| log_seconds_since(
            i.ctx.now,
            d.last_run_at
        ))
);

command_feature!(RunsInCwd, "runs_in_cwd", Numeric, INCREASING, |i| {
    i.index
        .dir_stat(i.idx, i.ctx.dir)
        .map_or(0.0, |d| f64::from(d.runs))
});

// --- Sequence --------------------------------------------------------------------

command_feature!(
    FollowsPrevShare,
    "follows_prev_share",
    Numeric,
    INCREASING,
    |i| share_of_runs(
        i.index.follower_runs(i.idx, i.ctx.prev_templates[0]),
        i.entry
    )
);

command_feature!(
    FollowsRecent3Share,
    "follows_recent3_share",
    Numeric,
    INCREASING,
    |i| {
        let prev = &i.ctx.prev_templates;
        let runs: u32 = prev
            .iter()
            .enumerate()
            .filter(|&(at, template)| template.is_some() && !prev[..at].contains(template))
            .map(|(_, &template)| i.index.follower_runs(i.idx, template))
            .sum();
        share_of_runs(runs, i.entry)
    }
);

// --- Outcome ---------------------------------------------------------------------

command_feature!(SuccessRate, "success_rate", Numeric, NONE, |i| {
    let known = i.entry.success_count + i.entry.fail_count;
    if known == 0 {
        f64::NAN // missing, to LightGBM; 0.0 would read as "always fails"
    } else {
        f64::from(i.entry.success_count) / f64::from(known)
    }
});

command_feature!(
    FailedWithin2Minutes,
    "failed_within_2_minutes",
    Binary,
    NONE,
    |i| {
        let at = i.entry.last_fail_at;
        flag(at > 0 && at <= i.ctx.now && i.ctx.now - at < CMD_FAIL_RECENT_SECS)
    }
);

command_feature!(LastExitNonzero, "last_exit_nonzero", Binary, NONE, |i| {
    flag(matches!(i.entry.last_exit_code, Some(code) if code != 0))
});

// --- Shape -----------------------------------------------------------------------

command_feature!(CommandLength, "command_length", Numeric, NONE, |i| {
    i.entry.text.chars().count() as f64
});

command_feature!(ArgumentCount, "argument_count", Numeric, NONE, |i| {
    template::word_count(&i.entry.text).saturating_sub(1) as f64
});

// --- Added after the first release (append-only) --------------------------------

command_feature!(FzfScore, "fzf_score", Numeric, INCREASING, |i| {
    f64::from(i.fzf_score)
});

command_feature!(RecencyRank, "recency_rank", Numeric, NONE, |i| {
    f64::from(i.recency_rank)
});

/// Append-only. The position is the column in the model. A `static` slice,
/// not a `Lazy`: it is read inside a rayon loop.
pub(super) static COMMAND_FEATURE_REGISTRY: &[&dyn CommandFeature] = &[
    &FuzzyScore,
    &CommandStartsWithQuery,
    &WordStartsWithQuery,
    &PicksForThisQuery,
    &PicksInEpisodeWithQuery,
    &LogSecondsSinceLastRun,
    &RunsLastHour,
    &RunsLastDay,
    &RunsLast7Days,
    &RunsLast30Days,
    &LogTotalRuns,
    &PicksLast30Days,
    &CwdRunShare,
    &RepoRunShare,
    &LogSecondsSinceLastRunInCwd,
    &RunsInCwd,
    &FollowsPrevShare,
    &FollowsRecent3Share,
    &SuccessRate,
    &FailedWithin2Minutes,
    &LastExitNonzero,
    &CommandLength,
    &ArgumentCount,
    &FzfScore,
    &RecencyRank,
];

/// The one feature allowed to be NaN (missing).
const MAY_BE_MISSING: &str = "success_rate";

/// The names, in model order.
pub(super) fn command_feature_names() -> Vec<&'static str> {
    COMMAND_FEATURE_REGISTRY.iter().map(|f| f.name()).collect()
}

/// Names and monotone constraints, for training.
pub(super) fn command_feature_specs() -> Vec<crate::training::FeatureSpec<'static>> {
    COMMAND_FEATURE_REGISTRY
        .iter()
        .map(|f| crate::training::FeatureSpec {
            name: f.name(),
            monotone: match f.monotonicity() {
                None => 0,
                Some(Monotonicity::Increasing) => 1,
                Some(Monotonicity::Decreasing) => -1,
            },
        })
        .collect()
}

/// Write every feature of one candidate into `out`, which has exactly one
/// slot per registry entry.
///
/// Panics (E-L-08) when a feature other than `success_rate` is not finite:
/// that is a bug in the feature, and the message names the feature, never
/// the command.
pub(super) fn compute_all(inputs: &CommandFeatureInputs, out: &mut [f64]) {
    assert_eq!(
        out.len(),
        COMMAND_FEATURE_REGISTRY.len(),
        "one slot per command feature"
    );
    for (slot, feature) in out.iter_mut().zip(COMMAND_FEATURE_REGISTRY) {
        let value = feature.compute(inputs);
        check_value(feature.name(), value);
        *slot = value;
    }
}

fn check_value(name: &str, value: f64) {
    assert!(
        value.is_finite() || (name == MAY_BE_MISSING && value.is_nan()),
        "command feature {name} computed {value}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::index::test_support::{context, index_of};

    const NOW: i64 = 1_000_000;

    #[test]
    fn query_flags_equal_lower_casing_the_whole_text() {
        use rand::{Rng, SeedableRng};
        const PIECES: &[&str] = &[
            "Git", "gi", "a", "A", " ", "\t", "\u{0B}", "É", "é", "İ", "K", "k", "-x",
        ];
        let mut rng = rand::rngs::StdRng::seed_from_u64(9);
        let mut word_hits = 0;
        for _ in 0..50_000 {
            let mut pick = |max: usize| -> String {
                (0..rng.gen_range(0..=max))
                    .map(|_| PIECES[rng.gen_range(0..PIECES.len())])
                    .collect()
            };
            let text = pick(6);
            let query_lower = pick(2).to_lowercase();
            let lower = text.to_lowercase();
            assert_eq!(
                lower_starts_with(&text, &query_lower),
                lower.starts_with(&query_lower),
                "{text:?} {query_lower:?}"
            );
            let words = lower
                .split_whitespace()
                .any(|w| w.starts_with(&query_lower));
            assert_eq!(
                a_word_starts_with(&text, &query_lower),
                words,
                "{text:?} {query_lower:?}"
            );
            word_hits += usize::from(words);
        }
        assert!(word_hits > 5_000, "{word_hits}");
    }

    fn features(
        index: &CommandIndex,
        ctx: &RankContext,
        idx: u32,
        query: &str,
        fuzzy_score: i64,
    ) -> Vec<f64> {
        let query_lower = query.to_lowercase();
        let entry = &index.entries[idx as usize];
        let inputs = CommandFeatureInputs {
            query,
            query_lower: &query_lower,
            idx,
            entry,
            fuzzy_score,
            fzf_score: crate::cmd::rank::fzf_score(
                &crate::cmd::fzf::FzfQuery::new(query),
                &entry.text,
            ),
            recency_rank: 3,
            index,
            ctx,
        };
        let mut out = vec![0.0; COMMAND_FEATURE_REGISTRY.len()];
        compute_all(&inputs, &mut out);
        out
    }

    /// Fixture F1 of section 10.1.
    fn f1() -> (CommandIndex, RankContext) {
        let mut index = index_of(&["git status"]);
        let (a, b) = (index.dirs.intern("/a"), index.dirs.intern("/b"));
        let cd = index.templates.intern("cd PATH");
        index.record_run(0, NOW - 10, Some(a), None, Some(0), Some(cd));
        index.record_run(0, NOW - 4000, Some(a), None, Some(1), None);
        index.record_run(0, NOW - 90_000, Some(b), None, Some(0), Some(cd));
        index.record_run(0, NOW - 700_000, Some(b), None, None, None);
        let mut ctx = context(NOW);
        ctx.dir = Some(a);
        ctx.prev_templates = [Some(cd), None, None];
        (index, ctx)
    }

    fn assert_close(actual: &[f64], expected: &[f64]) {
        assert_eq!(actual.len(), expected.len());
        for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (a.is_nan() && e.is_nan()) || (a - e).abs() <= 1e-4,
                "feature {} ({}): {a} != {e}",
                i + 1,
                COMMAND_FEATURE_REGISTRY[i].name()
            );
        }
    }

    #[test]
    fn tc_l_01_every_feature_on_f1() {
        let (index, ctx) = f1();
        let expected = [
            57.0, 0.0, 0.0, 0.0, 0.0, 2.3979, 1.0, 2.0, 3.0, 4.0, 1.6094, 0.0, 0.5, 0.0, 2.3979,
            2.0, 0.5, 0.5, 0.6667, 0.0, 0.0, 10.0, 1.0,
            // fzf_score: "g" at the start (16 + 2 * 8), a 3-character gap
            // (-3 - 1 - 1), "s" at a word start (16 + 8).
            51.0, // recency_rank, as the fixture passes it.
            3.0,
        ];
        assert_close(&features(&index, &ctx, 0, "gs", 57), &expected);
    }

    #[test]
    fn tc_l_02_query_features() {
        let (index, ctx) = f1();
        let first_three = |query: &str| features(&index, &ctx, 0, query, 9)[..3].to_vec();
        assert_eq!(first_three(""), [0.0, 0.0, 0.0]);
        assert_eq!(first_three("git"), [9.0, 1.0, 1.0]);
        assert_eq!(first_three("st"), [9.0, 0.0, 1.0]);
        assert_eq!(first_three("GIT"), [9.0, 1.0, 1.0]);
    }

    #[test]
    fn tc_l_03_unknowns() {
        let mut index = index_of(&["make"]);
        index.record_run(0, NOW - 50, None, None, None, None);
        let ctx = context(NOW);
        let values = features(&index, &ctx, 0, "", 0);
        assert!(values[18].is_nan(), "success_rate is missing");
        assert_close(&values[12..18], &[0.0, 0.0, 19.57, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn tc_l_04_failed_within_2_minutes() {
        let failed = |last_fail_at: i64| {
            let mut index = index_of(&["make"]);
            index.record_run(0, NOW - 1000, None, None, Some(0), None);
            index.entries[0].last_fail_at = last_fail_at;
            features(&index, &context(NOW), 0, "", 0)[19]
        };
        assert_eq!(failed(NOW - 119), 1.0);
        assert_eq!(failed(NOW - 120), 0.0);
        assert_eq!(failed(0), 0.0);
        assert_eq!(failed(NOW + 5), 0.0, "clock skew");
    }

    #[test]
    fn tc_l_05_follows_recent3_share_counts_a_template_once() {
        let mut index = index_of(&["make"]);
        let (x, y) = (index.templates.intern("x"), index.templates.intern("y"));
        for prev in [Some(x), Some(x), Some(y), None, None, None] {
            index.record_run(0, NOW - 100, None, None, Some(0), prev);
        }
        let mut ctx = context(NOW);
        ctx.prev_templates = [Some(x), Some(y), Some(x)];
        assert_eq!(features(&index, &ctx, 0, "", 0)[17], 0.5);
    }

    #[test]
    fn tc_l_06_registry_shape() {
        let names = command_feature_names();
        assert_eq!(
            names,
            [
                "fuzzy_score",
                "command_starts_with_query",
                "word_starts_with_query",
                "picks_for_this_query",
                "picks_in_episode_with_query",
                "log_seconds_since_last_run",
                "runs_last_hour",
                "runs_last_day",
                "runs_last_7_days",
                "runs_last_30_days",
                "log_total_runs",
                "picks_last_30_days",
                "cwd_run_share",
                "repo_run_share",
                "log_seconds_since_last_run_in_cwd",
                "runs_in_cwd",
                "follows_prev_share",
                "follows_recent3_share",
                "success_rate",
                "failed_within_2_minutes",
                "last_exit_nonzero",
                "command_length",
                "argument_count",
                "fzf_score",
                "recency_rank",
            ]
        );
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "names are unique");
        let monotone: Vec<i8> = command_feature_specs().iter().map(|f| f.monotone).collect();
        assert_eq!(
            monotone,
            [
                1, 0, 0, 1, 1, -1, 1, 1, 1, 1, 1, 1, 1, 1, -1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 0
            ]
        );
        let binary: Vec<&str> = COMMAND_FEATURE_REGISTRY
            .iter()
            .filter(|f| f.feature_type() == FeatureType::Binary)
            .map(|f| f.name())
            .collect();
        assert_eq!(
            binary,
            [
                "command_starts_with_query",
                "word_starts_with_query",
                "failed_within_2_minutes",
                "last_exit_nonzero"
            ]
        );
    }

    #[test]
    fn picks_feed_the_three_pick_features() {
        let mut index = index_of(&["git status"]);
        index.record_run(0, NOW - 100, None, None, Some(0), None);
        index.record_pick(0, NOW - 50, "gs", &["g".to_string(), "gs".to_string()]);
        let values = features(&index, &context(NOW), 0, "gs", 1);
        assert_eq!([values[3], values[4], values[11]], [1.0, 1.0, 1.0]);
        let values = features(&index, &context(NOW), 0, "g", 1);
        assert_eq!([values[3], values[4], values[11]], [0.0, 1.0, 1.0]);
    }

    #[test]
    fn repo_share_counts_runs_in_the_context_repo() {
        let mut index = index_of(&["make"]);
        let repo = index.repos.intern("/r");
        index.record_run(0, NOW - 100, None, Some(repo), Some(0), None);
        index.record_run(0, NOW - 90, None, None, Some(0), None);
        let mut ctx = context(NOW);
        ctx.repo = Some(repo);
        assert_eq!(features(&index, &ctx, 0, "", 0)[13], 0.5);
    }

    #[test]
    fn a_command_that_never_ran_is_never_on_both_time_axes() {
        let index = index_of(&["make"]);
        let values = features(&index, &context(NOW), 0, "", 0);
        assert_eq!(values[5], CMD_NEVER_SECS_LOG);
        assert_eq!(values[10], 0.0);
        assert_eq!(values[16], 0.0);
    }

    #[test]
    fn e_l_13_runs_in_the_future_count_as_now_and_not_in_windows() {
        let mut index = index_of(&["make"]);
        let dir = index.dirs.intern("/w");
        index.record_run(0, NOW + 30, Some(dir), None, Some(0), None);
        let mut ctx = context(NOW);
        ctx.dir = Some(dir);
        let values = features(&index, &ctx, 0, "", 0);
        assert_eq!(values[5], 0.0, "log_seconds_since_last_run");
        assert_eq!(values[14], 0.0, "log_seconds_since_last_run_in_cwd");
        assert_eq!(values[6..10], [0.0, 0.0, 0.0, 0.0], "windows end at now");
    }

    #[test]
    #[should_panic(expected = "command feature runs_in_cwd computed inf")]
    fn e_l_08_a_non_finite_value_is_a_bug_that_names_the_feature() {
        check_value("runs_in_cwd", f64::INFINITY);
    }

    #[test]
    #[should_panic(expected = "command feature success_rate computed inf")]
    fn e_l_08_success_rate_may_be_missing_but_not_infinite() {
        check_value("success_rate", f64::NAN);
        check_value("success_rate", f64::INFINITY);
    }
}
