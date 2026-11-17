//! Training the command model (`docs/ctrl-r/03-learn.md` sections 5, 6, 8).
//!
//! The training set is a time-ordered replay of everything recorded: one
//! [`Replayer`] whose [`CommandIndex`] is filled through the same mutators
//! as nothing but the past, so a row's features reflect only what had
//! happened before it (ADR-12). Two kinds of groups come out of it:
//!
//! - every executed command that had run before is a group with a synthetic
//!   query (its first 0-3 characters), the command as the positive and up to
//!   30 other matching commands as negatives (section 5.2);
//! - every Ctrl-R pick is a group of the impressions of its picker process,
//!   the picked command as the positive (section 5.3).
//!
//! The fit is the file model's (`crate::training::fit_capped` through
//! `fit_command_model`, at most `CMD_MODEL_MAX_TREES` trees); the files it
//! writes are `cmd_model.txt` and `cmd_model_stats.json`. Training runs in a
//! detached child process the picker starts at most once an hour (ADR-10),
//! or in the foreground with `psychic history --retrain`.
//!
//! Nothing here logs or prints a query or a command: counts and feature
//! names only.

use super::corpus::splitmix64;
use super::features::{
    COMMAND_FEATURE_REGISTRY, CommandFeatureInputs, command_feature_names, command_feature_specs,
    compute_all,
};
use super::fuzzy;
use super::fzf::FzfQuery;
use super::index::{CommandIndex, RankContext};
use super::rank::{fzf_score, recency_rank};
use super::store::{CmdStore, CommandRecord, EventRecord, RunRecord, Source};
use super::tuning::{
    CMD_HOOK_WEIGHT, CMD_IMPORT_WEIGHT, CMD_MODEL_MAX_TREES, CMD_PICK_RUN_SECS, CMD_PICK_WEIGHT,
    CMD_RECENCY_RANK_CAP, CMD_RETRAIN_MIN_INTERVAL_SECS, CMD_STAGE2_K, CMD_TRAIN_FRAGMENT_SHARE,
    CMD_TRAIN_LOCK_STALE_SECS, CMD_TRAIN_MAX_RUN_GROUPS, CMD_TRAIN_MIN_GROUPS, CMD_TRAIN_QLEN_MIX,
    CMD_TRAIN_RANDOM_NEGATIVES, CMD_TRAIN_RANDOM_TRIES, CMD_TRAIN_RECENT_NEGATIVES,
    CMD_TRAIN_RECENT_SCAN,
};
use anyhow::{Context, Result};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Written at every training attempt; its mtime is the retrain back-off clock.
const ATTEMPT_FILE: &str = "cmd_train_attempt";
/// Held while a training runs.
const LOCK_FILE: &str = "cmd_train.lock";
/// How often a held lock's mtime is refreshed; well under
/// `CMD_TRAIN_LOCK_STALE_SECS`.
const LOCK_HEARTBEAT_SECS: u64 = 60;
const MODEL_FILE: &str = "cmd_model.txt";
const STATS_FILE: &str = "cmd_model_stats.json";
const TRAINING_LOG: &str = "training.log";
/// Set by benchmarks and tests: the picker starts no training.
const NO_RETRAIN_ENV: &str = "PSYCHIC_NO_RETRAIN";
/// What `crate::training` says when the time split leaves a part empty.
const NOT_ENOUGH_FOR_A_SPLIT: &str = "every split needs rows";
/// Mixed into the run id for the random-negative stream.
const RANDOM_NEGATIVE_SALT: u64 = 0x9E37_79B9_7F4A_7C15;
/// Mixed into a run id to choose whether its query is a fragment, and which.
const FRAGMENT_SALT: u64 = 0xF4A6_0000_0000_F4A6;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Everything recorded, in the orders of T2, T3 (`started_at, id`) and T4 (`id`).
pub(super) struct ReplayInput {
    pub commands: Vec<CommandRecord>,
    pub runs: Vec<RunRecord>,
    pub events: Vec<EventRecord>,
}

impl ReplayInput {
    pub(super) fn read(store: &CmdStore) -> Result<Self> {
        Ok(ReplayInput {
            commands: store.all_commands()?,
            runs: store.all_runs()?,
            events: store.all_events()?,
        })
    }
}

/// One step of the timeline: runs and events merged by time.
#[derive(Clone, Copy)]
pub(super) enum Step<'a> {
    Run(&'a RunRecord),
    Event(&'a EventRecord),
}

/// Runs (by `started_at`) and events (by `timestamp`) merged, events first at
/// equal times, ending before the first step at or after `until`.
pub(super) struct Timeline<'a> {
    runs: Peekable<std::slice::Iter<'a, RunRecord>>,
    events: Peekable<std::slice::Iter<'a, EventRecord>>,
    /// Steps before this time are skipped.
    from: Option<i64>,
    until: Option<i64>,
}

impl<'a> Timeline<'a> {
    pub(super) fn new(input: &'a ReplayInput, until: Option<i64>) -> Self {
        Self::window(input, None, until)
    }

    /// The steps at or after `from` and before `until`: what happens after a
    /// replay stopped at `from` (the benchmark's test window).
    pub(super) fn window(input: &'a ReplayInput, from: Option<i64>, until: Option<i64>) -> Self {
        Timeline {
            runs: input.runs.iter().peekable(),
            events: input.events.iter().peekable(),
            from,
            until,
        }
    }
}

impl<'a> Iterator for Timeline<'a> {
    type Item = Step<'a>;

