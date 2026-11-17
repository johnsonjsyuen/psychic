//! `CommandIndex`: every statistic ranking reads, in memory
//! (`docs/ctrl-r/02-browse.md` section 5).
//!
//! One type, two fill paths, one read path (ADR-12): [`CommandIndex::load`]
//! fills it from the SQL aggregates at picker start, and the mutators fill it
//! by replaying history (training and the benchmark, Phase C). The heuristic
//! and the features read it only through the accessors, so both fill paths
//! are seen the same way.
//!
//! Positions in `entries` are called `idx` (`u32`) everywhere; database ids
//! are `db_id` (`i64`). No accessor touches the database or allocates.

use super::fuzzy;
use super::store::{CmdStore, CountRow, CwdRunsRow, FollowerRow, IdRange, PickRow, RecentRunRow};
use super::template;
use super::tuning::{CMD_EVENT_WINDOW_SECS, CMD_LOAD_READERS};
use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// Strings (directories, repository roots, templates) as small ids.
#[derive(Default)]
pub(super) struct Interner {
    ids: FxHashMap<Box<str>, u32>,
    names: Vec<Box<str>>,
}

impl Interner {
    /// The id of `s`, adding it if it is new.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = u32::try_from(self.names.len()).expect("fewer than 2^32 distinct strings");
        self.names.push(s.into());
        self.ids.insert(s.into(), id);
        id
    }

    /// The id of `s`, if it was ever interned.
    pub fn get(&self, s: &str) -> Option<u32> {
        self.ids.get(s).copied()
    }

    /// The string behind `id`.
    #[allow(dead_code, reason = "tests; the benchmark (Phase D)")]
    pub fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }
}

/// One distinct command text and its aggregates.
pub(super) struct CommandEntry {
    pub db_id: i64,
    pub text: Arc<str>,
    /// `text.to_lowercase()`, computed once.
    pub text_lower: Box<str>,
    /// `text.chars().count()`.
    pub chars: u32,
    /// `template::word_count(text)`.
    pub words: u32,
    /// `fuzzy::text_classes(text)`: which bytes the text holds, for the
    /// filter's prefilter.
    pub classes: u64,
    pub last_run_at: i64,
    pub run_count: u32,
    pub success_count: u32,
    pub fail_count: u32,
    /// All-time Ctrl-R picks; display only, no feature reads it.
    pub pick_count: u32,
    pub last_exit_code: Option<i32>,
    /// Newest run with a non-zero exit code; 0 = none known.
    pub last_fail_at: i64,
    /// Ascending; every run newer than `CMD_EVENT_WINDOW_SECS` is present.
    pub run_times: Vec<i64>,
    pub deleted: bool,
}

impl CommandEntry {
    /// A command with zero counts.
    fn new(db_id: i64, text: &str) -> Self {
        CommandEntry {
            db_id,
            text: Arc::from(text),
            text_lower: text.to_lowercase().into_boxed_str(),
            chars: text.chars().count() as u32,
            words: template::word_count(text) as u32,
            classes: fuzzy::text_classes(text),
            last_run_at: 0,
            run_count: 0,
            success_count: 0,
            fail_count: 0,
            pick_count: 0,
            last_exit_code: None,
            last_fail_at: 0,
            run_times: Vec::new(),
            deleted: false,
        }
    }
}

/// `ids` cut into at most `parts` ranges of about equal width, in order.
fn split_ids(ids: IdRange, parts: usize) -> Vec<IdRange> {
    assert!(
        parts > 0 && ids.first <= ids.last,
        "a non-empty id range and a part"
    );
    let width = (ids.last as i128 - ids.first as i128 + 1) as u128;
    let step = width.div_ceil(parts as u128);
    let mut ranges = Vec::with_capacity(parts);
    let mut first = ids.first as i128;
    while first <= ids.last as i128 {
        let last = (first + step as i128 - 1).min(ids.last as i128);
        ranges.push(IdRange {
            first: first as i64,
            last: last as i64,
        });
        first = last + 1;
    }
    ranges
}

