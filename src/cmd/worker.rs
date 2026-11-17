//! The picker's worker thread (`docs/ctrl-r/02-browse.md` section 6).
//!
//! It owns the [`CommandIndex`], the [`RankContext`], one fuzzy matcher and
//! a [`CmdStore`] connection. The UI thread asks with [`CmdRequest`]s and is
//! answered with [`CmdResponse`]s on its own event channel. No SQL runs per
//! keystroke: a query is a filter over the index and a ranking, nothing else.
//!
//! The filter and the narrowing rule are free functions so the benchmark
//! (Phase D) can drive them without a thread.

use super::index::{CommandIndex, LoadParams, RankContext};
use super::rank::{
    self, CmdRankTimings, CommandCandidate, CommandRanking, CommandScore, HeuristicParts,
};
use super::store::{CmdStore, DirUse, PrecededBy};
use super::tuning::{CMD_PAGE_ROWS, CMD_PAR_FILTER_MIN};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// Which commands the list may show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    All,
    /// Run in the picker's directory at least once.
    Directory,
    /// Run in the picker's repository at least once.
    Repo,
    /// Run in the shell session that opened the picker.
    Session,
}

impl Scope {
    /// All -> Directory -> Repo -> Session -> All.
    pub fn next(self) -> Scope {
        match self {
            Scope::All => Scope::Directory,
            Scope::Directory => Scope::Repo,
            Scope::Repo => Scope::Session,
            Scope::Session => Scope::All,
        }
    }

    /// The reverse of [`Scope::next`].
    pub fn prev(self) -> Scope {
        match self {
            Scope::All => Scope::Session,
            Scope::Directory => Scope::All,
            Scope::Repo => Scope::Directory,
            Scope::Session => Scope::Repo,
        }
    }

    /// How the input box names it.
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Directory => "this directory",
            Scope::Repo => "this repo",
            Scope::Session => "this session",
        }
    }
}

pub(super) enum CmdRequest {
    UpdateQuery {
        query: String,
        query_id: u64,
        scope: Scope,
    },
    /// `CMD_PAGE_ROWS` rows from `start` of the current ranking.
    GetRows {
        query_id: u64,
        start: usize,
    },
    Details {
        command_id: i64,
    },
    /// Delete, then answer like `UpdateQuery` for the current query under `query_id`.
    Delete {
        command_id: i64,
        query_id: u64,
    },
    Shutdown,
}

pub(super) enum CmdResponse {
    Loaded {
        commands: usize,
        load_ms: f64,
    },
    /// The index could not be loaded. Never contains command text.
    LoadFailed {
        message: String,
    },
    QueryUpdated {
        query_id: u64,
        query: String,
        total_matched: usize,
        total_commands: usize,
        rows: Vec<CmdRow>,
        rank_ms: f64,
    },
    Rows {
        query_id: u64,
        start: usize,
        rows: Vec<CmdRow>,
    },
    Details(CmdDetails),
    /// A `Delete` failed in the database; the `QueryUpdated` that follows
    /// still lists the command.
    DeleteFailed,
}

/// One row of the list, as the UI draws it.
#[derive(Clone)]
pub(super) struct CmdRow {
    pub command_id: i64,
    pub text: Arc<str>,
    pub last_run_at: i64,
    pub run_count: u32,
    pub pick_count: u32,
    pub runs_here: u32,
    pub last_exit_code: Option<i32>,
    pub score: f64,
    pub heuristic: f64,
    pub heuristic_parts: HeuristicParts,
    pub ml_score: Option<f64>,
    pub ml_weight: f64,
    pub fuzzy_score: i64,
    /// Registry order; empty when no model scored the row.
    pub features: Vec<f64>,
}

/// What the preview shows below the counts, fetched when a row is selected.
pub(super) struct CmdDetails {
    pub command_id: i64,
    pub dirs: Vec<DirUse>,
    pub preceded_by: Vec<PrecededBy>,
}

/// Start the worker. It loads the index from `data_dir` first, so it is
/// spawned before the terminal is set up.
pub(super) fn spawn<T>(
    data_dir: &Path,
    params: LoadParams,
    event_tx: Sender<T>,
) -> (Sender<CmdRequest>, JoinHandle<()>)
where
    T: From<CmdResponse> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let data_dir: PathBuf = data_dir.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("cmd-worker".to_string())
        .spawn(move || run(&data_dir, params, &rx, &event_tx))
        .expect("the worker thread starts");
    (tx, handle)
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

/// Whether the filter may start from the previous matches: the query grew
/// (a text that matches the longer query also matches its prefix) and the
/// scope stayed.
pub(super) fn narrowing_applies(
    query: &str,
    scope: Scope,
    last_query: &str,
    last_scope: Scope,
) -> bool {
    !query.is_empty()
        && !last_query.is_empty()
        && query.starts_with(last_query)
        && scope == last_scope
}