    fn next(&mut self) -> Option<Step<'a>> {
        loop {
            let event_first = match (self.events.peek(), self.runs.peek()) {
                (Some(event), Some(run)) => event.timestamp <= run.started_at,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => return None,
            };
            let (step, at) = if event_first {
                let event = self.events.peek().expect("peeked");
                (Step::Event(event), event.timestamp)
            } else {
                let run = self.runs.peek().expect("peeked");
                (Step::Run(run), run.started_at)
            };
            if self.until.is_some_and(|until| at >= until) {
                return None;
            }
            if event_first {
                self.events.next();
            } else {
                self.runs.next();
            }
            if self.from.is_none_or(|from| at >= from) {
                return Some(step);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The replayer
// ---------------------------------------------------------------------------

/// The last Ctrl-R pick of each command in each shell session: a run right
/// after it is that pick being executed (section 5.2, "Eligible").
#[derive(Default)]
struct PickMemory {
    /// shell session -> command id -> pick time.
    last: FxHashMap<Box<str>, FxHashMap<i64, i64>>,
}

impl PickMemory {
    fn remember(&mut self, pick: &EventRecord) {
        if let Some(session) = pick.shell_session.as_deref() {
            let times = match self.last.get_mut(session) {
                Some(times) => times,
                None => self.last.entry(session.into()).or_default(),
            };
            times.insert(pick.command_id, pick.timestamp);
        }
    }

    fn follows_pick(&self, run: &RunRecord) -> bool {
        self.last
            .get(run.shell_session.as_str())
            .and_then(|times| times.get(&run.command_id))
            .is_some_and(|&at| (0..=CMD_PICK_RUN_SECS).contains(&(run.started_at - at)))
    }
}

/// A Ctrl-R pick and the impressions its picker process logged before it.
pub(super) struct PickEpisode {
    pub pick: EventRecord,
    pub impressions: Vec<EventRecord>,
}

/// The state of the world at one moment of the timeline: the index as
/// serving would have loaded it then, plus what serving gets from SQL
/// (session tails) and what group building needs (recency order).
pub(super) struct Replayer {
    pub index: CommandIndex,
    /// Texts of commands that have not run yet; moved into the index at
    /// their first run.
    unrun_texts: FxHashMap<i64, Box<str>>,
    /// Template id (in `index.templates`) of every command.
    template_of: FxHashMap<i64, u32>,
    /// shell session -> the template ids of its last 3 runs, newest first.
    session_tails: FxHashMap<Box<str>, [Option<u32>; 3]>,
    /// `(idx, seq)` per run, oldest first; an entry is current while its
    /// seq is the idx's `last_seq`.
    recency: Vec<(u32, u64)>,
    /// Per idx: the seq of its newest run.
    last_seq: Vec<u64>,
    /// Per idx: the dir id of its newest run with a known cwd.
    last_cwd: Vec<Option<u32>>,
    seq: u64,
    /// Picker session id -> impressions not yet followed by a pick.
    pending: FxHashMap<Box<str>, Vec<EventRecord>>,
    picks: PickMemory,
}

impl Replayer {
    /// An empty index; commands enter it at their first run.
    pub fn new(commands: &[CommandRecord]) -> Self {
        let mut index = CommandIndex::default();
        let template_of = commands
            .iter()
            .map(|c| (c.id, index.templates.intern(&c.template)))
            .collect();
        Replayer {
            index,
            unrun_texts: commands
                .iter()
                .map(|c| (c.id, c.text.as_str().into()))
                .collect(),
            template_of,
            session_tails: FxHashMap::default(),
            recency: Vec::new(),
            last_seq: Vec::new(),
            last_cwd: Vec::new(),
            seq: 0,
            pending: FxHashMap::default(),
            picks: PickMemory::default(),
        }
    }

    /// The idx of `command_id` if it has run before.
    pub fn knows(&self, command_id: i64) -> Option<u32> {
        self.index.idx_of(command_id)
    }

    /// What ranking at `now` in this place would see. Nothing is interned: a
    /// directory never seen gives `None`, which yields the same feature
    /// values as an id with no statistics.
    pub fn context(
        &self,
        now: i64,
        cwd: Option<&str>,
        repo_root: Option<&str>,
        shell_session: Option<&str>,
    ) -> RankContext {
        RankContext {
            now,
            dir: cwd
                .filter(|cwd| !cwd.is_empty())
                .and_then(|cwd| self.index.dirs.get(cwd)),
            repo: repo_root.and_then(|root| self.index.repos.get(root)),
            prev_templates: shell_session
                .and_then(|session| self.session_tails.get(session))
                .copied()
                .unwrap_or([None; 3]),
        }
    }

    /// Commands, most recently run first, each once.
    pub fn recent_distinct(&self) -> impl Iterator<Item = u32> + '_ {
        self.recency
            .iter()
            .rev()
            .filter(|&&(idx, seq)| self.last_seq[idx as usize] == seq)
            .map(|&(idx, _)| idx)
    }

    /// Whether `run` executes a pick already applied: same command and shell
    /// session, at most `CMD_PICK_RUN_SECS` after it.
    pub fn follows_pick(&self, run: &RunRecord) -> bool {
        self.picks.follows_pick(run)
    }

    /// The dir id of the command's most recent run with a known cwd.
    pub fn last_cwd(&self, idx: u32) -> Option<u32> {
        self.last_cwd[idx as usize]
    }

    /// Apply one run. A run of a command missing from the command list (one
    /// recorded between the reads) is skipped.
    pub fn apply_run(&mut self, run: &RunRecord) {
        let idx = match self.index.idx_of(run.command_id) {
            Some(idx) => idx,
            None => {
                let Some(text) = self.unrun_texts.remove(&run.command_id) else {
                    return;
                };
                self.last_seq.push(0);
                self.last_cwd.push(None);
                self.index.add_command(run.command_id, &text)
            }
        };
        let dir = run
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.is_empty())
            .map(|cwd| self.index.dirs.intern(cwd));
        let repo = run
            .repo_root
            .as_deref()
            .map(|root| self.index.repos.intern(root));
        let prev_template = run
            .prev_command_id
            .and_then(|prev| self.template_of.get(&prev).copied());
        self.index
            .record_run(idx, run.started_at, dir, repo, run.exit_code, prev_template);
        if dir.is_some() {
            self.last_cwd[idx as usize] = dir;
        }

        let template = self.template_of.get(&run.command_id).copied();
        let tail = match self.session_tails.get_mut(run.shell_session.as_str()) {
            Some(tail) => tail,
            None => self
                .session_tails
                .entry(run.shell_session.as_str().into())
                .or_insert([None; 3]),
        };
        *tail = [template, tail[0], tail[1]];

        self.seq += 1;
        self.recency.push((idx, self.seq));
        self.last_seq[idx as usize] = self.seq;
        if self.recency.len() > 2 * self.index.entries.len() + 1024 {
            let last_seq = &self.last_seq;
            self.recency
                .retain(|&(idx, seq)| last_seq[idx as usize] == seq);
        }
    }

    /// Apply one event. Never changes the index: a pick comes back as a
    /// [`PickEpisode`] so the caller can build its group from the state
    /// before it, then [`Replayer::commit_pick`]s it. Events of a command
    /// that has never run are dropped (E-L-11).
    pub fn apply_event(&mut self, event: &EventRecord) -> Option<PickEpisode> {
        let known = self.knows(event.command_id).is_some();
        if !event.is_pick {
            if known {
                match self.pending.get_mut(event.session_id.as_str()) {
                    Some(pending) => pending.push(event.clone()),
                    None => {
                        self.pending
                            .insert(event.session_id.as_str().into(), vec![event.clone()]);
                    }
                }
            }
            return None;
        }
        // The picker process ends at its pick: its impressions go with it.
        let impressions = self
            .pending
            .remove(event.session_id.as_str())
            .unwrap_or_default();
        known.then(|| PickEpisode {
            pick: event.clone(),
            impressions,
        })
    }

    /// Apply one step of the timeline: a run, or an event (committing the
    /// pick it ends, if any).
    pub fn apply_step(&mut self, step: Step<'_>) {
        match step {
            Step::Run(run) => self.apply_run(run),
            Step::Event(event) => {
                if let Some(episode) = self.apply_event(event) {
                    self.commit_pick(&episode);
                }
            }
        }
    }

    /// Record the episode's pick in the index.
    pub fn commit_pick(&mut self, episode: &PickEpisode) {
        let pick = &episode.pick;
        let idx = self
            .knows(pick.command_id)
            .expect("an episode is only returned for a known command");
        self.index
            .record_pick(idx, pick.timestamp, &pick.query, &pick.episode_queries);
        self.picks.remember(pick);
    }
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

/// One training row.
struct GroupRow {
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "tests check which commands a group holds")
    )]
    idx: u32,
    label: f32,
    features: Vec<f64>,
}