/// R1 over `range`, as entries.
fn read_entries(store: &CmdStore, range: IdRange) -> Result<Vec<CommandEntry>> {
    Ok(store
        .load_commands_in(range)?
        .into_iter()
        .map(|row| {
            let mut entry = CommandEntry::new(row.id, &row.text);
            entry.last_run_at = row.last_run_at;
            entry.run_count = row.run_count;
            entry.success_count = row.success_count;
            entry.fail_count = row.fail_count;
            entry.pick_count = row.pick_count;
            entry.last_exit_code = row.last_exit_code;
            entry
        })
        .collect())
}

/// What R2-R8 return, read before the index exists.
#[derive(Default)]
struct ContextRows {
    cwd_runs: Vec<CwdRunsRow>,
    repo_runs: Vec<CountRow>,
    recent_runs: Vec<RecentRunRow>,
    session_tail: Vec<String>,
    followers: Vec<FollowerRow>,
    session_commands: Vec<i64>,
    picks: Vec<PickRow>,
}

impl ContextRows {
    /// R2-R8, in order, skipping what the context does not call for.
    fn read(store: &CmdStore, p: &LoadParams) -> Result<Self> {
        let mut rows = ContextRows::default();
        if let Some(cwd) = p.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
            rows.cwd_runs = store.runs_in_cwd(cwd)?;
        }
        if let Some(repo_root) = p.repo_root.as_deref() {
            rows.repo_runs = store.runs_in_repo(repo_root)?;
        }
        rows.recent_runs = store.runs_since(p.now - CMD_EVENT_WINDOW_SECS)?;
        if let Some(session) = p.shell_session.as_deref() {
            rows.session_tail = store.session_tail(session)?;
            // R6 takes each template once.
            let mut distinct: Vec<String> = Vec::with_capacity(rows.session_tail.len());
            for template in &rows.session_tail {
                if !distinct.contains(template) {
                    distinct.push(template.clone());
                }
            }
            if !distinct.is_empty() {
                rows.followers = store.followers(&distinct)?;
            }
            rows.session_commands = store.session_commands(session)?;
        }
        rows.picks = store.picks_since(p.now - CMD_EVENT_WINDOW_SECS)?;
        Ok(rows)
    }
}

/// Runs of one command in one directory.
pub(super) struct DirStat {
    pub runs: u32,
    pub last_run_at: i64,
}

/// Pick times of each command, ascending.
type PickTimes = FxHashMap<u32, Vec<i64>>;

/// Every statistic ranking reads.
#[derive(Default)]
pub(super) struct CommandIndex {
    /// Position = idx.
    pub entries: Vec<CommandEntry>,
    /// Position = idx: the entry's `classes`, plus `fuzzy::LIVE` while it is
    /// not deleted. The filter's first test reads only this array.
    filter_classes: Vec<u64>,
    by_db_id: FxHashMap<i64, u32>,
    pub dirs: Interner,
    pub repos: Interner,
    pub templates: Interner,
    /// (idx, dir id).
    dir_stats: FxHashMap<(u32, u32), DirStat>,
    /// (idx, repo id) -> runs.
    repo_runs: FxHashMap<(u32, u32), u32>,
    /// (idx, template id of the previous command) -> runs.
    followers: FxHashMap<(u32, u32), u32>,
    picks: PickTimes,
    picks_by_query: FxHashMap<Box<str>, PickTimes>,
    episode_picks_by_query: FxHashMap<Box<str>, PickTimes>,
    /// Serving only: commands run in the picker's `--session`.
    pub session_commands: FxHashSet<u32>,
}

/// The moment and place being ranked for.
pub(super) struct RankContext {
    pub now: i64,
    /// Id in `dirs` of the cwd; `None` = unknown.
    pub dir: Option<u32>,
    /// Id in `repos` of the repository root; `None` = not in a repository.
    pub repo: Option<u32>,
    /// `[0]` = template id of the most recent command in the shell session.
    pub prev_templates: [Option<u32>; 3],
}

/// What `load` ranks for: the picker's start time and place.
pub(super) struct LoadParams {
    pub now: i64,
    pub cwd: Option<String>,
    pub repo_root: Option<String>,
    pub shell_session: Option<String>,
}

/// Insert `at` keeping `times` ascending.
fn insert_sorted(times: &mut Vec<i64>, at: i64) {
    let position = times.partition_point(|&t| t <= at);
    times.insert(position, at);
}