/// The commands that match `query` in `scope`, in ascending idx order.
/// `base`: the idx list to test, ascending; `None` = every entry.
pub(super) fn filter_candidates(
    index: &CommandIndex,
    ctx: &RankContext,
    matcher: &SkimMatcherV2,
    query: &str,
    scope: Scope,
    base: Option<&[u32]>,
) -> Vec<CommandCandidate> {
    let test = |idx: u32| -> Option<CommandCandidate> {
        let entry = &index.entries[idx as usize];
        if entry.deleted {
            return None;
        }
        let in_scope = match scope {
            Scope::All => true,
            Scope::Directory => index.dir_stat(idx, ctx.dir).is_some(),
            Scope::Repo => index.runs_in_repo(idx, ctx.repo) > 0,
            Scope::Session => index.session_commands.contains(&idx),
        };
        if !in_scope {
            return None;
        }
        let fuzzy_score = if query.is_empty() {
            0
        } else {
            matcher.fuzzy_match(&entry.text, query)?
        };
        Some(CommandCandidate { idx, fuzzy_score })
    };
    let all = u32::try_from(index.entries.len()).expect("fewer than 2^32 commands");
    let size = base.map_or(all as usize, <[u32]>::len);
    // Without a query there is nothing to match, and a pass over 50,000
    // entries costs less than waking the thread pool.
    let parallel = !query.is_empty() && size >= CMD_PAR_FILTER_MIN;
    match (base, parallel) {
        (Some(base), true) => base.par_iter().filter_map(|&idx| test(idx)).collect(),
        (Some(base), false) => base.iter().filter_map(|&idx| test(idx)).collect(),
        (None, true) => (0..all).into_par_iter().filter_map(test).collect(),
        (None, false) => (0..all).filter_map(test).collect(),
    }
}

// ---------------------------------------------------------------------------
// TIMING line
// ---------------------------------------------------------------------------

/// What one query cost, for the one `cmd_query` TIMING line it writes.
pub(super) struct CmdQueryTimings {
    pub filter_ms: f64,
    pub rank: CmdRankTimings,
    pub total_ms: f64,
    /// Size of the base set the filter tested.
    pub candidates: usize,
    pub matched: usize,
    /// In characters.
    pub query_len: usize,
    pub narrowed: bool,
    /// The matches of an earlier query were reused (a deletion back to it).
    pub reused: bool,
}

/// Milliseconds rounded to 3 decimals, as JSON.
fn ms(value: f64) -> String {
    format!("{}", (value * 1000.0).round() / 1000.0)
}

fn optional_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_string(), ms)
}

impl CmdQueryTimings {
    /// The line's JSON: numbers and one boolean, never a query or a command.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"op\":\"cmd_query\",\"filter_ms\":{},\"heuristic_ms\":{},\"features_ms\":{},\
             \"predict_ms\":{},\"blend_ms\":{},\"total_ms\":{},\"candidates\":{},\"matched\":{},\
             \"stage2\":{},\"query_len\":{},\"narrowed\":{},\"reused\":{}}}",
            ms(self.filter_ms),
            ms(self.rank.heuristic_ms),
            optional_ms(self.rank.features_ms),
            optional_ms(self.rank.predict_ms),
            optional_ms(self.rank.blend_ms),
            ms(self.total_ms),
            self.candidates,
            self.matched,
            self.rank.stage2,
            self.query_len,
            self.narrowed,
            self.reused
        )
    }
}

// ---------------------------------------------------------------------------
// The thread
// ---------------------------------------------------------------------------

/// Take everything already waiting. A newer `UpdateQuery` replaces an older
/// one that is last in the queue, and a newer `Details` every older
/// `Details` (scrolling asks for one per row, and each is SQL on this
/// thread, ahead of the next keystroke); every other request survives.
fn drain_requests(rx: &Receiver<CmdRequest>, first: CmdRequest) -> Vec<CmdRequest> {
    let mut queue = vec![first];
    while let Ok(request) = rx.try_recv() {
        push_superseding(&mut queue, request);
    }
    queue
}

fn push_superseding(queue: &mut Vec<CmdRequest>, request: CmdRequest) {
    if matches!(request, CmdRequest::UpdateQuery { .. })
        && matches!(queue.last(), Some(CmdRequest::UpdateQuery { .. }))
    {
        queue.pop();
    }
    if matches!(request, CmdRequest::Details { .. }) {
        queue.retain(|queued| !matches!(queued, CmdRequest::Details { .. }));
    }
    queue.push(request);
}

/// The index and everything the current query left behind.
struct Worker {
    index: CommandIndex,
    ctx: RankContext,
    matcher: SkimMatcherV2,
    store: Option<CmdStore>,
    /// Commands not deleted.
    live: usize,
    /// The matches of the query being typed and of its prefixes, shortest
    /// first; each entry's query is a prefix of the next one's.
    trail: Vec<Matches>,
    // What `GetRows` and `Delete` refer to.
    current_query: String,
    current_scope: Scope,
    current_query_id: u64,
    current: Arc<CommandRanking>,
    /// The ranking of the empty query in each scope it was asked in: it
    /// cannot change until a delete, and clearing the query is common.
    empty_rankings: Vec<(Scope, Arc<CommandRanking>)>,
}

/// What one non-empty query matched in one scope.
struct Matches {
    query: String,
    scope: Scope,
    candidates: Vec<CommandCandidate>,
}

/// The result of one filter pass.
struct Filtered {
    candidates: Vec<CommandCandidate>,
    /// How many commands were tested: 0 when an earlier result was reused.
    base_size: usize,
    narrowed: bool,
    reused: bool,
}

impl Worker {
    fn new(
        index: CommandIndex,
        ctx: RankContext,
        store: Option<CmdStore>,
        matcher: SkimMatcherV2,
    ) -> Self {
        let live = index.entries.iter().filter(|e| !e.deleted).count();
        Worker {
            index,
            ctx,
            matcher,
            store,
            live,
            trail: Vec::new(),
            current_query: String::new(),
            current_scope: Scope::All,
            current_query_id: 0,
            current: Arc::new(CommandRanking {
                scores: Vec::new(),
                sorted: 0,
                timings: CmdRankTimings::default(),
            }),
            empty_rankings: Vec::new(),
        }
    }