/// The rows of one decision, ranked against each other.
struct Group {
    rows: Vec<GroupRow>,
    weight: f64,
    timestamp: i64,
}

/// The `CMD_RECENCY_RANK_CAP` newest commands matching `query` as
/// `(last_run_at, db_id)`, newest first: what serving's
/// `CommandRanking::newest` holds for the same state and query.
pub(super) fn newest_matching(replayer: &Replayer, query: &str) -> Vec<(i64, i64)> {
    let matcher = fuzzy::Query::new(query);
    let index = &replayer.index;
    let mut found: Vec<(i64, i64)> = Vec::new();
    for idx in replayer.recent_distinct() {
        let entry = &index.entries[idx as usize];
        // Runs replay in time order, so this walks last runs newest first;
        // past the cap only an equal last run can still belong.
        if found.len() >= CMD_RECENCY_RANK_CAP
            && found.last().is_some_and(|&(at, _)| entry.last_run_at < at)
        {
            break;
        }
        if matcher.matches(index, idx).is_some() {
            found.push((entry.last_run_at, entry.db_id));
        }
    }
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.truncate(CMD_RECENCY_RANK_CAP);
    found
}

/// The features of `idx` for `query` in `ctx`; `newest` is
/// [`newest_matching`] for `query`, `fzf` fzf's score (`NO_FZF_MATCH` when
/// it does not match). Skim's score is computed here, as serving computes it
/// for the rows its model scores.
fn feature_row(
    index: &CommandIndex,
    ctx: &RankContext,
    query: &str,
    newest: &[(i64, i64)],
    idx: u32,
    fzf: i32,
) -> Vec<f64> {
    let query_lower = query.to_lowercase();
    let entry = &index.entries[idx as usize];
    let inputs = CommandFeatureInputs {
        query,
        query_lower: &query_lower,
        idx,
        entry,
        fuzzy_score: fuzzy::skim_score(fuzzy::skim(), query, &entry.text),
        fzf_score: fzf,
        recency_rank: recency_rank(newest, (entry.last_run_at, entry.db_id)),
        index,
        ctx,
    };
    let mut row = vec![0.0; COMMAND_FEATURE_REGISTRY.len()];
    compute_all(&inputs, &mut row);
    row
}

/// The synthetic query length of a run: 0-3 characters by the cumulative
/// `CMD_TRAIN_QLEN_MIX`, drawn by a seeded hash of the run id (ADR-13).
fn query_length(seed: u64, run_id: i64) -> usize {
    let r = (splitmix64(seed ^ run_id as u64) % 100) as u32;
    let mut bound = 0;
    for (length, share) in CMD_TRAIN_QLEN_MIX.iter().enumerate() {
        bound += share;
        if r < bound {
            return length;
        }
    }
    CMD_TRAIN_QLEN_MIX.len() - 1
}

/// The first `length` characters of `text`, or all of it if shorter.
fn synthetic_query(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}

/// A fragment query (section 5.2; the relevance benchmark's second style,
/// 04 section 5.2): the first `len` characters of one word after the first,
/// counted from the word's first letter or digit. `pick` chooses the word,
/// the same one at every length. `None` when the text has no such word or
/// the chosen one is shorter than `len`.
pub(super) fn fragment_query(text: &str, len: usize, pick: u64) -> Option<String> {
    let words: Vec<&str> = text
        .split_whitespace()
        .skip(1)
        .filter_map(|word| {
            word.find(|c: char| c.is_alphanumeric())
                .map(|at| &word[at..])
        })
        .collect();
    if words.is_empty() {
        return None;
    }
    let word = words[(pick % words.len() as u64) as usize];
    (word.chars().count() >= len).then(|| word.chars().take(len).collect())
}

