//! `psychic internal bench-history`: build benchmark corpora and describe them.
//!
//! Phase D0 builds `generate`, `extract-agent` and `stats`; the relevance and
//! latency benchmarks (`relevance`, `twostage`, `latency`) are Phase D. See
//! `docs/ctrl-r/04-bench.md` sections 3 and 4.
//!
//! Every output here is an aggregate: no command text (01 section 10.6).
//! There is no SQL in this file; everything goes through `CmdStore`.

mod perf;
mod relevance;

use super::corpus::{self, Profile};
use super::store::{CmdStore, CommandRecord, EventRecord, RunRecord, StoreCounts};
use super::tuning::CMD_EVENT_WINDOW_SECS;
use anyhow::{Result, bail};
use rustc_hash::{FxHashMap, FxHashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(clap::Args, Debug)]
pub struct BenchArgs {
    #[command(subcommand)]
    pub command: BenchCommand,
}

#[derive(clap::Subcommand, Debug)]
pub enum BenchCommand {
    /// Write a seeded synthetic history into --data-dir
    Generate {
        /// Distinct commands
        #[arg(long)]
        commands: usize,
        /// Runs (executions) in total
        #[arg(long)]
        runs: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Unix second the history ends at (the newest run is 60 s before); default now
        #[arg(long, allow_negative_numbers = true)]
        anchor: Option<i64>,
        /// Shape of the history: like a person at a shell, the D0 generator, or a coding agent
        #[arg(long, value_enum, default_value_t = Profile::Interactive)]
        profile: Profile,
    },
    /// Import Bash tool calls from Claude Code transcripts into --data-dir
    ExtractAgent {
        /// Default: $CLAUDE_CONFIG_DIR/projects, else ~/.claude/projects
        #[arg(long)]
        projects_dir: Option<PathBuf>,
    },
    /// Describe the history in --data-dir
    Stats,
    /// Replay the history and score the rankers
    Relevance {
        /// Models trained per split, seeds 1..=N
        #[arg(long, default_value_t = 5)]
        seeds: u64,
        /// About this many sampled test runs per split
        #[arg(long, default_value_t = 2000)]
        max_test_runs: usize,
        /// Also the rolling-origin folds at 60, 70 and 80% of the timeline
        #[arg(long)]
        folds: bool,
        #[arg(long)]
        json: Option<PathBuf>,
        #[arg(long)]
        md: Option<PathBuf>,
        /// The corpus label printed on every result; default: from the history
        #[arg(long, default_value = "")]
        label: String,
        /// Exit 1 unless R1, R2 and R3 all pass
        #[arg(long)]
        strict: bool,
        /// Control run: train on labels shuffled within each group (the model then knows nothing)
        #[arg(long)]
        shuffle_labels: bool,
    },
    /// Compare two-stage ranking with full scoring
    Twostage {
        #[arg(long, default_value_t = 400)]
        queries: usize,
    },
    /// Time filter+rank without a terminal
    Latency {
        #[arg(long, default_value_t = 300)]
        keystrokes: usize,
        /// Sleep this long before each recorded key, as a typist pauses
        #[arg(long, default_value_t = 0)]
        pause_ms: u64,
        /// Rank as of this Unix second instead of now (to compare two builds)
        #[arg(long)]
        now: Option<i64>,
        /// Write digests of each key's candidates and ranking here (no text)
        #[arg(long)]
        dump: Option<PathBuf>,
    },
}

/// What `run` needs from the process environment, gathered by the caller.
pub(super) struct BenchEnv {
    pub now: i64,
    pub home: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
}

/// `--projects-dir`, else `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`.
fn projects_dir(given: Option<PathBuf>, env: &BenchEnv) -> Result<PathBuf> {
    if let Some(dir) = given {
        return Ok(dir);
    }
    if let Some(config) = &env.claude_config_dir {
        return Ok(config.join("projects"));
    }
    match &env.home {
        Some(home) => Ok(home.join(".claude/projects")),
        None => bail!("HOME is not set: pass --projects-dir"),
    }
}

