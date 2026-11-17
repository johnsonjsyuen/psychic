//! Impression and pick logging (`docs/ctrl-r/02-browse.md` section 10).
//!
//! Mirrors file mode's `Analytics`: a query that has been on screen for
//! `CMD_IMPRESSION_AGE_MS` has its visible rows logged once as impressions;
//! Enter logs the pick, with every query typed on the way to it. Rows go to
//! `command_events` on the UI thread's own connection. A failure is logged
//! and otherwise ignored: it never stops the pick (E-B-06).

use super::store::{CmdStore, EventAction, LogBound, NewEvent};
use super::tuning::CMD_IMPRESSION_AGE_MS;
use anyhow::Result;
use rustc_hash::FxHashSet;
use std::time::{Duration, Instant};

/// A row the user saw: its command and 1-based rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Shown {
    pub command_id: i64,
    pub rank: usize,
}

/// One accepted query and what of it has been logged.
struct Subsession {
    id: u64,
    query: String,
    created_at: Instant,
    logged: bool,
    /// Command ids logged as impressions under this query.
    logged_ids: FxHashSet<i64>,
}

/// Where the picker runs; written on picks only.
pub(super) struct PickPlace {
    pub cwd: String,
    pub repo_root: Option<String>,
    pub shell_session: Option<String>,
}

pub(super) struct CmdEvents {
    session_id: String,
    place: PickPlace,
    /// `None` when the UI thread could not open it (E-B-19): nothing is logged.
    store: Option<CmdStore>,
    current: Option<Subsession>,
    /// Every distinct query of this episode, in the order first seen.
    episode_queries: Vec<String>,
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn rank(rank: usize) -> Option<u32> {
    Some(u32::try_from(rank).expect("a rank fits in u32"))
}

impl CmdEvents {
    pub fn new(session_id: String, place: PickPlace, store: Option<CmdStore>) -> Self {
        CmdEvents {
            session_id,
            place,
            store,
            current: None,
            episode_queries: Vec::new(),
        }
    }

    /// A `QueryUpdated` was accepted: its rows are what is on screen now.
    pub fn new_subsession(&mut self, query_id: u64, query: String) {
        self.current = Some(Subsession {
            id: query_id,
            query,
            created_at: Instant::now(),
            logged: false,
            logged_ids: FxHashSet::default(),
        });
    }

    /// Note the current query as part of this episode, and say whether its
    /// impressions should be logged now: not yet logged, and on screen for
    /// `CMD_IMPRESSION_AGE_MS` (or `force`).
    pub fn wants_impressions(&mut self, force: bool) -> bool {
        let Some(current) = &self.current else {
            return false;
        };
        if !self.episode_queries.contains(&current.query) {
            self.episode_queries.push(current.query.clone());
        }
        !current.logged
            && (force
                || current.created_at.elapsed() >= Duration::from_millis(CMD_IMPRESSION_AGE_MS))
    }

    /// D6 once more, on this (the UI thread's) connection, at the exit of a
    /// picker that deleted something: empty the write-ahead log without
    /// waiting, in case the delete's own checkpoint found it busy
    /// (01 section 10.5). A log still busy is only logged.
    pub fn truncate_log(&self) {
        let Some(store) = &self.store else {
            return;
        };
        match store.truncate_log_now() {
            Ok(LogBound::Truncated | LogBound::Within) => {}
            Ok(LogBound::Busy) => log::warn!(
                "another connection is using events.db-wal, so a deleted command's text \
                 can stay in it until the next checkpoint"
            ),
            Err(error) => log::warn!("{}", error),
        }
    }

    /// Log `shown` as impressions of the current query, in one transaction.
    pub fn log_impressions(&mut self, shown: &[Shown]) -> Result<()> {
        let Some(current) = self.current.as_mut() else {
            return Ok(());
        };
        if shown.is_empty() {
            return Ok(());
        }
        current.logged = true;
        current
            .logged_ids
            .extend(shown.iter().map(|s| s.command_id));
        let Some(store) = &self.store else {
            return Ok(());
        };
        let events: Vec<NewEvent> = shown
            .iter()
            .map(|s| impression(&self.session_id, current, *s))
            .collect();
        store.log_events(now(), &events)
    }