/// The synthetic query of a run's group (section 5.2): its length by
/// [`query_length`]; then, for a non-empty one, a fragment of a later word
/// for `CMD_TRAIN_FRAGMENT_SHARE` percent of runs (by a seeded hash of the
/// run id) when the text has one long enough, else a prefix.
fn training_query(seed: u64, run_id: i64, text: &str) -> String {
    let length = query_length(seed, run_id);
    let draw = splitmix64(seed ^ run_id as u64 ^ FRAGMENT_SALT);
    let fragment = length > 0 && ((draw % 100) as u32) < CMD_TRAIN_FRAGMENT_SHARE;
    fragment
        .then(|| fragment_query(text, length, draw >> 8))
        .flatten()
        .unwrap_or_else(|| synthetic_query(text, length))
}

fn source_weight(source: Source) -> f64 {
    match source {
        Source::Hook => CMD_HOOK_WEIGHT,
        Source::Import => CMD_IMPORT_WEIGHT,
    }
}

/// The group of one executed command for `query` (section 5.2, from
/// "Context" down): the positive and up to `CMD_TRAIN_RECENT_NEGATIVES` (0)
/// recent plus `CMD_TRAIN_RANDOM_NEGATIVES` (30) random other
/// commands that match, ordered by idx. `None` when the command never ran
/// before, does not match its own query, or no negative is found. Does not
/// touch the replayer.
fn build_run_group(replayer: &Replayer, run: &RunRecord, query: &str, seed: u64) -> Option<Group> {
    let positive = replayer.knows(run.command_id)?;
    let index = &replayer.index;
    let ctx = replayer.context(
        run.started_at,
        run.cwd.as_deref(),
        run.repo_root.as_deref(),
        Some(&run.shell_session),
    );
    // The picker's match: fzf's, through the same prefilter.
    let matcher = fuzzy::Query::new(query);
    let fuzzy = |idx: u32| matcher.matches(index, idx);
    let usable = |idx: u32| idx != positive && !index.entries[idx as usize].deleted;
    let positive_score = fuzzy(positive)?;

    // (idx, fzf score) of each negative.
    let mut negatives: Vec<(u32, i32)> = Vec::new();
    for idx in replayer
        .recent_distinct()
        .filter(|&idx| usable(idx))
        .take(CMD_TRAIN_RECENT_SCAN)
    {
        if negatives.len() == CMD_TRAIN_RECENT_NEGATIVES {
            break;
        }
        if let Some(score) = fuzzy(idx) {
            negatives.push((idx, score));
        }
    }

    // An empty query: serving's model sees only the CMD_STAGE2_K newest
    // commands (stage 1 is newest first), so draw from those.
    let pool: Option<Vec<u32>> = query
        .is_empty()
        .then(|| replayer.recent_distinct().take(CMD_STAGE2_K).collect());
    let commands = pool.as_ref().map_or(index.entries.len(), Vec::len) as u64;
    let mut state = splitmix64(seed ^ run.id as u64 ^ RANDOM_NEGATIVE_SALT);
    let mut random = 0;
    for _ in 0..CMD_TRAIN_RANDOM_TRIES {
        if random == CMD_TRAIN_RANDOM_NEGATIVES {
            break;
        }
        state = splitmix64(state);
        let at = (state % commands) as usize;
        let candidate = pool.as_ref().map_or(at as u32, |pool| pool[at]);
        if !usable(candidate) || negatives.iter().any(|&(idx, _)| idx == candidate) {
            continue;
        }
        if let Some(score) = fuzzy(candidate) {
            negatives.push((candidate, score));
            random += 1;
        }
    }
    if negatives.is_empty() {
        return None;
    }

    let mut members = negatives;
    members.push((positive, positive_score));
    let newest = newest_matching(replayer, query);
    // Not the positive first: tied scores would resolve in its favour.
    members.sort_unstable_by_key(|&(idx, _)| idx);
    let rows = members
        .into_iter()
        .map(|(idx, score)| GroupRow {
            idx,
            label: if idx == positive { 1.0 } else { 0.0 },
            features: feature_row(index, &ctx, query, &newest, idx, score),
        })
        .collect();
    Some(Group {
        rows,
        weight: source_weight(run.source),
        timestamp: run.started_at,
    })
}

/// The group of one Ctrl-R pick (section 5.3): a row per impression of its
/// picker process, each with its own query, labelled by whether it shows
/// the picked command; the whole episode scored at the moment of the pick.
/// `None` without both a positive and a negative row.
fn build_pick_group(replayer: &Replayer, episode: &PickEpisode) -> Option<Group> {
    let pick = &episode.pick;
    let ctx = replayer.context(
        pick.timestamp,
        pick.cwd.as_deref(),
        pick.repo_root.as_deref(),
        pick.shell_session.as_deref(),
    );
    let index = &replayer.index;
    // One walk of the recency order per distinct query of the episode.
    let mut newest: FxHashMap<&str, Vec<(i64, i64)>> = FxHashMap::default();
    for impression in &episode.impressions {
        let query = impression.query.as_str();
        newest
            .entry(query)
            .or_insert_with(|| newest_matching(replayer, query));
    }
    let rows: Vec<GroupRow> = episode
        .impressions
        .iter()
        .filter_map(|impression| {
            let idx = replayer.knows(impression.command_id)?;
            let query = impression.query.as_str();
            // An impression shows a command that matched when it was
            // logged; NO_FZF_MATCH if it no longer does.
            let score = fzf_score(&FzfQuery::new(query), &index.entries[idx as usize].text);
            Some(GroupRow {
                idx,
                label: if impression.command_id == pick.command_id {
                    1.0
                } else {
                    0.0
                },
                features: feature_row(index, &ctx, query, &newest[query], idx, score),
            })
        })
        .collect();
    let has = |label: f32| rows.iter().any(|row| row.label == label);
    if !has(1.0) || !has(0.0) {
        return None;
    }
    Some(Group {
        rows,
        weight: CMD_PICK_WEIGHT,
        timestamp: pick.timestamp,
    })
}

// ---------------------------------------------------------------------------
// The training set
// ---------------------------------------------------------------------------

pub(super) struct BuildOptions {
    pub seed: u64,
    pub max_run_groups: usize,
    /// `Some(t)`: stop the timeline at the first run or event at or after `t`.
    pub until: Option<i64>,
    /// A benchmark control, never set by training: each group's positive
    /// label moves to a row of the group drawn by a seeded hash, so the
    /// labels carry nothing true (04-bench.md section 5.7).
    pub shuffle_labels: bool,
}

