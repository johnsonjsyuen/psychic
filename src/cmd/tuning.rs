//! Every tunable constant of shell history search, in one place.
//!
//! Values and meanings are specified in `docs/ctrl-r/01-record.md` section 3
//! (Phase A); later phases add their own blocks below.

/// A command longer than this many bytes (after trimming) is not recorded.
pub(super) const CMD_MAX_TEXT_BYTES: usize = 8192;

/// A run's predecessor counts as its "previous command" only if it started at
/// most this many seconds before, in the same shell session.
pub(super) const CMD_SEQUENCE_MAX_GAP_SECS: i64 = 1800;

/// Opt-in marker file in the data directory. The shell hooks record only
/// while it exists (or `PSYCHIC_HISTORY=1`).
pub(super) const CMD_MARKER_FILE: &str = "history-enabled";

/// 30 days: how far back individual runs and picks are loaded at picker start.
pub(super) const CMD_EVENT_WINDOW_SECS: i64 = 2_592_000;

/// First words that are never recorded: psychic's own commands.
pub(super) const CMD_OWN_COMMANDS: &[&str] = &["psychic", "p", "pd", "pc"];

/// Commands whose every non-flag argument is a path, for templating.
pub(super) const CMD_PATH_ARG_COMMANDS: &[&str] = &[
    "cd", "pushd", "ls", "ll", "la", "cat", "less", "more", "bat", "vim", "vi", "nvim", "nano",
    "emacs", "hx", "code", "rm", "rmdir", "mkdir", "touch", "cp", "mv", "ln", "chmod", "chown",
    "source", ".", "open", "xdg-open", "head", "tail", "wc", "du", "tree",
];

/// `track-command` leaves its runs in `events.db-wal` instead of checkpointing
/// on close; once the log is longer than this many bytes, the run that finds
/// it so checkpoints it and cuts it to zero. Bounds both the log and what the
/// next process to open the database alone re-reads (about 0.25 ms per MiB).
pub(super) const CMD_WAL_TRUNCATE_BYTES: u64 = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Phase B: the picker (`docs/ctrl-r/02-browse.md` section 3)
// ---------------------------------------------------------------------------

/// Age, in seconds, at which the recency term is 0.5. It only breaks ties
/// of the fzf score and shapes the value the model blends with (02 section 7).
pub(super) const CMD_H_RECENCY_SCALE_SECS: f64 = 3600.0;

/// Connections that read the commands (R1) at picker start, each a slice
/// of the id range, on a thread of its own.
pub(super) const CMD_LOAD_READERS: usize = 4;

/// Base-set size from which the filter (and the heuristic) uses rayon.
pub(super) const CMD_PAR_FILTER_MIN: usize = 2000;

/// Rows per `QueryUpdated` / `Rows` response.
pub(super) const CMD_PAGE_ROWS: usize = 200;

/// Request the next page when the selection is this close to the last loaded row.
pub(super) const CMD_PAGE_PREFETCH: usize = 50;

/// A query on screen this long, in milliseconds, counts as seen.
pub(super) const CMD_IMPRESSION_AGE_MS: u64 = 200;

/// Tick thread sleep, in milliseconds; each tick wakes the event loop once.
pub(super) const CMD_TICK_MS: u64 = 1000;

/// Terminals narrower than this stack the list over the preview.
pub(super) const CMD_STACK_BELOW_COLS: u16 = 120;

// ---------------------------------------------------------------------------
// Phase C: the model (`docs/ctrl-r/03-learn.md` section 3)
// ---------------------------------------------------------------------------

/// The `recency_rank` feature counts newer matching commands up to this.
pub(super) const CMD_RECENCY_RANK_CAP: usize = 15;

/// Rows, from the top of the heuristic order, that the model scores.
pub(super) const CMD_STAGE2_K: usize = 2000;

/// `blend_examples` are divided by this before the file blend formula.
pub(super) const CMD_BLEND_SCALE: f64 = 10.0;

/// Sample weight of every row of a Ctrl-R pick group.
pub(super) const CMD_PICK_WEIGHT: f64 = 4.0;

/// Sample weight of the rows of a group built from a hook-recorded run.
pub(super) const CMD_HOOK_WEIGHT: f64 = 1.0;

/// Sample weight of the rows of a group built from an imported run.
pub(super) const CMD_IMPORT_WEIGHT: f64 = 0.3;

/// Seed of query-length choice and negative sampling, and the LightGBM seed
/// in production.
pub(super) const CMD_TRAIN_SEED: u64 = 42;

/// Only the newest this-many eligible runs produce groups.
pub(super) const CMD_TRAIN_MAX_RUN_GROUPS: usize = 8000;