/// How many of the ascending `times` fall in `(now - secs, now]`.
fn count_in_window(times: &[i64], now: i64, secs: i64) -> u32 {
    assert!(
        (0..=CMD_EVENT_WINDOW_SECS).contains(&secs),
        "only the last {CMD_EVENT_WINDOW_SECS} s of runs and picks are loaded, asked for {secs}"
    );
    let from = times.partition_point(|&t| t <= now - secs);
    let to = times.partition_point(|&t| t <= now);
    (to.saturating_sub(from)) as u32
}

impl CommandIndex {
    // --- Mutators (the only writers) -------------------------------------------

    /// Add a command with zero counts; returns its idx.
    pub(super) fn add_command(&mut self, db_id: i64, text: &str) -> u32 {
        self.push_entry(CommandEntry::new(db_id, text))
    }

    fn push_entry(&mut self, entry: CommandEntry) -> u32 {
        let idx = u32::try_from(self.entries.len()).expect("fewer than 2^32 commands");
        let previous = self.by_db_id.insert(entry.db_id, idx);
        assert!(previous.is_none(), "command {} added twice", entry.db_id);
        self.filter_classes.push(entry.classes | fuzzy::LIVE);
        self.entries.push(entry);
        idx
    }

    /// One execution of `idx`.
    pub(super) fn record_run(
        &mut self,
        idx: u32,
        at: i64,
        dir: Option<u32>,
        repo: Option<u32>,
        exit: Option<i32>,
        prev_template: Option<u32>,
    ) {
        let entry = &mut self.entries[idx as usize];
        entry.run_count += 1;
        match exit {
            Some(0) => entry.success_count += 1,
            Some(_) => entry.fail_count += 1,
            None => {}
        }
        if at >= entry.last_run_at {
            entry.last_exit_code = exit;
        }
        entry.last_run_at = entry.last_run_at.max(at);
        if matches!(exit, Some(code) if code != 0) {
            entry.last_fail_at = entry.last_fail_at.max(at);
        }
        insert_sorted(&mut entry.run_times, at);
        if let Some(dir) = dir {
            let stat = self.dir_stats.entry((idx, dir)).or_insert(DirStat {
                runs: 0,
                last_run_at: at,
            });
            stat.runs += 1;
            stat.last_run_at = stat.last_run_at.max(at);
        }
        if let Some(repo) = repo {
            *self.repo_runs.entry((idx, repo)).or_insert(0) += 1;
        }
        if let Some(template) = prev_template {
            *self.followers.entry((idx, template)).or_insert(0) += 1;
        }
    }

    /// One Ctrl-R pick of `idx` at `at` with `query`, after the episode's
    /// `episode_queries`.
    pub(super) fn record_pick(
        &mut self,
        idx: u32,
        at: i64,
        query: &str,
        episode_queries: &[String],
    ) {
        assert!(
            (idx as usize) < self.entries.len(),
            "pick of an unknown command"
        );
        insert_sorted(self.picks.entry(idx).or_default(), at);
        insert_sorted(
            self.picks_by_query
                .entry(query.into())
                .or_default()
                .entry(idx)
                .or_default(),
            at,
        );
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        for episode_query in episode_queries {
            if seen.insert(episode_query) {
                insert_sorted(
                    self.episode_picks_by_query
                        .entry(episode_query.as_str().into())
                        .or_default()
                        .entry(idx)
                        .or_default(),
                    at,
                );
            }
        }
    }

    /// Hide `idx` from every later filter.
    pub(super) fn remove(&mut self, idx: u32) {
        self.entries[idx as usize].deleted = true;
        self.filter_classes[idx as usize] &= !fuzzy::LIVE;
    }

    // --- Load (picker start) ---------------------------------------------------