    /// The filter. A query typed earlier in this run of typing (deleting
    /// back to it, even through the empty query) reuses its matches; a longer one starts from the matches
    /// of its longest prefix seen (narrowing); anything else tests every
    /// command. Both give exactly what testing every command would.
    fn filter(&mut self, query: &str, scope: Scope) -> Filtered {
        if query.is_empty() {
            // Every command, nothing to match: no trail to use or extend,
            // and the one there stays for typing the same letters again.
            return Filtered {
                candidates: filter_candidates(
                    &self.index,
                    &self.ctx,
                    &self.matcher,
                    query,
                    scope,
                    None,
                ),
                base_size: self.index.entries.len(),
                narrowed: false,
                reused: false,
            };
        }
        while let Some(last) = self.trail.last() {
            if narrowing_applies(query, scope, &last.query, last.scope) {
                break;
            }
            self.trail.pop();
        }
        if let Some(last) = self.trail.last()
            && last.query == query
        {
            return Filtered {
                candidates: last.candidates.clone(),
                base_size: 0,
                narrowed: false,
                reused: true,
            };
        }
        let base: Option<Vec<u32>> = self
            .trail
            .last()
            .map(|last| last.candidates.iter().map(|c| c.idx).collect());
        let base_size = base.as_ref().map_or(self.index.entries.len(), Vec::len);
        let candidates = filter_candidates(
            &self.index,
            &self.ctx,
            &self.matcher,
            query,
            scope,
            base.as_deref(),
        );
        self.trail.push(Matches {
            query: query.to_string(),
            scope,
            candidates: candidates.clone(),
        });
        Filtered {
            candidates,
            base_size,
            narrowed: base.is_some(),
            reused: false,
        }
    }

    /// After a delete every earlier result is stale.
    fn forget_narrowing(&mut self) {
        self.trail.clear();
        self.empty_rankings.clear();
    }

    fn row(&self, score: &CommandScore) -> CmdRow {
        let entry = &self.index.entries[score.idx as usize];
        CmdRow {
            command_id: entry.db_id,
            text: Arc::clone(&entry.text),
            last_run_at: entry.last_run_at,
            run_count: entry.run_count,
            pick_count: entry.pick_count,
            runs_here: self
                .index
                .dir_stat(score.idx, self.ctx.dir)
                .map_or(0, |d| d.runs),
            last_exit_code: entry.last_exit_code,
            score: score.score,
            heuristic: score.heuristic,
            heuristic_parts: score.parts,
            ml_score: score.ml_score,
            ml_weight: score.ml_weight,
            fuzzy_score: score.fuzzy_score,
            features: score.features.clone(),
        }
    }

    /// Up to `CMD_PAGE_ROWS` rows of the current ranking from `start`,
    /// sorting the rest of it first when the page reaches past its head.
    fn rows(&mut self, start: usize) -> Vec<CmdRow> {
        let scores = self.current.scores.len();
        let start = start.min(scores);
        let end = (start + CMD_PAGE_ROWS).min(scores);
        if end > self.current.sorted {
            // A cached empty-query ranking shares the Arc: take the cache's
            // handle out first, so the sort happens in place, not on a copy.
            let (empty, current_scope) = (self.current_query.is_empty(), self.current_scope);
            let cached = self
                .empty_rankings
                .iter()
                .position(|(scope, _)| empty && *scope == current_scope)
                .map(|at| self.empty_rankings.swap_remove(at));
            drop(cached);
            Arc::make_mut(&mut self.current).sort_rest(&self.index);
            if empty {
                self.empty_rankings
                    .push((current_scope, Arc::clone(&self.current)));
            }
        }
        self.current.scores[start..end]
            .iter()
            .map(|s| self.row(s))
            .collect()
    }

    /// Filter and rank the current query; answer; then log one TIMING line.
    fn answer_current<T: From<CmdResponse>>(&mut self, event_tx: &Sender<T>) -> bool {
        let started = Instant::now();
        let (query, scope) = (self.current_query.clone(), self.current_scope);
        let cached = query
            .is_empty()
            .then(|| self.empty_rankings.iter().find(|(s, _)| *s == scope))
            .flatten()
            .map(|(_, ranking)| Arc::clone(ranking));
        let (filter_ms, filtered, rank_timings) = match cached {
            Some(ranking) => {
                self.current = ranking;
                let filtered = Filtered {
                    candidates: Vec::new(),
                    base_size: 0,
                    narrowed: false,
                    reused: true,
                };
                (0.0, filtered, CmdRankTimings::default())
            }
            None => {
                let filter_started = Instant::now();
                let filtered = self.filter(&query, scope);
                let filter_ms = filter_started.elapsed().as_secs_f64() * 1000.0;
                let ranking = Arc::new(rank::rank_commands(
                    &query,
                    &filtered.candidates,
                    &self.index,
                    &self.ctx,
                    CMD_PAGE_ROWS,
                ));
                let rank_timings = ranking.timings;
                if query.is_empty() {
                    self.empty_rankings.push((scope, Arc::clone(&ranking)));
                }
                self.current = ranking;
                (filter_ms, filtered, rank_timings)
            }
        };
        let rows = self.rows(0);
        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        let sent = event_tx
            .send(T::from(CmdResponse::QueryUpdated {
                query_id: self.current_query_id,
                query: query.clone(),
                total_matched: self.current.scores.len(),
                total_commands: self.live,
                rows,
                rank_ms: total_ms,
            }))
            .is_ok();
        let timings = CmdQueryTimings {
            filter_ms,
            rank: rank_timings,
            total_ms,
            candidates: filtered.base_size,
            matched: self.current.scores.len(),
            query_len: query.chars().count(),
            narrowed: filtered.narrowed,
            reused: filtered.reused,
        };
        log::info!("TIMING {}", timings.to_json());
        sent
    }

