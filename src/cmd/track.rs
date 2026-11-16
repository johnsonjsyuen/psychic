//! `psychic track-command`: record one command the shell just ran.
//!
//! Runs on every prompt while recording is on, in the background, so it is
//! kept to process start, one database open and one short transaction
//! (budget B4, 10 ms p95). It never checks the opt-in marker - being called is
//! the hook's decision - and never logs: `main` dispatches here before the
//! logger exists. See `docs/ctrl-r/01-record.md` section 9.

use super::cli::TrackCommandArgs;
use super::store::{CmdStore, NewRun, Source};
use super::tuning::CMD_MAX_TEXT_BYTES;
use super::{privacy, repo, template};
use anyhow::{Result, bail};
use std::path::Path;

/// Whether `text` (already trimmed) may be recorded: not empty, not too long,
/// not psychic's own, not matching `PSYCHIC_HISTORY_IGNORE` (`ignore`).
/// Shared with the importer, so both apply the same rules.
pub(super) fn is_recordable(text: &str, ignore: Option<&str>) -> bool {
    !text.is_empty()
        && text.len() <= CMD_MAX_TEXT_BYTES
        && !template::is_own_command(text)
        && !privacy::is_ignored(text, ignore)
}

/// Record `args` in `data_dir`. `now` is the current Unix second and
/// `env_ignore` the value of `PSYCHIC_HISTORY_IGNORE`.
pub(super) fn run(
    data_dir: Option<&Path>,
    args: &TrackCommandArgs,
    now: i64,
    env_ignore: Option<&str>,
) -> Result<()> {
    let Some(data_dir) = data_dir else {
        bail!("No data directory: pass --data-dir, or set HOME");
    };
    let raw = args.command.to_string_lossy();
    let text = raw.trim();
    if !is_recordable(text, env_ignore) {
        return Ok(());
    }

    // A negative duration means the clock stepped back; it is unknown.
    let duration_ms = args.duration_ms.filter(|ms| *ms >= 0);
    let started_at = args
        .started_at
        .unwrap_or(now - duration_ms.unwrap_or(0) / 1000);
    let cwd = Some(args.cwd.as_str()).filter(|cwd| !cwd.is_empty());
    let repo_root = cwd.and_then(|cwd| repo::find_repo_root(Path::new(cwd)));
    let repo_root = repo_root.as_deref().map(Path::to_string_lossy);
    let template = template::templatize(text);

    CmdStore::open(data_dir)?.record_run(&NewRun {
        text,
        template: &template,
        started_at,
        cwd,
        repo_root: repo_root.as_deref(),
        exit_code: args.exit_code,
        duration_ms,
        shell_session: &args.session,
        source: Source::Hook,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::store::test_support::query_strings;
    use std::ffi::OsString;

    fn args(text: &str) -> TrackCommandArgs {
        TrackCommandArgs {
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: None,
            duration_ms: None,
            session: "s1".to_string(),
            command: OsString::from(text),
        }
    }

    fn data_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("psychic-cmd-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn empty_long_own_and_ignored_commands_are_not_recorded() {
        let dir = data_dir("track-skip");
        let long = "a".repeat(8193);
        for text in [
            "   ",
            long.as_str(),
            "p",
            "pd foo",
            "psychic history",
            "export TOKEN=1",
        ] {
            run(Some(&dir), &args(text), 1000, Some("export *")).unwrap();
        }
        // Nothing was recorded, so the store was never even opened.
        assert!(!dir.exists());

        run(Some(&dir), &args(&"a".repeat(8192)), 1000, Some("export *")).unwrap();
        let store = CmdStore::open(&dir).unwrap();
        assert_eq!(
            query_strings(&store, "SELECT COUNT(*) FROM commands"),
            vec!["1"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_start_is_derived_from_the_duration_unless_given() {
        let dir = data_dir("track-start");
        let with = |text: &str, started_at, duration_ms| TrackCommandArgs {
            started_at,
            duration_ms,
            ..args(text)
        };
        run(Some(&dir), &with("a", None, Some(2500)), 1000, None).unwrap();
        run(Some(&dir), &with("b", Some(990), Some(2500)), 1000, None).unwrap();
        run(Some(&dir), &with("c", None, None), 1000, None).unwrap();
        run(Some(&dir), &with("d", None, Some(-40)), 1000, None).unwrap();

        let store = CmdStore::open(&dir).unwrap();
        assert_eq!(
            query_strings(
                &store,
                "SELECT c.text, r.started_at, IFNULL(r.duration_ms, 'NULL'), r.cwd, r.source
                 FROM command_runs r JOIN commands c ON c.id = r.command_id ORDER BY r.id"
            ),
            vec![
                "a|998|2500|/tmp|hook",
                "b|990|2500|/tmp|hook",
                "c|1000|NULL|/tmp|hook",
                "d|1000|NULL|/tmp|hook",
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_text_is_trimmed_and_templated_and_the_repo_found() {
        let dir = data_dir("track-repo");
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let cwd = repo.join("src").to_string_lossy().into_owned();
        let tracked = TrackCommandArgs {
            cwd: cwd.clone(),
            exit_code: Some(2),
            ..args("  vim main.rs \n")
        };
        run(Some(&dir), &tracked, 1000, None).unwrap();

        let store = CmdStore::open(&dir).unwrap();
        assert_eq!(
            query_strings(
                &store,
                "SELECT c.text, c.template, c.fail_count, c.last_exit_code, r.repo_root = ?1
                 FROM command_runs r JOIN commands c ON c.id = r.command_id"
                    .replace("?1", &format!("'{}'", repo.display()))
                    .as_str()
            ),
            vec!["vim main.rs|vim PATH|1|2|1"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_data_directory_is_an_error_without_the_text() {
        let error = run(None, &args("secret-thing"), 1000, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "No data directory: pass --data-dir, or set HOME"
        );
    }
}
