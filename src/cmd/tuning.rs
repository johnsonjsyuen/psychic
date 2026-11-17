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

// ---------------------------------------------------------------------------
// Phase B: the picker (`docs/ctrl-r/02-browse.md` section 3)
// ---------------------------------------------------------------------------

/// Heuristic weight of recency.
pub(super) const CMD_H_W_RECENCY: f64 = 1.0;

/// Heuristic weight of "run in this directory".
pub(super) const CMD_H_W_DIR: f64 = 0.5;

/// Heuristic weight of "run in this repository".
pub(super) const CMD_H_W_REPO: f64 = 0.2;

/// Heuristic weight of the normalised fuzzy score.
pub(super) const CMD_H_W_FUZZY: f64 = 0.6;

/// Age, in seconds, at which the recency term is 0.5.
pub(super) const CMD_H_RECENCY_SCALE_SECS: f64 = 3600.0;

/// Runs in a directory at which the directory term stops growing.
pub(super) const CMD_H_DIR_SATURATION: f64 = 10.0;

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