/// Below this many groups nothing is trained.
pub(super) const CMD_TRAIN_MIN_GROUPS: usize = 50;

/// Percent of run groups with synthetic query length 0, 1, 2, 3.
pub(super) const CMD_TRAIN_QLEN_MIX: [u32; 4] = [40, 20, 20, 20];

/// Percent of run groups with a non-empty query whose query is a fragment
/// of a later word rather than a prefix.
pub(super) const CMD_TRAIN_FRAGMENT_SHARE: u32 = 25;

/// Negatives taken from the most recently run matching commands. 0: the
/// newest commands as fixed negatives skewed every recency feature
/// (03 section 5.2, results.md 2026-11-17).
pub(super) const CMD_TRAIN_RECENT_NEGATIVES: usize = 0;

/// Distinct recent commands examined to find them.
pub(super) const CMD_TRAIN_RECENT_SCAN: usize = 500;

/// Negatives drawn at random from all matching commands.
pub(super) const CMD_TRAIN_RANDOM_NEGATIVES: usize = 30;

/// Draws allowed to find them.
pub(super) const CMD_TRAIN_RANDOM_TRIES: usize = 200;

/// "Never" on the `ln(1 + seconds)` axis: `ln(1 + 10 years)`, as file mode.
pub(super) const CMD_NEVER_SECS_LOG: f64 = 19.57;

/// Window of the `failed_within_2_minutes` feature.
pub(super) const CMD_FAIL_RECENT_SECS: i64 = 120;

/// The picker starts a retrain only when the last attempt is older than this.
pub(super) const CMD_RETRAIN_MIN_INTERVAL_SECS: u64 = 3600;

/// A run this soon after a Ctrl-R pick of the same command in the same shell
/// session is that pick being executed; it gets no group of its own.
pub(super) const CMD_PICK_RUN_SECS: i64 = 300;

/// A training lock file older than this is taken over.
pub(super) const CMD_TRAIN_LOCK_STALE_SECS: u64 = 900;

/// Every constant above as `name -> value`: the `tuning` object of the
/// relevance benchmark's JSON (`docs/ctrl-r/04-bench.md` section 5.6), so a
/// result names the settings it was measured with.
pub(super) fn snapshot() -> serde_json::Map<String, serde_json::Value> {
    macro_rules! entries {
        ($($name:ident),* $(,)?) => {{
            let mut map = serde_json::Map::new();
            $(map.insert(stringify!($name).to_string(), serde_json::json!($name));)*
            map
        }};
    }
    entries!(
        CMD_MAX_TEXT_BYTES,
        CMD_SEQUENCE_MAX_GAP_SECS,
        CMD_MARKER_FILE,
        CMD_EVENT_WINDOW_SECS,
        CMD_OWN_COMMANDS,
        CMD_PATH_ARG_COMMANDS,
        CMD_WAL_TRUNCATE_BYTES,
        CMD_H_RECENCY_SCALE_SECS,
        CMD_LOAD_READERS,
        CMD_PAR_FILTER_MIN,
        CMD_PAGE_ROWS,
        CMD_PAGE_PREFETCH,
        CMD_IMPRESSION_AGE_MS,
        CMD_TICK_MS,
        CMD_STACK_BELOW_COLS,
        CMD_RECENCY_RANK_CAP,
        CMD_STAGE2_K,
        CMD_BLEND_SCALE,
        CMD_PICK_WEIGHT,
        CMD_HOOK_WEIGHT,
        CMD_IMPORT_WEIGHT,
        CMD_TRAIN_SEED,
        CMD_TRAIN_MAX_RUN_GROUPS,
        CMD_TRAIN_MIN_GROUPS,
        CMD_TRAIN_QLEN_MIX,
        CMD_TRAIN_FRAGMENT_SHARE,
        CMD_TRAIN_RECENT_NEGATIVES,
        CMD_TRAIN_RECENT_SCAN,
        CMD_TRAIN_RANDOM_NEGATIVES,
        CMD_TRAIN_RANDOM_TRIES,
        CMD_NEVER_SECS_LOG,
        CMD_FAIL_RECENT_SECS,
        CMD_RETRAIN_MIN_INTERVAL_SECS,
        CMD_PICK_RUN_SECS,
        CMD_TRAIN_LOCK_STALE_SECS
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_snapshot_holds_every_constant() {
        let source = include_str!("tuning.rs");
        let declared = source
            .lines()
            .filter(|line| line.starts_with("pub(super) const "))
            .count();
        assert_eq!(super::snapshot().len(), declared);
    }
}
