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
#[allow(dead_code, reason = "read by the picker (Phase B)")]
pub(super) const CMD_EVENT_WINDOW_SECS: i64 = 2_592_000;

/// First words that are never recorded: psychic's own commands.
pub(super) const CMD_OWN_COMMANDS: &[&str] = &["psychic", "p", "pd", "pc"];

/// Commands whose every non-flag argument is a path, for templating.
pub(super) const CMD_PATH_ARG_COMMANDS: &[&str] = &[
    "cd", "pushd", "ls", "ll", "la", "cat", "less", "more", "bat", "vim", "vi", "nvim", "nano",
    "emacs", "hx", "code", "rm", "rmdir", "mkdir", "touch", "cp", "mv", "ln", "chmod", "chown",
    "source", ".", "open", "xdg-open", "head", "tail", "wc", "du", "tree",
];