    /// R1-R8 of `01-record.md` section 6.2, each once. R1, the one full
    /// read, is split by id over `CMD_LOAD_READERS` connections, each on a
    /// thread that also builds its entries; R2-R8 run, in order, on one more
    /// connection at the same time. (An in-memory store, which another
    /// connection would see empty, reads everything on `store`.) A row whose
    /// command is not in R1 (recorded or deleted meanwhile) is skipped.
    pub(super) fn load(store: &CmdStore, p: &LoadParams) -> Result<(CommandIndex, RankContext)> {
        let file = store.file();
        let readers = if file.is_some() { CMD_LOAD_READERS } else { 1 };
        let ranges = match store.command_ids()? {
            Some(ids) => split_ids(ids, readers),
            None => Vec::new(),
        };
        let (parts, context) = std::thread::scope(|scope| {
            let file = file.as_deref();
            let open = move || -> Result<CmdStore> {
                let side = CmdStore::open_file(file.expect("only with a file"))?;
                side.map_for_reading()?;
                Ok(side)
            };
            let context = file.map(|_| scope.spawn(move || ContextRows::read(&open()?, p)));
            let others: Vec<_> = ranges
                .iter()
                .skip(1)
                .map(|&range| scope.spawn(move || read_entries(&open()?, range)))
                .collect();
            let mut parts = vec![store.map_for_reading().and_then(|()| match ranges.first() {
                Some(&range) => read_entries(store, range),
                None => Ok(Vec::new()),
            })];
            parts.extend(
                others
                    .into_iter()
                    .map(|reader| reader.join().expect("a reader does not panic")),
            );
            let context = match context {
                Some(reader) => reader.join().expect("a reader does not panic"),
                None => ContextRows::read(store, p),
            };
            (parts, context)
        });

        let mut index = CommandIndex::default();
        let total = parts
            .iter()
            .map(|part| part.as_ref().map_or(0, Vec::len))
            .sum();
        index.entries.reserve(total);
        index.filter_classes.reserve(total);
        index.by_db_id.reserve(total);
        for part in parts {
            for entry in part? {
                index.push_entry(entry);
            }
        }
        let ctx = index.apply(context?, p);
        Ok((index, ctx))
    }