    fn details(&self, command_id: i64) -> CmdDetails {
        let store = self.store.as_ref();
        let fetched = store.map(|store| {
            store
                .command_dirs(command_id)
                .and_then(|dirs| Ok((dirs, store.command_predecessors(command_id)?)))
        });
        match fetched {
            Some(Ok((dirs, preceded_by))) => CmdDetails {
                command_id,
                dirs,
                preceded_by,
            },
            other => {
                if let Some(Err(error)) = other {
                    log::warn!("command details: {}", error);
                }
                CmdDetails {
                    command_id,
                    dirs: Vec::new(),
                    preceded_by: Vec::new(),
                }
            }
        }
    }

    /// Delete one command from the database and the index. `false` when the
    /// database refused; the index is then left as it was (E-B-07).
    fn delete(&mut self, command_id: i64) -> bool {
        let Some(idx) = self.index.idx_of(command_id) else {
            return true; // not known here: nothing to delete
        };
        if self.index.entries[idx as usize].deleted {
            return true;
        }
        let Some(store) = self.store.as_ref() else {
            return false;
        };
        let text = Arc::clone(&self.index.entries[idx as usize].text);
        if let Err(error) = store.delete_command(command_id, &text, self.ctx.now) {
            log::error!("{}", error);
            return false;
        }
        self.index.remove(idx);
        self.live -= 1;
        self.forget_narrowing();
        true
    }

    /// Handle one request; `false` to stop the thread.
    fn handle<T: From<CmdResponse>>(&mut self, request: CmdRequest, event_tx: &Sender<T>) -> bool {
        match request {
            CmdRequest::UpdateQuery {
                query,
                query_id,
                scope,
            } => {
                self.current_query = query;
                self.current_scope = scope;
                self.current_query_id = query_id;
                self.answer_current(event_tx)
            }
            CmdRequest::GetRows { query_id, start } => {
                if query_id != self.current_query_id {
                    return true;
                }
                let rows = self.rows(start);
                event_tx
                    .send(T::from(CmdResponse::Rows {
                        query_id,
                        start,
                        rows,
                    }))
                    .is_ok()
            }
            CmdRequest::Details { command_id } => event_tx
                .send(T::from(CmdResponse::Details(self.details(command_id))))
                .is_ok(),
            CmdRequest::Delete {
                command_id,
                query_id,
            } => {
                if !self.delete(command_id)
                    && event_tx.send(T::from(CmdResponse::DeleteFailed)).is_err()
                {
                    return false;
                }
                self.current_query_id = query_id;
                self.answer_current(event_tx)
            }
            CmdRequest::Shutdown => false,
        }
    }
}

/// After a failed load: empty answers until `Shutdown` (E-B-01).
fn serve_empty<T: From<CmdResponse>>(rx: &Receiver<CmdRequest>, event_tx: &Sender<T>) {
    while let Ok(request) = rx.recv() {
        let response = match request {
            CmdRequest::UpdateQuery {
                query, query_id, ..
            } => CmdResponse::QueryUpdated {
                query_id,
                query,
                total_matched: 0,
                total_commands: 0,
                rows: Vec::new(),
                rank_ms: 0.0,
            },
            CmdRequest::Delete { query_id, .. } => CmdResponse::QueryUpdated {
                query_id,
                query: String::new(),
                total_matched: 0,
                total_commands: 0,
                rows: Vec::new(),
                rank_ms: 0.0,
            },
            CmdRequest::Details { command_id } => CmdResponse::Details(CmdDetails {
                command_id,
                dirs: Vec::new(),
                preceded_by: Vec::new(),
            }),
            CmdRequest::GetRows { .. } => continue,
            CmdRequest::Shutdown => return,
        };
        if event_tx.send(T::from(response)).is_err() {
            return;
        }
    }
}

fn load(
    data_dir: &Path,
    params: &LoadParams,
) -> anyhow::Result<(CmdStore, CommandIndex, RankContext)> {
    let store = CmdStore::open(data_dir)?;
    let (index, ctx) = CommandIndex::load(&store, params)?;
    Ok((store, index, ctx))
}