/// Groups by kind, and rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct GroupCounts {
    pub pick: usize,
    pub hook: usize,
    pub import: usize,
    pub rows: usize,
}

impl GroupCounts {
    pub fn groups(&self) -> usize {
        self.pick + self.hook + self.import
    }
}

pub(super) struct BuiltSet {
    pub set: crate::training::TrainingSet,
    pub counts: GroupCounts,
    /// The replay state at `until` (or the end).
    pub replayer: Replayer,
}

/// Collects groups into the columns of a `TrainingSet`. Group ids are 1, 2,
/// 3, ... in the order groups arrive, which is time order.
#[derive(Default)]
struct SetBuilder {
    features: Vec<f64>,
    labels: Vec<f32>,
    episodes: Vec<i64>,
    timestamps: Vec<f64>,
    weights: Vec<f64>,
    groups: i64,
}

impl SetBuilder {
    fn push(&mut self, group: Group) {
        self.groups += 1;
        for row in group.rows {
            self.features.extend_from_slice(&row.features);
            self.labels.push(row.label);
            self.episodes.push(self.groups);
            self.timestamps.push(group.timestamp as f64);
            self.weights.push(group.weight);
        }
    }

    fn finish(self) -> crate::training::TrainingSet {
        crate::training::TrainingSet::from_parts(
            COMMAND_FEATURE_REGISTRY.len(),
            self.features,
            self.labels,
            self.episodes,
            self.timestamps,
            self.weights,
        )
    }
}

/// How many runs before `until` are eligible for a group: the cap's first
/// pass, with the same rules as the replay (a command that ran before, not
/// the execution of a pick).
fn eligible_runs(input: &ReplayInput, until: Option<i64>) -> usize {
    let known: FxHashSet<i64> = input.commands.iter().map(|c| c.id).collect();
    let mut seen: FxHashSet<i64> = FxHashSet::default();
    let mut picks = PickMemory::default();
    let mut eligible = 0;
    for step in Timeline::new(input, until) {
        match step {
            Step::Run(run) => {
                if !known.contains(&run.command_id) {
                    continue;
                }
                if seen.contains(&run.command_id) && !picks.follows_pick(run) {
                    eligible += 1;
                }
                seen.insert(run.command_id);
            }
            Step::Event(event) => {
                if event.is_pick && seen.contains(&event.command_id) {
                    picks.remember(event);
                }
            }
        }
    }
    eligible
}

/// Replay everything before `options.until` and build the groups of
/// sections 5.2 and 5.3. Deterministic for a given input and seed.
pub(super) fn build_training_set(input: &ReplayInput, options: &BuildOptions) -> BuiltSet {
    let built = build_groups(input, options);
    let mut builder = SetBuilder::default();
    for (ordinal, mut group) in built.groups.into_iter().enumerate() {
        if options.shuffle_labels {
            shuffle_labels(&mut group, splitmix64(options.seed ^ ordinal as u64));
        }
        builder.push(group);
    }
    BuiltSet {
        set: builder.finish(),
        counts: built.counts,
        replayer: built.replayer,
    }
}

/// Permute the labels of `group` (Fisher-Yates on a splitmix64 stream from
/// `state`): the group keeps its rows and its number of positives, but which
/// rows are positive no longer depends on what was run.
fn shuffle_labels(group: &mut Group, mut state: u64) {
    for i in (1..group.rows.len()).rev() {
        state = splitmix64(state);
        let j = (state % (i as u64 + 1)) as usize;
        let (a, b) = (group.rows[i].label, group.rows[j].label);
        group.rows[i].label = b;
        group.rows[j].label = a;
    }
}

/// The replay state after every run and event before `until` (or all of
/// them), without building any group: what serving would have loaded then.
pub(super) fn replay(input: &ReplayInput, until: Option<i64>) -> Replayer {
    let mut replayer = Replayer::new(&input.commands);
    for step in Timeline::new(input, until) {
        replayer.apply_step(step);
    }
    replayer
}

/// The command model's fit: the shared one (`crate::training`), at most
/// `CMD_MODEL_MAX_TREES` trees. Training and the relevance benchmark both
/// fit through here.
pub(super) fn fit_command_model(
    set: &crate::training::TrainingSet,
    seed: u64,
) -> Result<crate::training::Fitted> {
    crate::training::fit_capped(
        set,
        &command_feature_specs(),
        seed as u32,
        CMD_MODEL_MAX_TREES,
    )
}

/// The groups of a replay, in time order.
struct Groups {
    groups: Vec<Group>,
    counts: GroupCounts,
    replayer: Replayer,
}

fn build_groups(input: &ReplayInput, options: &BuildOptions) -> Groups {
    let first_grouped = eligible_runs(input, options.until).saturating_sub(options.max_run_groups);
    let mut replayer = Replayer::new(&input.commands);
    let mut groups = Vec::new();
    let mut counts = GroupCounts::default();
    let mut ordinal = 0;
    for step in Timeline::new(input, options.until) {
        match step {
            Step::Run(run) => {
                if let Some(idx) = replayer.knows(run.command_id)
                    && !replayer.follows_pick(run)
                {
                    if ordinal >= first_grouped {
                        let text = &replayer.index.entries[idx as usize].text;
                        let query = training_query(options.seed, run.id, text);
                        let group = build_run_group(&replayer, run, &query, options.seed);
                        if let Some(group) = group {
                            match run.source {
                                Source::Hook => counts.hook += 1,
                                Source::Import => counts.import += 1,
                            }
                            counts.rows += group.rows.len();
                            groups.push(group);
                        }
                    }
                    ordinal += 1;
                }
                replayer.apply_run(run);
            }
            Step::Event(event) => {
                if let Some(episode) = replayer.apply_event(event) {
                    if let Some(group) = build_pick_group(&replayer, &episode) {
                        counts.pick += 1;
                        counts.rows += group.rows.len();
                        groups.push(group);
                    }
                    replayer.commit_pick(&episode);
                }
            }
        }
    }
    Groups {
        groups,
        counts,
        replayer,
    }
}