    /// Fill everything but the commands themselves from R2-R8.
    fn apply(&mut self, rows: ContextRows, p: &LoadParams) -> RankContext {
        let mut ctx = RankContext {
            now: p.now,
            dir: None,
            repo: None,
            prev_templates: [None; 3],
        };
        if let Some(cwd) = p.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
            let dir = self.dirs.intern(cwd);
            ctx.dir = Some(dir);
            for row in rows.cwd_runs {
                if let Some(idx) = self.idx_of(row.command_id) {
                    let stat = DirStat {
                        runs: row.runs,
                        last_run_at: row.last_run_at,
                    };
                    self.dir_stats.insert((idx, dir), stat);
                }
            }
        }
        if let Some(repo_root) = p.repo_root.as_deref() {
            let repo = self.repos.intern(repo_root);
            ctx.repo = Some(repo);
            for row in rows.repo_runs {
                if let Some(idx) = self.idx_of(row.command_id) {
                    self.repo_runs.insert((idx, repo), row.runs);
                }
            }
        }
        // Rows arrive in ascending time, so a push keeps `run_times` sorted.
        for row in rows.recent_runs {
            if let Some(idx) = self.idx_of(row.command_id) {
                let entry = &mut self.entries[idx as usize];
                entry.run_times.push(row.started_at);
                if matches!(row.exit_code, Some(code) if code != 0) {
                    entry.last_fail_at = entry.last_fail_at.max(row.started_at);
                }
            }
        }
        for (slot, template) in ctx.prev_templates.iter_mut().zip(&rows.session_tail) {
            *slot = Some(self.templates.intern(template));
        }
        for row in rows.followers {
            if let Some(idx) = self.idx_of(row.command_id) {
                let template = self.templates.intern(&row.template);
                self.followers.insert((idx, template), row.runs);
            }
        }
        for command_id in rows.session_commands {
            if let Some(idx) = self.idx_of(command_id) {
                self.session_commands.insert(idx);
            }
        }
        for pick in rows.picks {
            if let Some(idx) = self.idx_of(pick.command_id) {
                self.record_pick(idx, pick.timestamp, &pick.query, &pick.episode_queries);
            }
        }
        ctx
    }

    // --- Accessors (the only readers) --------------------------------------------

    /// Per idx: the byte classes of the text, with `fuzzy::LIVE` unless
    /// deleted.
    pub(super) fn filter_classes(&self) -> &[u64] {
        &self.filter_classes
    }

    /// The idx of a database id.
    pub(super) fn idx_of(&self, db_id: i64) -> Option<u32> {
        self.by_db_id.get(&db_id).copied()
    }

    /// Runs of `idx` in `dir`; `None` when `dir` is `None` or it never ran there.
    pub(super) fn dir_stat(&self, idx: u32, dir: Option<u32>) -> Option<&DirStat> {
        self.dir_stats.get(&(idx, dir?))
    }

    /// Runs of `idx` in `repo`; 0 when `repo` is `None` or it never ran there.
    pub(super) fn runs_in_repo(&self, idx: u32, repo: Option<u32>) -> u32 {
        repo.and_then(|repo| self.repo_runs.get(&(idx, repo)).copied())
            .unwrap_or(0)
    }

    /// Runs of `idx` right after a command with `template`.
    pub(super) fn follower_runs(&self, idx: u32, template: Option<u32>) -> u32 {
        template
            .and_then(|template| self.followers.get(&(idx, template)).copied())
            .unwrap_or(0)
    }

    /// Runs of `idx` in `(now - secs, now]`.
    pub(super) fn runs_in_window(&self, idx: u32, now: i64, secs: i64) -> u32 {
        count_in_window(&self.entries[idx as usize].run_times, now, secs)
    }

    /// Picks of `idx` in `(now - secs, now]`.
    pub(super) fn picks_in_window(&self, idx: u32, now: i64, secs: i64) -> u32 {
        self.picks
            .get(&idx)
            .map_or(0, |times| count_in_window(times, now, secs))
    }

    /// Picks of `idx` with exactly `query` in `(now - secs, now]`.
    pub(super) fn picks_for_query(&self, query: &str, idx: u32, now: i64, secs: i64) -> u32 {
        Self::query_picks(&self.picks_by_query, query, idx, now, secs)
    }

    /// Picks of `idx` in an episode that had `query` among its queries.
    pub(super) fn episode_picks_for_query(
        &self,
        query: &str,
        idx: u32,
        now: i64,
        secs: i64,
    ) -> u32 {
        Self::query_picks(&self.episode_picks_by_query, query, idx, now, secs)
    }

    fn query_picks(
        map: &FxHashMap<Box<str>, PickTimes>,
        query: &str,
        idx: u32,
        now: i64,
        secs: i64,
    ) -> u32 {
        map.get(query)
            .and_then(|by_idx| by_idx.get(&idx))
            .map_or(0, |times| count_in_window(times, now, secs))
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// An index with one command per text, never run.
    pub(in crate::cmd) fn index_of(texts: &[&str]) -> CommandIndex {
        let mut index = CommandIndex::default();
        for (i, text) in texts.iter().enumerate() {
            index.add_command(i as i64 + 1, text);
        }
        index
    }

    /// A context at `now` with no place and no session.
    pub(in crate::cmd) fn context(now: i64) -> RankContext {
        RankContext {
            now,
            dir: None,
            repo: None,
            prev_templates: [None; 3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::cmd::store::{EventAction, NewEvent, NewRun, Source};

    const NOW: i64 = 1_000_000;

    #[test]
    fn tc_b_15_windows_count_runs_in_the_half_open_interval() {
        let mut index = index_of(&["a"]);
        for at in [NOW - 4000, NOW - 3000, NOW - 10] {
            index.record_run(0, at, None, None, Some(0), None);
        }
        assert_eq!(index.runs_in_window(0, NOW, 3600), 2);
        assert_eq!(index.runs_in_window(0, NOW, 60), 1);
        assert_eq!(index.runs_in_window(0, NOW, 5), 0);
        // Exactly at `now - secs` is out, at `now` is in.
        assert_eq!(index.runs_in_window(0, NOW - 10, 2990), 1);
        assert_eq!(index.runs_in_window(0, NOW - 3000, 1000), 1);
    }

    #[test]
    #[should_panic(expected = "only the last")]
    fn tc_b_15_a_window_longer_than_what_is_loaded_is_a_bug() {
        let index = index_of(&["a"]);
        index.runs_in_window(0, NOW, CMD_EVENT_WINDOW_SECS + 1);
    }

    #[test]
    fn tc_b_16_out_of_order_runs_keep_the_newest_exit_code() {
        let mut index = index_of(&["a"]);
        index.record_run(0, 300, None, None, Some(0), None);
        index.record_run(0, 100, None, None, Some(1), None);
        index.record_run(0, 200, None, None, None, None);
        let entry = &index.entries[0];
        assert_eq!(entry.run_times, vec![100, 200, 300]);
        assert_eq!(entry.last_exit_code, Some(0));
        assert_eq!(entry.last_fail_at, 100);
        assert_eq!(entry.last_run_at, 300);
        assert_eq!(
            (entry.run_count, entry.success_count, entry.fail_count),
            (3, 1, 1)
        );
    }

    #[test]
    fn add_command_fills_the_derived_text_fields() {
        let index = index_of(&["Git Commit -m 'ä b' | wc"]);
        let entry = &index.entries[0];
        assert_eq!(&*entry.text_lower, "git commit -m 'ä b' | wc");
        assert_eq!(entry.chars, 24);
        assert_eq!(entry.words, 5);
        assert_eq!(index.idx_of(1), Some(0));
        assert_eq!(index.idx_of(2), None);
    }

    #[test]
    fn id_ranges_are_cut_into_contiguous_parts() {
        let range = |first, last| IdRange { first, last };
        assert_eq!(
            split_ids(range(1, 10), 4),
            [range(1, 3), range(4, 6), range(7, 9), range(10, 10)]
        );
        assert_eq!(split_ids(range(5, 6), 4), [range(5, 5), range(6, 6)]);
        assert_eq!(split_ids(range(7, 7), 1), [range(7, 7)]);
        let wide = split_ids(range(i64::MIN, i64::MAX), 4);
        assert_eq!(wide.len(), 4);
        assert_eq!((wide[0].first, wide[3].last), (i64::MIN, i64::MAX));
        assert!(wide.windows(2).all(|w| w[0].last + 1 == w[1].first));
    }

    #[test]
    #[should_panic(expected = "added twice")]
    fn a_database_id_is_added_once() {
        let mut index = index_of(&["a"]);
        index.add_command(1, "b");
    }

    #[test]
    fn picks_count_per_query_and_per_episode_query_once() {
        let mut index = index_of(&["a", "b"]);
        let episode = ["g".to_string(), "gi".to_string(), "g".to_string()];
        index.record_pick(1, NOW - 100, "gi", &episode);
        index.record_pick(1, NOW - 50, "g", &["g".to_string()]);
        assert_eq!(index.picks_in_window(1, NOW, 3600), 2);
        assert_eq!(index.picks_in_window(0, NOW, 3600), 0);
        assert_eq!(index.picks_for_query("gi", 1, NOW, 3600), 1);
        assert_eq!(index.picks_for_query("g", 1, NOW, 3600), 1);
        assert_eq!(index.episode_picks_for_query("g", 1, NOW, 3600), 2);
        assert_eq!(index.episode_picks_for_query("gi", 1, NOW, 3600), 1);
        assert_eq!(index.episode_picks_for_query("x", 1, NOW, 3600), 0);
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "psychic-cmd-index-{}-{}",
                tag,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn run<'a>(
        text: &'a str,
        at: i64,
        cwd: &'a str,
        repo: Option<&'a str>,
        session: &'a str,
        exit_code: Option<i32>,
    ) -> NewRun<'a> {
        NewRun {
            text,
            template: text,
            started_at: at,
            cwd: Some(cwd),
            repo_root: repo,
            exit_code,
            duration_ms: None,
            shell_session: session,
            source: Source::Hook,
        }
    }

    /// IT-B-01: 6 runs over 3 commands, 2 dirs, 1 repo, 2 sessions, 1 pick.
    #[test]
    fn it_b_01_load_fills_every_statistic_from_the_database() {
        let dir = TempDir::new("load");
        let store = CmdStore::open(&dir.0).unwrap();
        let runs = [
            run("make", NOW - 500, "/r/a", Some("/r"), "s1", Some(0)),
            run("make test", NOW - 400, "/r/a", Some("/r"), "s1", Some(2)),
            run("make", NOW - 300, "/r/a", Some("/r"), "s1", Some(0)),
            run("ls", NOW - 250, "/home", None, "s2", Some(0)),
            run("make test", NOW - 200, "/r/a", Some("/r"), "s1", Some(0)),
            run("ls", NOW - 100, "/r/a", Some("/r"), "s1", Some(0)),
        ];
        for r in &runs {
            store.record_run(r).unwrap();
        }
        let id = |text| crate::cmd::store::test_support::command_id(&store, text).unwrap();
        store
            .log_events(
                NOW - 50,
                &[NewEvent {
                    action: EventAction::Pick,
                    command_id: id("make"),
                    query: "ma",
                    rank: Some(1),
                    session_id: "tui",
                    subsession_id: 2,
                    episode_queries: Some(&["m".to_string(), "ma".to_string()]),
                    cwd: Some("/r/a"),
                    repo_root: Some("/r"),
                    shell_session: Some("s1"),
                }],
            )
            .unwrap();

        let (index, ctx) = CommandIndex::load(
            &store,
            &LoadParams {
                now: NOW,
                cwd: Some("/r/a".to_string()),
                repo_root: Some("/r".to_string()),
                shell_session: Some("s1".to_string()),
            },
        )
        .unwrap();

        assert_eq!(index.entries.len(), 3);
        let idx = |text| index.idx_of(id(text)).unwrap();
        let (make, test, ls) = (idx("make"), idx("make test"), idx("ls"));
        assert_eq!(ctx.now, NOW);
        assert_eq!(index.dirs.name(ctx.dir.unwrap()), "/r/a");
        assert_eq!(index.repos.name(ctx.repo.unwrap()), "/r");

        let here = |i| index.dir_stat(i, ctx.dir).map(|d| (d.runs, d.last_run_at));
        assert_eq!(here(make), Some((2, NOW - 300)));
        assert_eq!(here(test), Some((2, NOW - 200)));
        assert_eq!(here(ls), Some((1, NOW - 100)));
        assert_eq!(index.dir_stat(make, None).map(|d| d.runs), None);
        assert_eq!(
            [make, test, ls].map(|i| index.runs_in_repo(i, ctx.repo)),
            [2, 2, 1]
        );
        assert_eq!(index.runs_in_repo(ls, None), 0);

        // The session's last three templates, newest first.
        let names: Vec<&str> = ctx
            .prev_templates
            .iter()
            .map(|t| index.templates.name(t.unwrap()))
            .collect();
        assert_eq!(names, ["ls", "make test", "make"]);
        // R6: in s1, `make test` followed `make` twice, `make` followed
        // `make test` once, `ls` followed `make test` once.
        let template = |name| index.templates.get(name);
        assert_eq!(index.follower_runs(test, template("make")), 2);
        assert_eq!(index.follower_runs(make, template("make test")), 1);
        assert_eq!(index.follower_runs(ls, template("make test")), 1);
        assert_eq!(index.follower_runs(ls, template("ls")), 0);
        assert_eq!(index.follower_runs(ls, None), 0);

        let session: FxHashSet<u32> = [make, test, ls].into_iter().collect();
        assert_eq!(index.session_commands, session);

        assert_eq!(index.picks_in_window(make, NOW, 3600), 1);
        assert_eq!(index.picks_for_query("ma", make, NOW, 3600), 1);
        assert_eq!(index.episode_picks_for_query("m", make, NOW, 3600), 1);
        assert_eq!(
            index.entries[make as usize].run_times,
            vec![NOW - 500, NOW - 300]
        );
        assert_eq!(index.entries[test as usize].last_fail_at, NOW - 400);
        assert_eq!(index.entries[test as usize].last_exit_code, Some(0));
        assert_eq!(index.entries[make as usize].run_count, 2);
    }

    #[test]
    fn e_b_10_and_e_b_11_an_unknown_session_and_cwd_leave_the_context_empty() {
        let store = CmdStore::in_memory();
        store
            .record_run(&run("ls", NOW - 5, "/x", None, "s1", Some(0)))
            .unwrap();
        let (index, ctx) = CommandIndex::load(
            &store,
            &LoadParams {
                now: NOW,
                cwd: None,
                repo_root: None,
                shell_session: Some("nobody".to_string()),
            },
        )
        .unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(
            (ctx.dir, ctx.repo, ctx.prev_templates),
            (None, None, [None; 3])
        );
        assert!(index.session_commands.is_empty());
    }
}