fn run<T: From<CmdResponse>>(
    data_dir: &Path,
    params: LoadParams,
    rx: &Receiver<CmdRequest>,
    event_tx: &Sender<T>,
) {
    // The rayon threads start, and each builds its matcher scratch space,
    // while the index loads: not inside the load's own parallel part, and
    // not inside the first query.
    let matcher = SkimMatcherV2::default();
    let load_started = Instant::now();
    let loaded = std::thread::scope(|scope| {
        scope.spawn(|| {
            rayon::broadcast(|_| {
                let _ = matcher.fuzzy_match("warm up", "wu");
            })
        });
        load(data_dir, &params)
    });
    let (store, index, ctx) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            log::error!("could not load command history: {}", error);
            let message = error.to_string();
            if event_tx
                .send(T::from(CmdResponse::LoadFailed { message }))
                .is_ok()
            {
                serve_empty(rx, event_tx);
            }
            return;
        }
    };
    let load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
    let mut worker = Worker::new(index, ctx, Some(store), matcher);

    let commands = worker.live;
    if event_tx
        .send(T::from(CmdResponse::Loaded { commands, load_ms }))
        .is_err()
    {
        return;
    }
    log::info!(
        "TIMING {{\"op\":\"cmd_index_load\",\"ms\":{},\"commands\":{}}}",
        ms(load_ms),
        commands
    );
    log::info!(
        "TIMING {{\"op\":\"cmd_worker_ready\",\"ms\":{}}}",
        ms(crate::PROCESS_START.elapsed().as_secs_f64() * 1000.0)
    );

    while let Ok(first) = rx.recv() {
        for request in drain_requests(rx, first) {
            if !worker.handle(request, event_tx) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::index::test_support::{context, index_of};
    use crate::cmd::store::test_support::{command_id, table_rows};
    use crate::cmd::store::{NewRun, Source};
    use std::time::Duration;

    const NOW: i64 = 1_000_000;

    fn texts_of(index: &CommandIndex, candidates: &[CommandCandidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|c| index.entries[c.idx as usize].text.to_string())
            .collect()
    }

    #[test]
    fn tc_b_06_filter_keeps_only_fuzzy_matches() {
        // Not `cargo test`: its g...s is a fuzzy match for `gs`.
        let mut index = index_of(&["git status", "cargo build", "ls"]);
        let ctx = context(NOW);
        let matcher = SkimMatcherV2::default();
        let found = filter_candidates(&index, &ctx, &matcher, "gs", Scope::All, None);
        assert_eq!(texts_of(&index, &found), ["git status"]);
        assert!(found[0].fuzzy_score > 0);

        let all = filter_candidates(&index, &ctx, &matcher, "", Scope::All, None);
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|c| c.fuzzy_score == 0));

        index.remove(0);
        assert!(filter_candidates(&index, &ctx, &matcher, "gs", Scope::All, None).is_empty());
        assert_eq!(
            filter_candidates(&index, &ctx, &matcher, "", Scope::All, None).len(),
            2
        );
    }

    #[test]
    fn tc_b_07_narrowing_starts_from_the_previous_matches() {
        let index = index_of(&["git status", "grep it", "ls"]);
        let ctx = context(NOW);
        let mut worker = Worker::new(index, ctx, None, SkimMatcherV2::default());

        let g = worker.filter("g", Scope::All);
        assert!(!g.narrowed);
        assert_eq!(g.base_size, 3);
        let gi = worker.filter("gi", Scope::All);
        assert!(gi.narrowed);
        assert_eq!(gi.base_size, g.candidates.len());
        let x = worker.filter("x", Scope::All);
        assert!(!x.narrowed);
        assert_eq!(x.base_size, 3);

        // The narrowed result equals a fresh one.
        let fresh = filter_candidates(
            &worker.index,
            &worker.ctx,
            &worker.matcher,
            "gi",
            Scope::All,
            None,
        );
        assert_eq!(gi.candidates, fresh);

        // A scope change between calls disables narrowing.
        worker.filter("g", Scope::All);
        assert!(!worker.filter("gi", Scope::Session).narrowed);
    }

    #[test]
    fn deleting_back_to_an_earlier_query_reuses_its_matches() {
        let index = index_of(&["git status", "grep it", "ls"]);
        let mut worker = Worker::new(index, context(NOW), None, SkimMatcherV2::default());
        let g = worker.filter("g", Scope::All);
        let gi = worker.filter("gi", Scope::All);
        assert!(gi.narrowed);
        let back = worker.filter("g", Scope::All);
        assert!(back.reused && !back.narrowed);
        assert_eq!(back.base_size, 0);
        assert_eq!(back.candidates, g.candidates);
        // Through the empty query too.
        let all = worker.filter("", Scope::All);
        assert_eq!((all.candidates.len(), all.reused), (3, false));
        assert!(worker.filter("g", Scope::All).reused);
        // Not in another scope, and not after a delete.
        assert!(!worker.filter("g", Scope::Session).reused);
        worker.filter("g", Scope::All);
        worker.forget_narrowing();
        assert!(!worker.filter("g", Scope::All).reused);
        // A sibling of the last query narrows from the common prefix.
        worker.filter("gi", Scope::All);
        let gr = worker.filter("gr", Scope::All);
        assert!(gr.narrowed);
        assert_eq!(gr.base_size, g.candidates.len());
        let fresh = filter_candidates(
            &worker.index,
            &worker.ctx,
            &worker.matcher,
            "gr",
            Scope::All,
            None,
        );
        assert_eq!(gr.candidates, fresh);
        // So does an empty query on either side.
        assert!(!narrowing_applies("g", Scope::All, "", Scope::All));
        assert!(!narrowing_applies("", Scope::All, "g", Scope::All));
        assert!(narrowing_applies("gi", Scope::Repo, "g", Scope::Repo));
    }

    #[test]
    fn tc_b_08_scopes() {
        let mut index = index_of(&["a", "b", "c", "d"]);
        let dir = index.dirs.intern("/here");
        let repo = index.repos.intern("/repo");
        index.record_run(0, NOW - 1, Some(dir), None, Some(0), None);
        index.record_run(1, NOW - 1, None, Some(repo), Some(0), None);
        index.record_run(2, NOW - 1, None, None, Some(0), None);
        index.record_run(3, NOW - 1, None, None, Some(0), None);
        index.session_commands.insert(2);
        let mut ctx = context(NOW);
        ctx.dir = Some(dir);
        ctx.repo = Some(repo);
        let matcher = SkimMatcherV2::default();
        let run = |ctx: &RankContext, scope| {
            texts_of(
                &index,
                &filter_candidates(&index, ctx, &matcher, "", scope, None),
            )
        };
        assert_eq!(run(&ctx, Scope::Directory), ["a"]);
        assert_eq!(run(&ctx, Scope::Repo), ["b"]);
        assert_eq!(run(&ctx, Scope::Session), ["c"]);
        assert_eq!(run(&ctx, Scope::All).len(), 4);
        ctx.dir = None;
        assert!(run(&ctx, Scope::Directory).is_empty());
    }

    #[test]
    fn scopes_cycle_both_ways() {
        let mut scope = Scope::All;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(scope.label());
            assert_eq!(scope.next().prev(), scope);
            scope = scope.next();
        }
        assert_eq!(scope, Scope::All);
        assert_eq!(seen, ["all", "this directory", "this repo", "this session"]);
    }

    #[test]
    fn the_parallel_filter_returns_ascending_idx_like_the_sequential_one() {
        let texts: Vec<String> = (0..CMD_PAR_FILTER_MIN * 2)
            .map(|i| format!("cmd {i}"))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let index = index_of(&refs);
        let ctx = context(NOW);
        let matcher = SkimMatcherV2::default();
        let parallel = filter_candidates(&index, &ctx, &matcher, "1", Scope::All, None);
        let small: Vec<u32> = (0..100).collect();
        let sequential = filter_candidates(&index, &ctx, &matcher, "1", Scope::All, Some(&small));
        assert!(parallel.windows(2).all(|w| w[0].idx < w[1].idx));
        assert_eq!(&parallel[..sequential.len()], &sequential[..]);
    }

    fn update(id: u64) -> CmdRequest {
        CmdRequest::UpdateQuery {
            query: id.to_string(),
            query_id: id,
            scope: Scope::All,
        }
    }

    fn describe(request: &CmdRequest) -> String {
        match request {
            CmdRequest::UpdateQuery { query_id, .. } => format!("UpdateQuery({query_id})"),
            CmdRequest::GetRows { .. } => "GetRows".to_string(),
            CmdRequest::Details { .. } => "Details".to_string(),
            CmdRequest::Delete { .. } => "Delete".to_string(),
            CmdRequest::Shutdown => "Shutdown".to_string(),
        }
    }

    #[test]
    fn tc_b_09_a_newer_query_supersedes_only_a_query_right_before_it() {
        let (tx, rx) = mpsc::channel();
        for request in [
            update(2),
            CmdRequest::Details { command_id: 1 },
            update(3),
            update(4),
        ] {
            tx.send(request).unwrap();
        }
        let queue: Vec<String> = drain_requests(&rx, update(1))
            .iter()
            .map(describe)
            .collect();
        assert_eq!(queue, ["UpdateQuery(2)", "Details", "UpdateQuery(4)"]);

        // Only the newest Details survives; what is between stays.
        for request in [
            CmdRequest::Details { command_id: 2 },
            CmdRequest::GetRows {
                query_id: 9,
                start: 200,
            },
            CmdRequest::Details { command_id: 3 },
        ] {
            tx.send(request).unwrap();
        }
        let queue: Vec<String> = drain_requests(&rx, CmdRequest::Details { command_id: 1 })
            .iter()
            .map(describe)
            .collect();
        assert_eq!(queue, ["GetRows", "Details"]);

        let single: Vec<String> = drain_requests(&rx, update(7))
            .iter()
            .map(describe)
            .collect();
        assert_eq!(single, ["UpdateQuery(7)"]);
    }

    #[test]
    fn tc_b_17_the_timing_line_has_exactly_the_specified_keys_and_no_strings() {
        let timings = CmdQueryTimings {
            filter_ms: 0.41234,
            rank: CmdRankTimings {
                heuristic_ms: 0.2011,
                ..CmdRankTimings::default()
            },
            total_ms: 0.6549,
            candidates: 50_000,
            matched: 1840,
            query_len: 2,
            narrowed: true,
            reused: false,
        };
        let json: serde_json::Value = serde_json::from_str(&timings.to_json()).unwrap();
        let object = json.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "blend_ms",
                "candidates",
                "features_ms",
                "filter_ms",
                "heuristic_ms",
                "matched",
                "narrowed",
                "op",
                "predict_ms",
                "query_len",
                "reused",
                "stage2",
                "total_ms"
            ]
        );
        assert_eq!(object["op"], "cmd_query");
        assert!(object["features_ms"].is_null());
        assert_eq!(object["filter_ms"], 0.412);
        assert_eq!(object["total_ms"], 0.655);
        assert_eq!(object["narrowed"], true);
        assert_eq!(object["candidates"], 50_000);
        // Only the op name is a string.
        let strings: Vec<&String> = object
            .iter()
            .filter(|(key, value)| *key != "op" && value.is_string())
            .map(|(key, _)| key)
            .collect();
        assert!(strings.is_empty(), "{strings:?}");
    }

    // --- IT-B-02: the thread end to end ------------------------------------------

    enum Event {
        Response(CmdResponse),
    }

    impl From<CmdResponse> for Event {
        fn from(response: CmdResponse) -> Self {
            Event::Response(response)
        }
    }

    fn next(rx: &Receiver<Event>) -> CmdResponse {
        let Event::Response(response) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        response
    }

    fn texts(rows: &[CmdRow]) -> Vec<String> {
        rows.iter().map(|r| r.text.to_string()).collect()
    }

    fn hook_run<'a>(text: &'a str, at: i64, cwd: &'a str) -> NewRun<'a> {
        NewRun {
            text,
            template: text,
            started_at: at,
            cwd: Some(cwd),
            repo_root: None,
            exit_code: Some(0),
            duration_ms: None,
            shell_session: "s1",
            source: Source::Hook,
        }
    }

    #[test]
    fn it_b_02_the_worker_loads_answers_and_deletes() {
        let dir =
            std::env::temp_dir().join(format!("psychic-cmd-worker-it-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let store = CmdStore::open(&dir).unwrap();
            store
                .record_run(&hook_run("git status", NOW - 3000, "/a"))
                .unwrap();
            store.record_run(&hook_run("ls", NOW - 200, "/b")).unwrap();
            store
                .record_run(&hook_run("git log", NOW - 100, "/a"))
                .unwrap();
            store
                .record_run(&hook_run("git log", NOW - 50, "/b"))
                .unwrap();
        }
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let params = LoadParams {
            now: NOW,
            cwd: Some("/a".to_string()),
            repo_root: None,
            shell_session: Some("s1".to_string()),
        };
        let (tx, handle) = spawn(&dir, params, event_tx);

        let CmdResponse::Loaded { commands, .. } = next(&event_rx) else {
            panic!("Loaded comes first");
        };
        assert_eq!(commands, 3);

        tx.send(CmdRequest::UpdateQuery {
            query: String::new(),
            query_id: 1,
            scope: Scope::All,
        })
        .unwrap();
        let CmdResponse::QueryUpdated {
            query_id,
            rows,
            total_matched,
            total_commands,
            ..
        } = next(&event_rx)
        else {
            panic!("QueryUpdated");
        };
        assert_eq!((query_id, total_matched, total_commands), (1, 3, 3));
        assert_eq!(texts(&rows), ["git log", "ls", "git status"]);
        let here: Vec<u32> = rows.iter().map(|r| r.runs_here).collect();
        assert_eq!(here, [1, 0, 1]);

        tx.send(CmdRequest::UpdateQuery {
            query: "g".to_string(),
            query_id: 2,
            scope: Scope::All,
        })
        .unwrap();
        let CmdResponse::QueryUpdated {
            query_id,
            query,
            rows,
            ..
        } = next(&event_rx)
        else {
            panic!("QueryUpdated");
        };
        assert_eq!((query_id, query.as_str()), (2, "g"));
        assert_eq!(texts(&rows), ["git log", "git status"]);
        let git_log = rows[0].command_id;

        tx.send(CmdRequest::Details {
            command_id: git_log,
        })
        .unwrap();
        let CmdResponse::Details(details) = next(&event_rx) else {
            panic!("Details");
        };
        let dirs: Vec<(&str, u32)> = details
            .dirs
            .iter()
            .map(|d| (d.dir.as_str(), d.runs))
            .collect();
        assert_eq!(dirs, [("/b", 1), ("/a", 1)]);
        let before: Vec<(&str, u32)> = details
            .preceded_by
            .iter()
            .map(|p| (p.template.as_str(), p.runs))
            .collect();
        assert_eq!(before, [("git log", 1), ("ls", 1)]);

        tx.send(CmdRequest::Delete {
            command_id: git_log,
            query_id: 3,
        })
        .unwrap();
        let CmdResponse::QueryUpdated {
            query_id,
            query,
            rows,
            total_commands,
            ..
        } = next(&event_rx)
        else {
            panic!("QueryUpdated after Delete");
        };
        assert_eq!((query_id, query.as_str(), total_commands), (3, "g", 2));
        assert_eq!(texts(&rows), ["git status"]);

        tx.send(CmdRequest::Shutdown).unwrap();
        handle.join().expect("the worker exits cleanly");

        let store = CmdStore::open(&dir).unwrap();
        assert_eq!(command_id(&store, "git log"), None);
        let gone = |table: &str| {
            crate::cmd::store::test_support::query_strings(
                &store,
                &format!("SELECT COUNT(*) FROM {table} WHERE command_id = {git_log}"),
            )
        };
        assert_eq!(gone("command_runs"), ["0"]);
        assert_eq!(gone("command_events"), ["0"]);
        assert_eq!(table_rows(&store, "commands"), 2);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn e_b_01_a_database_that_cannot_be_read_answers_empty() {
        // A file where the data directory should be: nothing can be created in it.
        let path =
            std::env::temp_dir().join(format!("psychic-cmd-worker-notadir-{}", std::process::id()));
        std::fs::write(&path, "x").unwrap();
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let params = LoadParams {
            now: NOW,
            cwd: None,
            repo_root: None,
            shell_session: None,
        };
        let (tx, handle) = spawn(&path, params, event_tx);
        let CmdResponse::LoadFailed { message } = next(&event_rx) else {
            panic!("LoadFailed");
        };
        assert!(message.contains("could not create"), "{message}");
        tx.send(update(4)).unwrap();
        let CmdResponse::QueryUpdated {
            query_id,
            rows,
            total_commands,
            ..
        } = next(&event_rx)
        else {
            panic!("an empty QueryUpdated");
        };
        assert_eq!((query_id, rows.len(), total_commands), (4, 0, 0));
        tx.send(CmdRequest::Shutdown).unwrap();
        handle.join().unwrap();
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn e_b_07_a_failed_delete_keeps_the_command_and_says_so() {
        let mut index = index_of(&["a", "b"]);
        index.record_run(0, NOW - 1, None, None, Some(0), None);
        let mut worker = Worker::new(index, context(NOW), None, SkimMatcherV2::default());
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        worker.current_query = String::new();
        assert!(worker.handle(
            CmdRequest::Delete {
                command_id: 1,
                query_id: 9
            },
            &event_tx
        ));
        assert!(matches!(next(&event_rx), CmdResponse::DeleteFailed));
        let CmdResponse::QueryUpdated { query_id, rows, .. } = next(&event_rx) else {
            panic!("QueryUpdated");
        };
        assert_eq!(query_id, 9);
        assert_eq!(rows.len(), 2);
        assert!(!worker.index.entries[0].deleted);
    }

    #[test]
    fn the_empty_query_is_ranked_once_per_scope_until_a_delete() {
        let names: Vec<String> = (0..CMD_PAGE_ROWS + 30).map(|i| format!("c{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut index = index_of(&refs);
        for i in 0..refs.len() as u32 {
            index.record_run(i, NOW - i64::from(i) * 7, None, None, Some(0), None);
        }
        let mut worker = Worker::new(index, context(NOW), None, SkimMatcherV2::default());
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let ask = |worker: &mut Worker, query: &str, query_id: u64| {
            worker.handle(
                CmdRequest::UpdateQuery {
                    query: query.to_string(),
                    query_id,
                    scope: Scope::All,
                },
                &event_tx,
            );
            let CmdResponse::QueryUpdated { rows, .. } = next(&event_rx) else {
                panic!("QueryUpdated");
            };
            texts(&rows)
        };
        let first = ask(&mut worker, "", 1);
        let cached = Arc::clone(&worker.current);
        ask(&mut worker, "c1", 2);
        assert_eq!(ask(&mut worker, "", 3), first);
        assert!(Arc::ptr_eq(&worker.current, &cached), "not ranked again");
        let before = Arc::as_ptr(&cached);
        drop(cached);

        // Paging past the sorted head sorts a copy and caches it.
        worker.handle(
            CmdRequest::GetRows {
                query_id: 3,
                start: CMD_PAGE_ROWS,
            },
            &event_tx,
        );
        let CmdResponse::Rows { rows, .. } = next(&event_rx) else {
            panic!("Rows");
        };
        let expected: Vec<String> = (CMD_PAGE_ROWS..CMD_PAGE_ROWS + 30)
            .map(|i| format!("c{i}"))
            .collect();
        assert_eq!(texts(&rows), expected);
        assert_eq!(worker.empty_rankings[0].1.sorted, CMD_PAGE_ROWS + 30);
        assert!(Arc::ptr_eq(&worker.empty_rankings[0].1, &worker.current));
        assert_eq!(
            Arc::as_ptr(&worker.current),
            before,
            "sorted in place, not copied"
        );

        worker.forget_narrowing();
        assert!(worker.empty_rankings.is_empty(), "a delete forgets it");
    }

    #[test]
    fn e_b_08_details_that_cannot_be_read_are_empty() {
        let store = CmdStore::in_memory();
        crate::cmd::store::test_support::execute(&store, "DROP TABLE command_runs");
        let worker = Worker::new(
            index_of(&["a"]),
            context(NOW),
            Some(store),
            SkimMatcherV2::default(),
        );
        let details = worker.details(1);
        assert_eq!(
            (
                details.command_id,
                details.dirs.len(),
                details.preceded_by.len()
            ),
            (1, 0, 0)
        );
    }

    #[test]
    fn e_b_14_any_unicode_query_is_matched_or_not_without_failing() {
        let index = index_of(&["echo 日本語", "ls 🦀", "plain"]);
        let ctx = context(NOW);
        let matcher = SkimMatcherV2::default();
        for query in ["日本", "🦀", "\u{0}", "é", "\x1b[", "ﬁ"] {
            let found = filter_candidates(&index, &ctx, &matcher, query, Scope::All, None);
            assert!(found.len() <= 3, "{query:?}");
        }
        assert_eq!(
            texts_of(
                &index,
                &filter_candidates(&index, &ctx, &matcher, "日本", Scope::All, None)
            ),
            ["echo 日本語"]
        );
    }

    #[test]
    fn get_rows_pages_through_the_current_ranking_and_ignores_stale_ids() {
        let texts: Vec<String> = (0..CMD_PAGE_ROWS + 30).map(|i| format!("c{i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let index = index_of(&refs);
        let mut worker = Worker::new(index, context(NOW), None, SkimMatcherV2::default());
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        worker.handle(update(5), &event_tx);
        let _ = next(&event_rx);
        worker.handle(
            CmdRequest::GetRows {
                query_id: 4,
                start: CMD_PAGE_ROWS,
            },
            &event_tx,
        );
        worker.handle(
            CmdRequest::GetRows {
                query_id: 5,
                start: CMD_PAGE_ROWS,
            },
            &event_tx,
        );
        let CmdResponse::Rows {
            query_id,
            start,
            rows,
        } = next(&event_rx)
        else {
            panic!("Rows");
        };
        // The stale request (id 4) produced nothing; this is id 5's answer.
        assert_eq!((query_id, start, rows.len()), (5, CMD_PAGE_ROWS, 0));
        // Query "5" matches only some; ask with the empty query instead.
        worker.handle(
            CmdRequest::UpdateQuery {
                query: String::new(),
                query_id: 6,
                scope: Scope::All,
            },
            &event_tx,
        );
        let CmdResponse::QueryUpdated {
            rows,
            total_matched,
            ..
        } = next(&event_rx)
        else {
            panic!("QueryUpdated");
        };
        assert_eq!(
            (rows.len(), total_matched),
            (CMD_PAGE_ROWS, CMD_PAGE_ROWS + 30)
        );
        worker.handle(
            CmdRequest::GetRows {
                query_id: 6,
                start: CMD_PAGE_ROWS,
            },
            &event_tx,
        );
        let CmdResponse::Rows { rows, .. } = next(&event_rx) else {
            panic!("Rows");
        };
        assert_eq!(rows.len(), 30);
    }
}