/// Run one subcommand against `data_dir`, which the caller has checked is
/// not the real data directory.
pub(super) fn run(
    data_dir: &Path,
    command: BenchCommand,
    env: &BenchEnv,
    out: &mut dyn Write,
) -> Result<()> {
    match command {
        BenchCommand::Generate {
            commands,
            runs,
            seed,
            anchor,
            profile,
        } => {
            if commands == 0 || commands > runs {
                bail!("--commands must be between 1 and --runs");
            }
            let store = empty_store(data_dir)?;
            let anchor = anchor.unwrap_or(env.now);
            corpus::generate_profile(&store, commands, runs, seed, anchor, profile)?;
            writeln!(out, "generated {} commands, {} runs", commands, runs)?;
        }
        BenchCommand::ExtractAgent { projects_dir: dir } => {
            let dir = projects_dir(dir, env)?;
            let store = empty_store(data_dir)?;
            let report = corpus::extract_agent(&dir, env.home.as_deref(), &store)?;
            if report.files == 0 {
                writeln!(out, "no transcripts found under {}", dir.display())?;
                return Ok(());
            }
            for line in report.lines() {
                writeln!(out, "{line}")?;
            }
            if report.kept == 0 && report.tool_uses > 0 {
                writeln!(out, "transcript format not recognised")?;
            }
        }
        BenchCommand::Stats => {
            let store = CmdStore::open(data_dir)?;
            let runs = store.all_runs()?;
            let stats = CorpusStats::compute(&store.counts()?, &runs, &store.all_events()?);
            let shape = ShapeStats::compute(&runs, &store.all_commands()?);
            for line in stats.lines().into_iter().chain(shape.lines()) {
                writeln!(out, "{line}")?;
            }
        }
        BenchCommand::Relevance {
            seeds,
            max_test_runs,
            folds,
            json,
            md,
            label,
            strict,
            shuffle_labels,
        } => {
            if seeds == 0 {
                bail!("--seeds must be at least 1");
            }
            let options = relevance::Options {
                seeds,
                max_test_runs,
                folds,
                json,
                md,
                label,
                strict,
                shuffle_labels,
            };
            let today = jiff::Timestamp::from_second(env.now)?
                .strftime("%Y-%m-%d")
                .to_string();
            let passed = relevance::run(data_dir, &options, &today, out, &mut std::io::stderr())?;
            if options.strict && !passed {
                bail!("--strict: R1, R2 and R3 did not all pass");
            }
        }
        BenchCommand::Twostage { queries } => perf::twostage(data_dir, queries, out)?,
        BenchCommand::Latency {
            keystrokes,
            pause_ms,
            now,
            dump,
        } => {
            let options = perf::LatencyOptions {
                keystrokes,
                pause: std::time::Duration::from_millis(pause_ms),
                dump,
            };
            perf::latency(
                data_dir,
                &options,
                env.home.as_deref(),
                now.unwrap_or(env.now),
                out,
            )?
        }
    }
    Ok(())
}

/// The store of `data_dir`, which must not hold any command yet (E-N-02).
fn empty_store(data_dir: &Path) -> Result<CmdStore> {
    let store = CmdStore::open(data_dir)?;
    if store.counts()?.commands > 0 {
        bail!("--data-dir already holds commands");
    }
    Ok(store)
}

// ---------------------------------------------------------------------------
// stats (section 4.4)
// ---------------------------------------------------------------------------

/// A description of a history, without any command text.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CorpusStats {
    pub commands: u64,
    pub runs: u64,
    pub hook_runs: u64,
    pub import_runs: u64,
    /// Newest minus oldest `started_at`, in days.
    pub span_days: f64,
    /// Runs with `started_at > newest - CMD_EVENT_WINDOW_SECS`.
    pub runs_last_30_days: u64,
    pub with_cwd: f64,
    pub with_exit_code: f64,
    /// Share of runs in an `import-synth:` session (1-second fake timestamps).
    pub synthetic_timestamps: f64,
    pub sessions: u64,
    /// Share of runs whose command is the previous run's of the same session.
    pub immediate_repeat_share: f64,
    /// Share of runs that are their command's first run.
    pub first_ever_share: f64,
    /// `pick` rows in `command_events`: one per finished Ctrl-R episode.
    pub pick_episodes: u64,
}