// ---------------------------------------------------------------------------
// Model stats
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(super) struct FeatureGain {
    pub feature: String,
    pub gain: f64,
}

/// `cmd_model_stats.json` (section 6.3).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(super) struct CmdModelStats {
    pub trained_at: String,
    pub trained_at_unix: i64,
    /// Rounded to 2 decimals.
    pub training_duration_seconds: f64,
    /// Registry order at training time.
    pub feature_names: Vec<String>,
    pub num_rows: usize,
    pub pick_groups: usize,
    pub hook_groups: usize,
    pub import_groups: usize,
    /// Groups weighted by kind: what the blend weight is computed from.
    pub blend_examples: f64,
    pub best_iteration: usize,
    /// NDCG@1, NDCG@5.
    pub validation_ndcg: Vec<f64>,
    /// Every feature, gain descending.
    pub feature_gain: Vec<FeatureGain>,
    pub seed: u64,
}

impl CmdModelStats {
    pub(super) fn from_fit(
        fitted: &crate::training::Fitted,
        counts: &GroupCounts,
        seed: u64,
        seconds: f64,
        now: jiff::Timestamp,
    ) -> Self {
        let names = command_feature_names();
        assert_eq!(
            fitted.gain_importance.len(),
            names.len(),
            "one gain per feature"
        );
        let mut feature_gain: Vec<FeatureGain> = names
            .iter()
            .zip(&fitted.gain_importance)
            .map(|(name, &gain)| FeatureGain {
                feature: (*name).to_owned(),
                gain,
            })
            .collect();
        feature_gain.sort_by(|a, b| b.gain.total_cmp(&a.gain));
        CmdModelStats {
            trained_at: now.to_string(),
            trained_at_unix: now.as_second(),
            training_duration_seconds: (seconds * 100.0).round() / 100.0,
            feature_names: names.iter().map(|name| (*name).to_owned()).collect(),
            num_rows: counts.rows,
            pick_groups: counts.pick,
            hook_groups: counts.hook,
            import_groups: counts.import,
            blend_examples: blend_examples(counts),
            best_iteration: fitted.best_iteration,
            validation_ndcg: fitted.validation_ndcg.clone(),
            feature_gain,
            seed,
        }
    }

    pub fn groups(&self) -> usize {
        self.pick_groups + self.hook_groups + self.import_groups
    }
}

fn blend_examples(counts: &GroupCounts) -> f64 {
    counts.pick as f64 * CMD_PICK_WEIGHT
        + counts.hook as f64 * CMD_HOOK_WEIGHT
        + counts.import as f64 * CMD_IMPORT_WEIGHT
}

// ---------------------------------------------------------------------------
// Retrain
// ---------------------------------------------------------------------------

pub(super) enum RetrainOutcome {
    #[allow(dead_code, reason = "tests and the benchmark (Phase D) read the stats")]
    Trained(Box<CmdModelStats>),
    NotEnoughData {
        #[allow(dead_code, reason = "tests read it")]
        groups: usize,
    },
    AlreadyRunning,
    /// Commands or runs were deleted while it trained; nothing was written.
    HistoryShrank,
}

/// `cmd_train.lock`, held from creation to drop. The file holds a token
/// unique to this holder; a heartbeat thread refreshes its mtime so a long
/// fit (or one paused by a suspend) is not judged stale; dropping it (on
/// every exit path, unwinding included) stops the heartbeat and removes the
/// file only if it still holds this token, never another training's lock.
struct TrainLock {
    path: PathBuf,
    token: String,
    stop: Option<std::sync::mpsc::Sender<()>>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

impl TrainLock {
    /// The lock, or `None` while another training holds it. A lock older than
    /// `CMD_TRAIN_LOCK_STALE_SECS` (its training was killed) is taken over,
    /// once, and only if it still holds the token read when it was judged stale.
    fn take(data_dir: &Path, now: SystemTime) -> Result<Option<TrainLock>> {
        let path = data_dir.join(LOCK_FILE);
        let token = format!(
            "{} {}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        for attempt in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(token.as_bytes()) {
                        // An empty lock would block every training for
                        // CMD_TRAIN_LOCK_STALE_SECS.
                        let _ = std::fs::remove_file(&path);
                        return Err(error).context("could not write the training lock");
                    }
                    return Ok(Some(TrainLock::hold(path, token)));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt > 0 || !is_stale_lock(&path, now) {
                        return Ok(None);
                    }
                    let stale = std::fs::read_to_string(&path).unwrap_or_default();
                    remove_if_holds(&path, &stale)
                        .context("could not remove a stale training lock")?;
                }
                Err(error) => return Err(error).context("could not create the training lock"),
            }
        }
        Ok(None)
    }

    fn hold(path: PathBuf, token: String) -> TrainLock {
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let beat_path = path.clone();
        let heartbeat = std::thread::spawn(move || {
            let interval = Duration::from_secs(LOCK_HEARTBEAT_SECS);
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(interval)
            {
                if let Ok(file) = std::fs::File::options().write(true).open(&beat_path) {
                    let _ = file.set_modified(SystemTime::now());
                }
            }
        });
        TrainLock {
            path,
            token,
            stop: Some(stop),
            heartbeat: Some(heartbeat),
        }
    }
}

impl Drop for TrainLock {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
        let _ = remove_if_holds(&self.path, &self.token);
    }
}

/// Remove the lock at `path` if it holds `token`. Not atomic: a holder
/// that replaces it between the read and the removal loses its lock, a
/// window of microseconds after a 15-minute staleness.
fn remove_if_holds(path: &Path, token: &str) -> std::io::Result<()> {
    match std::fs::read_to_string(path) {
        Ok(held) if held == token => match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// How long ago `path` was modified; `None` when it does not exist. A
/// modification time in the future counts as now.
fn age(path: &Path, now: SystemTime) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(now.duration_since(modified).unwrap_or(Duration::ZERO))
}

fn is_stale_lock(path: &Path, now: SystemTime) -> bool {
    age(path, now).is_some_and(|age| age > Duration::from_secs(CMD_TRAIN_LOCK_STALE_SECS))
}

/// Where the report goes: stdout, or `training.log` under `--quiet`.
struct Report<'a> {
    data_dir: &'a Path,
    quiet: bool,
}

