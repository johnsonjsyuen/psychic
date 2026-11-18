//! The clap surface of `psychic history` and `psychic track-command`.
//!
//! This file is the single definition of every flag of both subcommands
//! (`docs/ctrl-r/01-record.md` section 4). Behaviour lives elsewhere.

use std::ffi::OsString;
use std::path::PathBuf;

/// `psychic track-command`: what the shell hooks call once per command.
///
/// Invoked as `track-command --cwd DIR --exit-code N [--started-at S]
/// [--duration-ms MS] --session ID -- TEXT`, with `TEXT` exactly one argv
/// element, so quotes, newlines and a leading `-` survive.
#[derive(clap::Args, Debug)]
pub struct TrackCommandArgs {
    /// Directory the command was typed in
    #[arg(long)]
    pub cwd: PathBuf,
    /// Exit status of the command
    #[arg(long, allow_negative_numbers = true)]
    pub exit_code: Option<i32>,
    /// When it started, Unix seconds
    #[arg(long, allow_negative_numbers = true)]
    pub started_at: Option<i64>,
    /// How long it ran, in milliseconds
    #[arg(long, allow_negative_numbers = true)]
    pub duration_ms: Option<i64>,
    /// Identifies the shell that ran it
    #[arg(long)]
    pub session: String,
    /// The command text, after `--`
    #[arg(last = true, required = true)]
    pub command: OsString,
}

/// `psychic history`: the picker, or one action on what is recorded.
#[derive(clap::Args, Debug)]
#[command(group(clap::ArgGroup::new("action").multiple(false)
    .args(["enable", "disable", "status", "import", "purge", "retrain"])))]
pub struct HistoryArgs {
    /// What to start the search with
    #[arg(long, value_name = "QUERY", default_value = "")]
    pub query: String,
    /// The shell session asking, so its recent commands count
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    /// The directory the shell is in
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<String>,
    /// Start recording commands, importing existing shell history the first time
    #[arg(long)]
    pub enable: bool,
    /// With --enable, skip importing existing shell history
    #[arg(long, requires = "enable")]
    pub no_import: bool,
    /// Stop recording commands; what is recorded is kept
    #[arg(long)]
    pub disable: bool,
    /// Show whether recording is on and how much is recorded
    #[arg(long)]
    pub status: bool,
    /// Import shell history (Atuin, zsh, bash, fish) now
    #[arg(long)]
    pub import: bool,
    /// Delete every recorded command
    #[arg(long)]
    pub purge: bool,
    /// Don't ask before purging
    #[arg(long, short)]
    pub yes: bool,
    /// Train the command ranking model
    #[arg(long)]
    pub retrain: bool,
    /// With --retrain, print nothing
    #[arg(long, requires = "retrain")]
    pub quiet: bool,
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};
    use clap::Parser;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    #[test]
    fn track_command_takes_the_text_after_the_double_dash_verbatim() {
        let cli = Cli::try_parse_from([
            "psychic",
            "track-command",
            "--cwd",
            "/x",
            "--session",
            "s",
            "--",
            "-rf --weird\nline",
        ])
        .expect("parses");
        let Some(Commands::TrackCommand(args)) = cli.command else {
            panic!("expected track-command");
        };
        assert_eq!(args.command, "-rf --weird\nline");
        assert_eq!(args.cwd, PathBuf::from("/x"));
        assert_eq!(args.session, "s");
        assert_eq!(args.exit_code, None);
    }

    #[test]
    fn track_command_accepts_negative_numbers() {
        let cli = Cli::try_parse_from([
            "psychic",
            "track-command",
            "--cwd",
            "/x",
            "--exit-code",
            "-1",
            "--duration-ms",
            "-5",
            "--session",
            "s",
            "--",
            "ls",
        ])
        .expect("parses");
        let Some(Commands::TrackCommand(args)) = cli.command else {
            panic!("expected track-command");
        };
        assert_eq!((args.exit_code, args.duration_ms), (Some(-1), Some(-5)));
    }

    #[test]
    fn track_command_without_the_double_dash_is_a_parse_error() {
        let parsed = Cli::try_parse_from([
            "psychic",
            "track-command",
            "--cwd",
            "/x",
            "--session",
            "s",
            "ls",
        ]);
        assert!(parsed.is_err());
    }

    #[test]
    fn two_history_actions_at_once_are_a_parse_error() {
        assert!(Cli::try_parse_from(["psychic", "history", "--enable", "--purge"]).is_err());
        assert!(Cli::try_parse_from(["psychic", "history", "--no-import"]).is_err());
        assert!(Cli::try_parse_from(["psychic", "history", "--enable", "--no-import"]).is_ok());
        assert!(Cli::try_parse_from(["psychic", "history", "--purge", "-y"]).is_ok());
    }

    #[test]
    fn track_command_takes_a_directory_that_is_not_utf8() {
        let cwd = OsString::from_vec(b"/tmp/caf\xe9".to_vec());
        let cli = Cli::try_parse_from([
            OsString::from("psychic"),
            OsString::from("track-command"),
            OsString::from("--cwd"),
            cwd.clone(),
            OsString::from("--session"),
            OsString::from("s"),
            OsString::from("--"),
            OsString::from("ls"),
        ])
        .expect("parses");
        let Some(Commands::TrackCommand(args)) = cli.command else {
            panic!("expected track-command");
        };
        assert_eq!(args.cwd, PathBuf::from(cwd));
    }
}