    /// Log the pick of `picked`: an impression first if it was never logged
    /// under this query (the user scrolled to it afterwards), then the pick
    /// with the episode's queries and the place, then the command's pick
    /// count. Call `wants_impressions(true)` and `log_impressions` first.
    pub fn log_pick(&mut self, picked: Shown) -> Result<()> {
        let Some(current) = self.current.as_mut() else {
            return Ok(());
        };
        let unseen = current.logged_ids.insert(picked.command_id);
        let episode = std::mem::take(&mut self.episode_queries);
        let Some(store) = &self.store else {
            return Ok(());
        };
        let mut events = Vec::with_capacity(2);
        if unseen {
            events.push(impression(&self.session_id, current, picked));
        }
        events.push(NewEvent {
            action: EventAction::Pick,
            command_id: picked.command_id,
            query: &current.query,
            rank: rank(picked.rank),
            session_id: &self.session_id,
            subsession_id: current.id,
            episode_queries: Some(&episode),
            cwd: Some(&self.place.cwd),
            repo_root: self.place.repo_root.as_deref(),
            shell_session: self.place.shell_session.as_deref(),
        });
        store.log_events(now(), &events)?;
        store.count_pick(picked.command_id)
    }
}

fn impression<'a>(session_id: &'a str, current: &'a Subsession, shown: Shown) -> NewEvent<'a> {
    NewEvent {
        action: EventAction::Impression,
        command_id: shown.command_id,
        query: &current.query,
        rank: rank(shown.rank),
        session_id,
        subsession_id: current.id,
        episode_queries: None,
        cwd: None,
        repo_root: None,
        shell_session: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::store::test_support::{query_strings, table_rows};
    use crate::cmd::store::{NewRun, Source};

    fn place() -> PickPlace {
        PickPlace {
            cwd: "/w".to_string(),
            repo_root: Some("/r".to_string()),
            shell_session: Some("s1".to_string()),
        }
    }

    fn age(events: &mut CmdEvents) {
        let current = events.current.as_mut().unwrap();
        current.created_at -= Duration::from_millis(CMD_IMPRESSION_AGE_MS);
    }

    #[test]
    fn tc_b_14_impressions_are_wanted_once_the_query_has_been_on_screen() {
        let mut events = CmdEvents::new("tui".to_string(), place(), None);
        assert!(!events.wants_impressions(false), "no subsession yet");
        events.new_subsession(2, "g".to_string());
        assert!(!events.wants_impressions(false));
        assert_eq!(events.episode_queries, ["g"], "noted even when not wanted");
        age(&mut events);
        assert!(events.wants_impressions(false));
        events
            .log_impressions(&[Shown {
                command_id: 1,
                rank: 1,
            }])
            .unwrap();
        assert!(!events.wants_impressions(false));
        assert!(!events.wants_impressions(true), "logged already");

        events.new_subsession(3, "gi".to_string());
        assert!(events.wants_impressions(true), "forced at once");
        assert_eq!(events.episode_queries, ["g", "gi"]);
    }

    /// IT-B-03.
    #[test]
    fn it_b_03_a_pick_is_logged_with_its_episode_and_counted() {
        let store = CmdStore::in_memory();
        let mut ids = Vec::new();
        for (i, text) in ["git a", "git b", "git c", "git d"].iter().enumerate() {
            let crate::cmd::store::RunOutcome::Inserted { command_id, .. } = store
                .record_run(&NewRun {
                    text,
                    template: text,
                    started_at: 100 + i as i64,
                    cwd: None,
                    repo_root: None,
                    exit_code: Some(0),
                    duration_ms: None,
                    shell_session: "s1",
                    source: Source::Hook,
                })
                .unwrap()
            else {
                panic!("inserted");
            };
            ids.push(command_id);
        }
        let shown: Vec<Shown> = ids[..3]
            .iter()
            .enumerate()
            .map(|(i, &command_id)| Shown {
                command_id,
                rank: i + 1,
            })
            .collect();
        let mut events = CmdEvents::new("tui".to_string(), place(), Some(store));
        events.new_subsession(2, "g".to_string());
        assert!(events.wants_impressions(true));
        events.log_impressions(&shown).unwrap();
        events.new_subsession(3, "gi".to_string());
        assert!(events.wants_impressions(true));
        events.log_impressions(&shown).unwrap();
        // The pick of rank 2 was seen under "gi": no extra impression.
        events.log_pick(shown[1]).unwrap();
        assert!(
            events.episode_queries.is_empty(),
            "the episode ends with the pick"
        );

        let store = events.store.as_ref().unwrap();
        let rows = query_strings(
            store,
            "SELECT action, command_id, query, rank, session_id, subsession_id, \
             episode_queries, cwd, repo_root, shell_session FROM command_events ORDER BY id",
        );
        let id = |i: usize| ids[i];
        let mut expected = Vec::new();
        for (subsession, query) in [(2, "g"), (3, "gi")] {
            for rank in 1..=3 {
                expected.push(format!(
                    "impression|{}|{query}|{rank}|tui|{subsession}|NULL|NULL|NULL|NULL",
                    id(rank - 1)
                ));
            }
        }
        expected.push(format!("pick|{}|gi|2|tui|3|[\"g\",\"gi\"]|/w|/r|s1", id(1)));
        assert_eq!(rows, expected);
        assert_eq!(
            query_strings(
                store,
                &format!("SELECT pick_count FROM commands WHERE id = {}", id(1))
            ),
            ["1"]
        );
        assert_eq!(table_rows(store, "command_events"), 7);
    }

    #[test]
    fn a_pick_scrolled_to_after_the_impressions_gets_its_own_impression() {
        let store = CmdStore::in_memory();
        let mut events = CmdEvents::new("tui".to_string(), place(), Some(store));
        events.new_subsession(2, String::new());
        events.wants_impressions(true);
        events
            .log_impressions(&[Shown {
                command_id: 7,
                rank: 1,
            }])
            .unwrap();
        events
            .log_pick(Shown {
                command_id: 9,
                rank: 40,
            })
            .unwrap();
        let rows = query_strings(
            events.store.as_ref().unwrap(),
            "SELECT action, command_id, rank FROM command_events ORDER BY id",
        );
        assert_eq!(rows, ["impression|7|1", "impression|9|40", "pick|9|40"]);
    }

    #[test]
    fn e_b_19_without_a_store_nothing_is_logged_and_nothing_fails() {
        let mut events = CmdEvents::new("tui".to_string(), place(), None);
        events.new_subsession(2, "q".to_string());
        assert!(events.wants_impressions(true));
        events
            .log_impressions(&[Shown {
                command_id: 1,
                rank: 1,
            }])
            .unwrap();
        events
            .log_pick(Shown {
                command_id: 1,
                rank: 1,
            })
            .unwrap();
    }
}