impl Report<'_> {
    fn write(&self, text: &str) -> Result<()> {
        if !self.quiet {
            let mut out = std::io::stdout().lock();
            writeln!(out, "{text}")?;
            return Ok(());
        }
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.data_dir.join(TRAINING_LOG))
            .context("could not open the training log")?;
        let local = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
        writeln!(
            log,
            "\n=== Command model training run at {} ===\n{}",
            local.strftime("%Y-%m-%d %H:%M:%S"),
            text
        )?;
        Ok(())
    }
}

/// Train the command model from everything recorded in `data_dir` and write
/// `cmd_model.txt` and `cmd_model_stats.json` (section 6.2). Under `quiet`
/// the report, and an error, go to `training.log` instead of stdout.
pub(super) fn retrain(data_dir: &Path, seed: u64, quiet: bool) -> Result<RetrainOutcome> {
    let report = Report { data_dir, quiet };
    let outcome = train_and_write(data_dir, seed, &report);
    match &outcome {
        Ok(RetrainOutcome::AlreadyRunning) => {
            log::info!("command model training skipped: another training run holds the lock");
        }
        Ok(_) => {}
        Err(error) => {
            log::error!("command model training failed: {error:#}");
            if quiet {
                // The error is already on its way to main; the log is a bonus.
                let _ = report.write(&format!("Training failed: {error:#}"));
            }
        }
    }
    outcome
}

fn train_and_write(data_dir: &Path, seed: u64, report: &Report) -> Result<RetrainOutcome> {
    let started = Instant::now();
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("could not create {}", data_dir.display()))?;
    let now = jiff::Timestamp::now();
    std::fs::write(
        data_dir.join(ATTEMPT_FILE),
        format!("{}\n", now.as_second()),
    )
    .context("could not record the training attempt")?;
    let Some(_lock) = TrainLock::take(data_dir, SystemTime::now())? else {
        return Ok(RetrainOutcome::AlreadyRunning);
    };

    let (input, before) = {
        let store = CmdStore::open(data_dir)?;
        let before = store.counts()?;
        (ReplayInput::read(&store)?, before)
    };
    let options = BuildOptions {
        seed,
        max_run_groups: CMD_TRAIN_MAX_RUN_GROUPS,
        until: None,
        shuffle_labels: false,
    };
    let built = build_training_set(&input, &options);
    drop(input);
    let counts = built.counts;
    let not_enough = || -> Result<RetrainOutcome> {
        let groups = counts.groups();
        log::info!("command model: nothing to train on yet ({groups} groups)");
        report.write(&format!(
            "Nothing to train on yet: {groups} groups (need {CMD_TRAIN_MIN_GROUPS}). \
             Ranking stays on the heuristic."
        ))?;
        Ok(RetrainOutcome::NotEnoughData { groups })
    };
    if counts.groups() < CMD_TRAIN_MIN_GROUPS {
        return not_enough();
    }
    let fitted = match fit_command_model(&built.set, seed) {
        Ok(fitted) => fitted,
        Err(error) if is_not_enough_for_a_split(&error) => return not_enough(),
        Err(error) => return Err(error),
    };
    drop(built);

    let seconds = started.elapsed().as_secs_f64();
    let stats = CmdModelStats::from_fit(&fitted, &counts, seed, seconds, now);
    let stats_json = serde_json::to_string_pretty(&stats)?;
    let written = write_unless_shrunk(
        data_dir,
        before,
        &[
            (MODEL_FILE, fitted.model.as_bytes()),
            (STATS_FILE, stats_json.as_bytes()),
        ],
    )?;
    if !written {
        log::info!("command model discarded: history was deleted while it trained");
        report.write("History was deleted while training; no model written.")?;
        return Ok(RetrainOutcome::HistoryShrank);
    }

    let ndcg: Vec<String> = fitted
        .validation_ndcg
        .iter()
        .map(|v| format!("{v:.4}"))
        .collect();
    report.write(&format!(
        "Trained on {} rows in {} groups ({} picks, {} runs, {} imported): train {}, validation {}, test {}\n\
         Best iteration {}, validation NDCG@1,5 {}\n\
         Took {:.2}s",
        counts.rows,
        counts.groups(),
        counts.pick,
        counts.hook,
        counts.import,
        fitted.train_rows,
        fitted.validation_rows,
        fitted.test_rows,
        fitted.best_iteration,
        ndcg.join(", "),
        started.elapsed().as_secs_f64()
    ))?;
    log::info!(
        "command model trained: {} rows, {} groups, {} trees",
        counts.rows,
        counts.groups(),
        fitted.best_iteration
    );
    Ok(RetrainOutcome::Trained(Box::new(stats)))
}

/// Put `files` (name, contents) into `data_dir` unless the history shrank
/// since `before` was counted: a purge or a delete while training must not
/// leave a model of what is gone (E-L-15). Each file is written beside its
/// target first; the history is checked; then they are renamed into place in
/// order (model first: a failure between the two leaves the old stats, whose
/// feature names still match, E-L-10). A shrink found before the renames
/// removes only the new files, so the previous model stays; one found after
/// them (a purge that landed in between) removes the model files, as the
/// purge itself does. Returns whether the files are in place.
fn write_unless_shrunk(
    data_dir: &Path,
    before: super::store::StoreCounts,
    files: &[(&str, &[u8])],
) -> Result<bool> {
    let shrunk = || -> Result<bool> {
        let now = CmdStore::open(data_dir)?.counts()?;
        let runs = |c: &super::store::StoreCounts| c.hook_runs + c.import_runs;
        Ok(now.commands < before.commands || runs(&now) < runs(&before))
    };
    let pending = |name: &str| data_dir.join(format!("{name}.{}.pending", std::process::id()));
    let discard = || {
        for (name, _) in files {
            let _ = std::fs::remove_file(pending(name));
        }
    };
    if shrunk()? {
        return Ok(false);
    }
    for (name, contents) in files {
        if let Err(error) = std::fs::write(pending(name), contents) {
            discard();
            return Err(error).with_context(|| format!("could not write {name}"));
        }
    }
    match shrunk() {
        Ok(false) => {}
        other => {
            discard();
            return other.map(|_| false);
        }
    }
    for (name, _) in files {
        if let Err(error) = std::fs::rename(pending(name), data_dir.join(name)) {
            discard();
            return Err(error).with_context(|| format!("could not move {name} into place"));
        }
    }
    super::privacy::tighten_permissions(data_dir)?;
    if shrunk()? {
        super::privacy::remove_model_files(data_dir)
            .context("could not remove a model of deleted history")?;
        return Ok(false);
    }
    Ok(true)
}