impl CorpusStats {
    /// `runs` in T3 order (`started_at, id`).
    pub(super) fn compute(
        counts: &StoreCounts,
        runs: &[RunRecord],
        events: &[EventRecord],
    ) -> Self {
        assert!(
            runs.is_sorted_by_key(|run| run.started_at),
            "runs come in timeline order"
        );
        let share = |n: usize| {
            if runs.is_empty() {
                0.0
            } else {
                n as f64 / runs.len() as f64
            }
        };
        let (oldest, newest) = match (runs.first(), runs.last()) {
            (Some(first), Some(last)) => (first.started_at, last.started_at),
            _ => (0, 0),
        };

        let mut last_in_session: FxHashMap<&str, i64> = FxHashMap::default();
        let mut seen_commands: FxHashSet<i64> = FxHashSet::default();
        let mut repeats = 0;
        let mut first_ever = 0;
        for run in runs {
            if last_in_session.insert(&run.shell_session, run.command_id) == Some(run.command_id) {
                repeats += 1;
            }
            if seen_commands.insert(run.command_id) {
                first_ever += 1;
            }
        }

        CorpusStats {
            commands: counts.commands,
            runs: runs.len() as u64,
            hook_runs: counts.hook_runs,
            import_runs: counts.import_runs,
            span_days: (newest - oldest) as f64 / 86_400.0,
            runs_last_30_days: runs
                .iter()
                .filter(|run| run.started_at > newest - CMD_EVENT_WINDOW_SECS)
                .count() as u64,
            with_cwd: share(runs.iter().filter(|run| run.cwd.is_some()).count()),
            with_exit_code: share(runs.iter().filter(|run| run.exit_code.is_some()).count()),
            synthetic_timestamps: share(
                runs.iter()
                    .filter(|run| run.shell_session.starts_with("import-synth:"))
                    .count(),
            ),
            sessions: last_in_session.len() as u64,
            immediate_repeat_share: share(repeats),
            first_ever_share: share(first_ever),
            pick_episodes: events.iter().filter(|event| event.is_pick).count() as u64,
        }
    }

    /// One `key: value` line each, in the order of section 4.4.
    pub(super) fn lines(&self) -> Vec<String> {
        vec![
            format!("commands: {}", self.commands),
            format!("runs: {}", self.runs),
            format!(
                "runs by source: hook {}, import {}",
                self.hook_runs, self.import_runs
            ),
            format!("span days: {:.1}", self.span_days),
            format!(
                "runs in last 30 days of the corpus: {}",
                self.runs_last_30_days
            ),
            format!("with cwd: {:.3}", self.with_cwd),
            format!("with exit code: {:.3}", self.with_exit_code),
            format!("synthetic timestamps: {:.3}", self.synthetic_timestamps),
            format!("sessions: {}", self.sessions),
            format!("immediate repeat share: {:.3}", self.immediate_repeat_share),
            format!("first-ever share: {:.3}", self.first_ever_share),
            format!("pick episodes: {}", self.pick_episodes),
        ]
    }
}

/// How repetitive a history is, and so how hard "which command comes next"
/// is on it: the second block of `stats` (section 4.4). No command text, only
/// its length.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ShapeStats {
    /// Share of runs taken by the 10 (100) most-run commands.
    pub top10_share: f64,
    pub top100_share: f64,
    /// Share of the commands that ran exactly once.
    pub once_only_commands: f64,
    /// For each run of a command that ran before: how many runs since its
    /// previous run (1 = the run just before), at p25, p50, p75, p90.
    pub gap_runs: [u64; 4],
    /// The same gap in seconds, at p50 and p90.
    pub gap_secs: [i64; 2],
    /// Of those runs with a cwd on both: same cwd as the previous run.
    pub same_dir_share: f64,
    /// Of those runs: same shell session as the previous run.
    pub same_session_share: f64,
    /// Length of the command text in characters, over runs, at p10, p50, p90.
    pub length_chars: [u64; 3],
}