/// The time split had too few groups for one of its parts (E-L-02).
fn is_not_enough_for_a_split(error: &anyhow::Error) -> bool {
    error.to_string().starts_with(NOT_ENOUGH_FOR_A_SPLIT)
}

// ---------------------------------------------------------------------------
// The retrain trigger
// ---------------------------------------------------------------------------

/// Whether the picker should start a training now: the last attempt (or
/// none) is at least `CMD_RETRAIN_MIN_INTERVAL_SECS` old, and no training
/// holds a fresh lock.
fn retrain_due(data_dir: &Path, now: SystemTime) -> bool {
    let attempted_long_ago = age(&data_dir.join(ATTEMPT_FILE), now)
        .is_none_or(|age| age >= Duration::from_secs(CMD_RETRAIN_MIN_INTERVAL_SECS));
    let lock = data_dir.join(LOCK_FILE);
    let unlocked = !lock.exists() || is_stale_lock(&lock, now);
    (attempted_long_ago || model_needs_new_features(data_dir)) && unlocked
}

/// The model on disk was trained with another feature list (an older
/// release), so `CmdModel::load` refuses it and ranking is the heuristic's
/// until a retrain: due at once rather than after the interval, unless an
/// attempt was made since that model was written (one that found too little
/// to train, which would otherwise repeat at every picker start).
fn model_needs_new_features(data_dir: &Path) -> bool {
    let stats = data_dir.join(STATS_FILE);
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let attempted_since = match (modified(&data_dir.join(ATTEMPT_FILE)), modified(&stats)) {
        (Some(attempt), Some(model)) => attempt > model,
        _ => false,
    };
    !attempted_since
        && std::fs::read_to_string(&stats)
            .ok()
            .and_then(|text| serde_json::from_str::<CmdModelStats>(&text).ok())
            .is_some_and(|stats| stats.feature_names != command_feature_names())
}

/// Start `exe --data-dir <data_dir> history --retrain --quiet` detached: its
/// own process group, so it outlives the picker and the terminal's SIGINT.
fn spawn_retrain(exe: &Path, data_dir: &Path) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    Command::new(exe)
        .arg("--data-dir")
        .arg(data_dir)
        .args(["history", "--retrain", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

/// Called once per picker, off the UI thread, after the first results: start
/// a background training if one is due (section 8). The new model is used
/// by the next picker. Failures are logged and otherwise ignored (E-L-09).
pub(super) fn maybe_spawn_retrain(data_dir: &Path, now: SystemTime) {
    let disabled = std::env::var_os(NO_RETRAIN_ENV).is_some();
    if let Some(mut child) = start_retrain_if_due(data_dir, now, disabled, std::env::current_exe) {
        // Reap it if the picker is still running when it ends.
        let _ = child.wait();
    }
}

/// [`maybe_spawn_retrain`] with the environment and `current_exe` passed in.
/// The child, if one was started.
fn start_retrain_if_due(
    data_dir: &Path,
    now: SystemTime,
    disabled: bool,
    exe: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Option<std::process::Child> {
    if disabled || !retrain_due(data_dir, now) {
        return None;
    }
    match exe().and_then(|exe| spawn_retrain(&exe, data_dir)) {
        Ok(child) => Some(child),
        Err(error) => {
            log::warn!("could not start command model training: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) mod test_support {
    //! Data directories with a trained model, for the tests of every module.
    use super::*;

    /// The newest synthetic run is 60 s before this.
    pub(in crate::cmd) const FIXTURE_ANCHOR: i64 = 1_800_000_000;

    /// A scratch directory under the temp dir, removed on drop.
    pub(in crate::cmd) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(in crate::cmd) fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "psychic-cmd-train-{}-{}",
                tag,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Fill `data_dir` with the synthetic generator's corpus of 300 commands
    /// and 4,000 runs (seed 1, newest run at `FIXTURE_ANCHOR - 60`).
    pub(in crate::cmd) fn generate_fixture(data_dir: &Path) {
        let store = CmdStore::open(data_dir).unwrap();
        super::super::corpus::generate(&store, 300, 4000, 1, FIXTURE_ANCHOR).unwrap();
    }

    /// [`generate_fixture`] and a model trained on it.
    pub(in crate::cmd) fn trained_fixture(data_dir: &Path) -> CmdModelStats {
        generate_fixture(data_dir);
        match retrain(data_dir, 42, true).unwrap() {
            RetrainOutcome::Trained(stats) => *stats,
            _ => panic!("the fixture trains"),
        }
    }

    /// The text of a real model that takes 2 features, not the registry's
    /// 23: every prediction through a `CmdModel` holding it fails.
    pub(in crate::cmd) fn two_feature_model() -> String {
        let groups = 40;
        let mut features = Vec::new();
        let mut labels = Vec::new();
        let mut episodes = Vec::new();
        for group in 0..groups {
            for row in 0..2 {
                features.extend([(group * 7 % 5 + row) as f64, (group % 3) as f64]);
                labels.push(if row == 1 { 1.0 } else { 0.0 });
                episodes.push(group as i64 + 1);
            }
        }
        let rows = labels.len();
        let set = crate::training::TrainingSet::from_parts(
            2,
            features,
            labels,
            episodes,
            (0..rows).map(|i| i as f64).collect(),
            vec![1.0; rows],
        );
        let specs = [
            crate::training::FeatureSpec {
                name: "a",
                monotone: 0,
            },
            crate::training::FeatureSpec {
                name: "b",
                monotone: 0,
            },
        ];
        crate::training::fit(&set, &specs).unwrap().model
    }
}