/// `sorted[min(len - 1, ceil(q * len) - 1)]` (section 6), 0 when empty.
fn percentile<T: Copy + Ord + Default>(sorted: &[T], q: f64) -> T {
    if sorted.is_empty() {
        return T::default();
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

impl ShapeStats {
    /// `runs` in T3 order (`started_at, id`); `commands` as T2.
    pub(super) fn compute(runs: &[RunRecord], commands: &[CommandRecord]) -> Self {
        let length: FxHashMap<i64, u64> = commands
            .iter()
            .map(|command| (command.id, command.text.chars().count() as u64))
            .collect();
        let mut runs_of: FxHashMap<i64, u64> = FxHashMap::default();
        // Per command: (position, started_at, cwd, session) of its last run.
        let mut last: FxHashMap<i64, (usize, i64, Option<&str>, &str)> = FxHashMap::default();
        let mut gap_runs = Vec::new();
        let mut gap_secs = Vec::new();
        let (mut with_dirs, mut same_dir, mut same_session) = (0usize, 0usize, 0usize);
        let mut lengths = Vec::with_capacity(runs.len());
        for (position, run) in runs.iter().enumerate() {
            *runs_of.entry(run.command_id).or_default() += 1;
            lengths.push(length.get(&run.command_id).copied().unwrap_or_default());
            let this = (
                position,
                run.started_at,
                run.cwd.as_deref(),
                run.shell_session.as_str(),
            );
            if let Some((at, started_at, cwd, session)) = last.insert(run.command_id, this) {
                gap_runs.push((position - at) as u64);
                gap_secs.push(run.started_at - started_at);
                if let (Some(before), Some(now)) = (cwd, this.2) {
                    with_dirs += 1;
                    same_dir += usize::from(before == now);
                }
                same_session += usize::from(session == this.3);
            }
        }
        gap_runs.sort_unstable();
        gap_secs.sort_unstable();
        lengths.sort_unstable();
        let mut counts: Vec<u64> = runs_of.values().copied().collect();
        counts.sort_unstable_by(|a, b| b.cmp(a));
        let ratio = |part: usize, whole: usize| {
            if whole == 0 {
                0.0
            } else {
                part as f64 / whole as f64
            }
        };
        let top = |k: usize| ratio(counts.iter().take(k).sum::<u64>() as usize, runs.len());

        ShapeStats {
            top10_share: top(10),
            top100_share: top(100),
            once_only_commands: ratio(counts.iter().filter(|&&n| n == 1).count(), counts.len()),
            gap_runs: [0.25, 0.5, 0.75, 0.9].map(|q| percentile(&gap_runs, q)),
            gap_secs: [0.5, 0.9].map(|q| percentile(&gap_secs, q)),
            same_dir_share: ratio(same_dir, with_dirs),
            same_session_share: ratio(same_session, gap_runs.len()),
            length_chars: [0.1, 0.5, 0.9].map(|q| percentile(&lengths, q)),
        }
    }

    /// One `key: value` line each, in the order of section 4.4.
    pub(super) fn lines(&self) -> Vec<String> {
        let [p25, p50, p75, p90] = self.gap_runs;
        let [s50, s90] = self.gap_secs;
        let [l10, l50, l90] = self.length_chars;
        vec![
            format!("top-10 share: {:.3}", self.top10_share),
            format!("top-100 share: {:.3}", self.top100_share),
            format!("commands run once: {:.3}", self.once_only_commands),
            format!("recurrence gap in runs p25/p50/p75/p90: {p25} / {p50} / {p75} / {p90}"),
            format!("recurrence gap in seconds p50/p90: {s50} / {s90}"),
            format!("same directory as last run: {:.3}", self.same_dir_share),
            format!("same session as last run: {:.3}", self.same_session_share),
            format!("length in chars p10/p50/p90: {l10} / {l50} / {l90}"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::NewRun;
    use super::super::store::test_support::hook_run;
    use super::*;

    fn env() -> BenchEnv {
        BenchEnv {
            now: 1_800_000_000,
            home: Some(PathBuf::from("/home/someone")),
            claude_config_dir: None,
        }
    }

    fn output(data_dir: &Path, command: BenchCommand) -> Result<String> {
        let mut out = Vec::new();
        run(data_dir, command, &env(), &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("psychic-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    // TC-N-09
    #[test]
    fn bench_refuses_the_real_data_dir_and_a_missing_one() {
        let stats = || BenchArgs {
            command: BenchCommand::Stats,
        };
        let guard =
            "bench-history needs an explicit --data-dir that is not your real data directory";
        // The guard runs before anything is opened, so the real dir is never touched.
        if let Ok(real) = crate::cli::get_default_data_dir() {
            let error = super::super::run_bench(&real, true, stats()).unwrap_err();
            assert_eq!(error.to_string(), guard);
        }
        let dir = scratch("bench-guard");
        let error = super::super::run_bench(&dir, false, stats()).unwrap_err();
        assert_eq!(error.to_string(), guard);
        assert!(!dir.exists(), "nothing was created");
        super::super::run_bench(&dir, true, stats()).expect("an explicit scratch dir is fine");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_dir_sees_through_spelling() {
        let dir = scratch("same-dir");
        std::fs::create_dir_all(dir.join("a")).unwrap();
        assert!(super::super::same_dir(&dir.join("a/../a"), &dir.join("a")));
        assert!(super::super::same_dir(
            &dir.join("missing"),
            &dir.join("missing")
        ));
        assert!(!super::super::same_dir(
            &dir.join("a"),
            &dir.join("missing")
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stats_of_an_empty_history_are_zeros() {
        let dir = scratch("bench-empty");
        assert_eq!(
            output(&dir, BenchCommand::Stats).unwrap(),
            "commands: 0
runs: 0
runs by source: hook 0, import 0
span days: 0.0
runs in last 30 days of the corpus: 0
with cwd: 0.000
with exit code: 0.000
synthetic timestamps: 0.000
sessions: 0
immediate repeat share: 0.000
first-ever share: 0.000
pick episodes: 0
top-10 share: 0.000
top-100 share: 0.000
commands run once: 0.000
recurrence gap in runs p25/p50/p75/p90: 0 / 0 / 0 / 0
recurrence gap in seconds p50/p90: 0 / 0
same directory as last run: 0.000
same session as last run: 0.000
length in chars p10/p50/p90: 0 / 0 / 0
"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stats_of_a_small_history() {
        let store = CmdStore::in_memory();
        let day = 86_400;
        let mut runs = vec![
            hook_run("a", "s1", 0, Some(0)),
            hook_run("a", "s1", 10, Some(1)), // immediate repeat
            hook_run("b", "s2", 20, None),
            hook_run("a", "s2", 31 * day, Some(0)), // a in s2 follows b: not a repeat
            hook_run("a", "import-synth:bash:1", 40 * day, None),
        ];
        runs[2].cwd = None;
        for run in &runs {
            store.record_run(run).unwrap();
        }
        let stats = CorpusStats::compute(
            &store.counts().unwrap(),
            &store.all_runs().unwrap(),
            &store.all_events().unwrap(),
        );
        assert_eq!(
            stats.lines(),
            vec![
                "commands: 2",
                "runs: 5",
                "runs by source: hook 5, import 0",
                "span days: 40.0",
                "runs in last 30 days of the corpus: 2",
                "with cwd: 0.800",
                "with exit code: 0.600",
                "synthetic timestamps: 0.200",
                "sessions: 3",
                "immediate repeat share: 0.200",
                "first-ever share: 0.400",
                "pick episodes: 0",
            ]
        );
    }

    #[test]
    fn shape_stats_of_a_small_history() {
        let store = CmdStore::in_memory();
        let run = |text, session, at, cwd| NewRun {
            cwd,
            ..hook_run(text, session, at, Some(0))
        };
        for run in [
            run("ls", "s1", 0, Some("/w")),
            run("make", "s1", 10, Some("/w")),
            run("ls", "s1", 20, Some("/w")), // 2 runs, 20 s; same dir and session
            run("ls", "s2", 50, Some("/x")), // 1 run, 30 s; neither
            run("git status", "s2", 60, None),
            run("git status", "s2", 70, Some("/x")), // 1 run, 10 s; no dir before
        ] {
            store.record_run(&run).unwrap();
        }
        let shape = ShapeStats::compute(&store.all_runs().unwrap(), &store.all_commands().unwrap());
        assert_eq!(
            shape.lines(),
            vec![
                "top-10 share: 1.000",
                "top-100 share: 1.000",
                "commands run once: 0.333",
                "recurrence gap in runs p25/p50/p75/p90: 1 / 1 / 2 / 2",
                "recurrence gap in seconds p50/p90: 20 / 30",
                "same directory as last run: 0.500",
                "same session as last run: 0.667",
                "length in chars p10/p50/p90: 2 / 2 / 10",
            ]
        );
    }

    #[test]
    fn generate_and_extract_need_an_empty_history() {
        let dir = scratch("bench-nonempty");
        let generate = || BenchCommand::Generate {
            commands: 10,
            runs: 100,
            seed: 1,
            anchor: Some(1_000_000),
            profile: Profile::Interactive,
        };
        assert_eq!(
            output(&dir, generate()).unwrap(),
            "generated 10 commands, 100 runs\n"
        );
        let error = output(&dir, generate()).unwrap_err();
        assert_eq!(error.to_string(), "--data-dir already holds commands");
        let error = output(
            &dir,
            BenchCommand::ExtractAgent {
                projects_dir: Some(dir.join("none")),
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "--data-dir already holds commands");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generate_rejects_impossible_sizes() {
        let dir = scratch("bench-sizes");
        for (commands, runs) in [(0, 10), (11, 10)] {
            let error = output(
                &dir,
                BenchCommand::Generate {
                    commands,
                    runs,
                    seed: 1,
                    anchor: None,
                    profile: Profile::Interactive,
                },
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "--commands must be between 1 and --runs");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_agent_without_transcripts_writes_nothing() {
        let dir = scratch("bench-no-transcripts");
        let projects = dir.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join("readme.txt"), "x").unwrap();
        let data = dir.join("data");
        assert_eq!(
            output(
                &data,
                BenchCommand::ExtractAgent {
                    projects_dir: Some(projects.clone())
                }
            )
            .unwrap(),
            format!("no transcripts found under {}\n", projects.display())
        );
        assert!(
            output(&data, BenchCommand::Stats)
                .unwrap()
                .starts_with("commands: 0\n")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_agent_reports_an_unrecognised_format() {
        let dir = scratch("bench-format");
        let projects = dir.join("projects/p");
        std::fs::create_dir_all(&projects).unwrap();
        // A Bash use whose command field was renamed: counted, never kept.
        std::fs::write(
            projects.join("s.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-11-01T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"x","input":{"cmd":"make"}}]}}"#,
        )
        .unwrap();
        let text = output(
            &dir.join("data"),
            BenchCommand::ExtractAgent {
                projects_dir: Some(dir.join("projects")),
            },
        )
        .unwrap();
        assert!(text.contains("tool uses: 1\nkept: 0\n"), "{text}");
        assert!(
            text.ends_with("transcript format not recognised\n"),
            "{text}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn projects_dir_precedence() {
        let mut env = env();
        assert_eq!(
            projects_dir(None, &env).unwrap(),
            PathBuf::from("/home/someone/.claude/projects")
        );
        env.claude_config_dir = Some(PathBuf::from("/cfg"));
        assert_eq!(
            projects_dir(None, &env).unwrap(),
            PathBuf::from("/cfg/projects")
        );
        assert_eq!(
            projects_dir(Some(PathBuf::from("/given")), &env).unwrap(),
            PathBuf::from("/given")
        );
        env.claude_config_dir = None;
        env.home = None;
        assert!(projects_dir(None, &env).is_err());
    }

    #[test]
    fn clap_surface() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "psychic",
            "--data-dir",
            "/tmp/x",
            "internal",
            "bench-history",
            "generate",
            "--commands",
            "5",
            "--runs",
            "9",
        ])
        .unwrap();
        let Some(crate::cli::Commands::Internal {
            command: crate::cli::InternalCommands::BenchHistory(args),
        }) = cli.command
        else {
            panic!("parsed as something else");
        };
        assert!(matches!(
            args.command,
            BenchCommand::Generate {
                commands: 5,
                runs: 9,
                seed: 1,
                anchor: None,
                profile: Profile::Interactive,
            }
        ));
        let cli = crate::cli::Cli::try_parse_from([
            "psychic",
            "--data-dir",
            "/tmp/x",
            "internal",
            "bench-history",
            "generate",
            "--commands",
            "5",
            "--runs",
            "9",
            "--profile",
            "agent",
        ])
        .unwrap();
        let Some(crate::cli::Commands::Internal {
            command: crate::cli::InternalCommands::BenchHistory(args),
        }) = cli.command
        else {
            panic!("parsed as something else");
        };
        assert!(matches!(
            args.command,
            BenchCommand::Generate {
                profile: Profile::Agent,
                ..
            }
        ));
    }
}
