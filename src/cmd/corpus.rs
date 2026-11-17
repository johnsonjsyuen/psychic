//! Benchmark corpora for `psychic internal bench-history`: a seeded synthetic
//! history and the Bash tool calls of Claude Code transcripts.
//!
//! Both write through the production write path (`CmdStore::record_run_in`),
//! so a corpus is indistinguishable from recorded history except for its
//! `shell_session` prefix (`synth:` or `agent:`). See
//! `docs/ctrl-r/04-bench.md` sections 4.2 and 4.3.
//!
//! Command text read from transcripts goes into `events.db` and nowhere else:
//! nothing here prints or logs it (`docs/ctrl-r/01-record.md` section 10.6).

use super::store::{CmdStore, NewRun, Source};
use super::{repo, template, track};
use anyhow::{Context, Result};
use rustc_hash::FxHashMap;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Random stream
// ---------------------------------------------------------------------------

/// The splitmix64 finaliser (`docs/ctrl-r/03-learn.md` section 5.2).
pub(super) fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The generator's one random stream. Every draw goes through it, in a fixed
/// order, so a `(commands, runs, seed)` triple always produces the same history.
struct Stream {
    state: u64,
}

impl Stream {
    fn next(&mut self) -> u64 {
        self.state = splitmix64(self.state);
        self.state
    }

    /// Uniform in `[0, 1)`: the top 53 bits, so the value is exact and never 1.0.
    fn u(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "below(0) has no value to return");
        self.next() % n
    }
}

/// An exponent-1 Zipf position in a list of length `n`, from a uniform `u`:
/// `P(k) = ln((k + 2) / (k + 1)) / ln(n + 1)`. The head is the most likely and
/// every position is possible.
fn zipf(u: f64, n: usize) -> usize {
    assert!(n > 0, "a Zipf pick needs a non-empty list");
    assert!((0.0..1.0).contains(&u), "u is in [0, 1)");
    ((((n + 1) as f64).powf(u)) as usize)
        .saturating_sub(1)
        .min(n - 1)
}

// ---------------------------------------------------------------------------
// Synthetic generator (section 4.3)
// ---------------------------------------------------------------------------

/// The shape of a synthetic history (`generate --profile`, section 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    /// Calibrated on a real interactive shell history: few immediate
    /// repeats, a heavy head of short commands, most commands run once.
    Interactive,
    /// The D0 generator, unchanged: about half of all runs repeat the
    /// previous command. A stress case.
    Repetitive,
    /// Like a coding agent: almost no repeats, a flat popularity and long
    /// commands. A stress case.
    Agent,
}

/// How a profile spells command `i`.
#[derive(Clone, Copy)]
enum TextStyle {
    /// `tool sub arg`, every command three words (the D0 texts).
    ThreeWords,
    /// The most popular commands are one or two words; some of the tail
    /// grows a pipe or extra flags.
    Interactive,
    /// A `cd` into the repository, the command, a message of a heavy-tailed
    /// number of words, and output trimming.
    Agent,
}

/// The parameters of a profile: section 4.3, table "Profiles".
struct Shape {
    /// Per run: chance of moving to another directory.
    p_move: f64,
    /// Per run: chance the run fails.
    p_fail: f64,
    /// After a failure: chance the next reused run retries it.
    p_retry: f64,
    /// Upper ends of the reuse branches over one draw `c`, in this order:
    /// repeat, chain, directory, recent; global above the last.
    repeat_below: f64,
    chain_below: f64,
    dir_below: f64,
    recent_below: f64,
    /// Repeat branch: weights of the session's last three, newest first.
    repeat_weights: [f64; RecentCommands::LEN],
    /// Exponent of the global popularity Zipf (1.0 is the D0 formula).
    zipf_s: f64,
    /// Exponent of the recent branch's Zipf over the working set.
    recent_s: f64,
    /// A chain, directory, recent or global pick that equals the previous
    /// command takes its list's next entry instead, so immediate repeats come
    /// only from the repeat and retry branches.
    avoid_repeat: bool,
    /// Share of commands (after the first [`ALWAYS_REUSABLE`]) that are
    /// one-offs: run when new, and again only by the repeat or retry branch.
    one_off: f64,
    /// A homed command's home is the directory it first ran in, rather than
    /// a hashed one.
    home_where_first_run: bool,
    text: TextStyle,
}

impl Profile {
    fn shape(self) -> Shape {
        match self {
            Profile::Interactive => Shape {
                p_move: 0.02,
                p_fail: 0.08,
                p_retry: 0.25,
                repeat_below: 0.045,
                chain_below: 0.10,
                dir_below: 0.30,
                recent_below: 0.65,
                repeat_weights: [1.0, 0.0, 0.0],
                zipf_s: 1.5,
                recent_s: 1.8,
                avoid_repeat: true,
                one_off: 0.5,
                home_where_first_run: true,
                text: TextStyle::Interactive,
            },
            Profile::Repetitive => Shape {
                p_move: 0.08,
                p_fail: 0.10,
                p_retry: 0.5,
                repeat_below: 0.55,
                chain_below: 0.68,
                dir_below: 0.87,
                recent_below: 0.87,
                repeat_weights: [0.80, 0.12, 0.08],
                zipf_s: 1.0,
                recent_s: 1.0,
                avoid_repeat: false,
                one_off: 0.0,
                home_where_first_run: false,
                text: TextStyle::ThreeWords,
            },
            Profile::Agent => Shape {
                p_move: 0.005,
                p_fail: 0.10,
                p_retry: 0.0,
                repeat_below: 0.0,
                chain_below: 0.05,
                dir_below: 0.60,
                recent_below: 0.85,
                repeat_weights: [1.0, 0.0, 0.0],
                zipf_s: 0.0,
                recent_s: 1.0,
                avoid_repeat: true,
                one_off: 0.0,
                home_where_first_run: true,
                text: TextStyle::Agent,
            },
        }
    }
}

/// A Zipf position of exponent `s` in a list of length `n`, from a uniform
/// `u`: the inverse CDF of the density `(x + 1)^-s` on `[0, n)`, floored.
/// `s = 1` is [`zipf`] exactly; `s = 0` is uniform; a larger `s` has a
/// heavier head.
fn zipf_s(u: f64, n: usize, s: f64) -> usize {
    if s == 1.0 {
        return zipf(u, n);
    }
    assert!(n > 0, "a Zipf pick needs a non-empty list");
    assert!((0.0..1.0).contains(&u), "u is in [0, 1)");
    let a = 1.0 - s;
    let top = ((n + 1) as f64).powf(a);
    let x = (1.0 + u * (top - 1.0)).powf(1.0 / a);
    (x as usize).saturating_sub(1).min(n - 1)
}

const TOOLS: [&str; 40] = [
    "git",
    "cargo",
    "npm",
    "docker",
    "kubectl",
    "make",
    "ls",
    "cat",
    "grep",
    "rg",
    "ssh",
    "curl",
    "python",
    "pytest",
    "just",
    "go",
    "terraform",
    "aws",
    "vim",
    "tar",
    "rsync",
    "scp",
    "find",
    "sed",
    "awk",
    "jq",
    "node",
    "yarn",
    "pnpm",
    "pip",
    "uv",
    "brew",
    "systemctl",
    "journalctl",
    "psql",
    "sqlite3",
    "gh",
    "helm",
    "ansible",
    "tmux",
];

const SUBS: [&str; 25] = [
    "status", "build", "test", "run", "push", "pull", "log", "diff", "add", "commit", "checkout",
    "fetch", "install", "update", "list", "show", "apply", "plan", "deploy", "exec", "logs",
    "start", "stop", "clean", "check",
];

const REPOS: usize = 8;
const DIRS_PER_REPO: usize = 5;
const DIRS: usize = REPOS * DIRS_PER_REPO;
/// Runs written per transaction.
const GENERATE_BATCH: usize = 10_000;
/// Distinct commands the recent branch chooses among: the most recently run.
const WORKING_SET: usize = 256;
/// The most popular commands are never one-offs, so the reusable list is
/// never empty once something has run.
const ALWAYS_REUSABLE: usize = 10;

/// The fixed part of the synthetic world: what command `i` is, where it lives
/// and what tends to follow it. Pure functions of the seed and `i`.
struct World {
    salts: [u64; 7],
}

impl World {
    fn new(seed: u64) -> Self {
        World {
            salts: std::array::from_fn(|salt| splitmix64(seed.wrapping_add(salt as u64))),
        }
    }

    /// `h(salt, i) = splitmix64(splitmix64(seed + salt) ^ i)`.
    fn h(&self, salt: usize, i: usize) -> u64 {
        splitmix64(self.salts[salt] ^ i as u64)
    }

    /// The D0 text: `tool sub arg`, unique because `arg` holds `i`.
    fn text(i: usize) -> String {
        let tool = TOOLS[i % TOOLS.len()];
        let sub = SUBS[(i / TOOLS.len()) % SUBS.len()];
        if i.is_multiple_of(3) {
            format!("{tool} {sub} src/file{i}.rs")
        } else {
            format!("{tool} {sub} arg{i}")
        }
    }

    /// Command `i` in `style`. Unique for every `i` in every style.
    fn styled_text(&self, style: TextStyle, i: usize) -> String {
        // Commands spelled as one word: the head of the popularity.
        const SINGLE_WORD: usize = 8;
        const PAIRS: usize = TOOLS.len() * SUBS.len();
        match style {
            TextStyle::ThreeWords => Self::text(i),
            TextStyle::Interactive if i < SINGLE_WORD => TOOLS[i].to_string(),
            TextStyle::Interactive => match self.h(5, i) % 10 {
                // Two words while `i` still names a distinct pair.
                0..=3 if i < PAIRS => {
                    format!("{} {}", TOOLS[i % TOOLS.len()], SUBS[i / TOOLS.len()])
                }
                0..=7 => Self::text(i),
                _ => format!("{} 2>&1 | grep -v pattern{}", Self::text(i), i % 97),
            },
            TextStyle::Agent => {
                // Words in the message: 5 to 205, most short, a few very long.
                let u = (self.h(5, i) >> 11) as f64 / (1u64 << 53) as f64;
                let words = 5 + (200.0 * u * u * u) as usize;
                let message: Vec<&str> =
                    (0..words).map(|k| SUBS[(i + 7 * k) % SUBS.len()]).collect();
                format!(
                    "cd /synth/repo{} && {} --message \"{}\" 2>&1 | tail -n {}",
                    i % REPOS,
                    Self::text(i),
                    message.join(" "),
                    20 + i % 80
                )
            }
        }
    }

    /// The directory command `i` is mostly run in; `None` for a global command.
    fn home(&self, i: usize) -> Option<usize> {
        (self.h(2, i) % 10 < 7).then(|| (self.h(1, i) % DIRS as u64) as usize)
    }

    /// Whether command `i` may be chosen again by the chain, directory,
    /// recent and global branches: all but a `one_off` share of the commands
    /// after the first [`ALWAYS_REUSABLE`].
    fn reusable(&self, i: usize, one_off: f64) -> bool {
        i < ALWAYS_REUSABLE || ((self.h(6, i) >> 11) as f64 / (1u64 << 53) as f64) >= one_off
    }

    /// The command that tends to follow `i` (never a later one).
    fn successor(&self, i: usize) -> Option<usize> {
        (self.h(3, i).is_multiple_of(4)).then(|| (self.h(4, i) % (i as u64 + 1)) as usize)
    }
}

fn dir_path(dir: usize) -> String {
    format!(
        "/synth/repo{}/dir{}",
        dir / DIRS_PER_REPO,
        dir % DIRS_PER_REPO
    )
}

fn repo_path(dir: usize) -> String {
    format!("/synth/repo{}", dir / DIRS_PER_REPO)
}

/// One simulated execution, before the timeline is anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SynthRun {
    command: usize,
    /// Seconds since the start of the simulation.
    t: i64,
    dir: usize,
    session: u64,
    exit_code: i32,
    duration_ms: i64,
}

/// The session's last three commands, newest first.
#[derive(Default)]
struct RecentCommands {
    newest_first: Vec<usize>,
}

impl RecentCommands {
    const LEN: usize = 3;

    fn push(&mut self, command: usize) {
        self.newest_first.insert(0, command);
        self.newest_first.truncate(Self::LEN);
    }

    fn newest(&self) -> Option<usize> {
        self.newest_first.first().copied()
    }

    /// One of the last three by `weights` (newest first, renormalised over
    /// those that exist), from a uniform `u`.
    fn weighted(&self, u: f64, weights: &[f64; Self::LEN]) -> Option<usize> {
        let weights = &weights[..self.newest_first.len()];
        let total: f64 = weights.iter().sum();
        let mut target = u * total;
        for (command, weight) in self.newest_first.iter().zip(weights) {
            if target < *weight {
                return Some(*command);
            }
            target -= weight;
        }
        self.newest_first.last().copied()
    }
}

/// The last [`WORKING_SET`] distinct commands run, most recent first, across
/// sessions: what a user is "working on" lately.
#[derive(Default)]
struct WorkingSet {
    newest_first: Vec<usize>,
}

impl WorkingSet {
    fn touch(&mut self, command: usize) {
        if let Some(at) = self.newest_first.iter().position(|&c| c == command) {
            self.newest_first.remove(at);
        }
        self.newest_first.insert(0, command);
        self.newest_first.truncate(WORKING_SET);
    }
}

/// When `avoid` is set and `list[pos]` is `previous`, the next entry
/// (wrapping); otherwise `list[pos]`. A one-entry list keeps its entry.
fn avoiding(
    avoid: bool,
    previous: Option<usize>,
    pos: usize,
    len: usize,
    at: impl Fn(usize) -> usize,
) -> usize {
    let pick = at(pos);
    if avoid && Some(pick) == previous && len > 1 {
        at((pos + 1) % len)
    } else {
        pick
    }
}

/// Section 4.3, "For each run": the whole simulation, without any IO.
fn simulate(commands: usize, runs: usize, seed: u64, shape: &Shape) -> Vec<SynthRun> {
    simulate_observed(commands, runs, seed, shape, |_, _, _, _| {})
}

/// [`simulate`], calling `observe(n, world, state, chosen)` for every run `n`
/// that reuses a command, with the state the choice was made from: what the
/// Bayes ceiling of the relevance report reads (04 section 5.5).
fn simulate_observed(
    commands: usize,
    runs: usize,
    seed: u64,
    shape: &Shape,
    mut observe: impl FnMut(usize, &World, &Seen, usize),
) -> Vec<SynthRun> {
    assert!(
        1 <= commands && commands <= runs,
        "need 1 <= commands <= runs"
    );
    let world = World::new(seed);
    let mut rng = Stream { state: seed };

    let mut seen = 0usize;
    // Commands the chain, directory, recent and global branches may choose,
    // in first-run order (lower = more popular).
    let mut reusable: Vec<usize> = Vec::new();
    let mut homed: Vec<Vec<usize>> = vec![Vec::new(); DIRS];
    let mut dir = rng.below(DIRS as u64) as usize;
    let mut session = 0u64;
    let mut recent = RecentCommands::default();
    let mut working = WorkingSet::default();
    let mut t = 0i64;
    let mut retry: Option<usize> = None;
    let mut out = Vec::with_capacity(runs);

    for n in 0..runs {
        // Gap.
        let g = rng.u();
        if g < 0.005 {
            session += 1;
            dir = rng.below(DIRS as u64) as usize;
            recent = RecentCommands::default();
            t += 3600 + rng.below(39_600) as i64;
        } else if g < 0.155 {
            t += 300 + rng.below(1500) as i64;
        } else {
            t += 5 + rng.below(55) as i64;
        }

        // Move.
        if rng.u() < shape.p_move {
            dir = rng.below(DIRS as u64) as usize;
        }

        // New, or a command already seen.
        let p_new = (commands - seen) as f64 / (runs - n) as f64;
        let x = rng.u();
        let command = if seen == 0 || x < p_new {
            let new = seen;
            seen += 1;
            if world.reusable(new, shape.one_off) {
                reusable.push(new);
                let home = world.home(new).map(|home| {
                    if shape.home_where_first_run {
                        dir
                    } else {
                        home
                    }
                });
                if let Some(home) = home {
                    homed[home].push(new);
                }
            }
            new
        } else {
            let state = Seen {
                retry,
                recent: &recent,
                working: &working,
                in_dir: &homed[dir],
                reusable: &reusable,
            };
            let chosen = choose_seen(&world, &mut rng, shape, &state);
            observe(n, &world, &state, chosen);
            chosen
        };

        // Outcome.
        let exit_code = if rng.u() < shape.p_fail { 1 } else { 0 };
        retry = (exit_code != 0).then_some(command);
        let duration_ms = 10 + rng.below(5000) as i64;

        out.push(SynthRun {
            command,
            t,
            dir,
            session,
            exit_code,
            duration_ms,
        });
        recent.push(command);
        if world.reusable(command, shape.one_off) {
            working.touch(command);
        }
    }
    assert_eq!(seen, commands, "p_new reaches 1 when it must");
    out
}

/// What "Else choose" looks at.
struct Seen<'a> {
    retry: Option<usize>,
    recent: &'a RecentCommands,
    working: &'a WorkingSet,
    in_dir: &'a [usize],
    reusable: &'a [usize],
}

/// "Else choose" of section 4.3: retry, repeat, chain, directory, recent or
/// global.
fn choose_seen(world: &World, rng: &mut Stream, shape: &Shape, state: &Seen) -> usize {
    assert!(!state.reusable.is_empty(), "the first command is reusable");
    if let Some(failed) = state.retry
        && rng.u() < shape.p_retry
    {
        return failed;
    }
    let previous = state.recent.newest();
    let avoid = shape.avoid_repeat;
    let c = rng.u();
    let branch = if c < shape.repeat_below {
        // The weighted pick draws only when there is something to pick.
        if previous.is_some() {
            state.recent.weighted(rng.u(), &shape.repeat_weights)
        } else {
            None
        }
    } else if c < shape.chain_below {
        previous
            .and_then(|last| world.successor(last))
            .filter(|&next| world.reusable(next, shape.one_off))
            .filter(|&next| !(avoid && Some(next) == previous))
    } else if c < shape.dir_below {
        let in_dir = state.in_dir;
        (!in_dir.is_empty()).then(|| {
            let pos = zipf(rng.u(), in_dir.len());
            avoiding(avoid, previous, pos, in_dir.len(), |k| in_dir[k])
        })
    } else if c < shape.recent_below {
        // The working set without its newest entry: the previous command
        // when that is reusable (a one-off is never in the working set).
        let older = state.working.newest_first.get(1..).unwrap_or_default();
        (!older.is_empty()).then(|| older[zipf_s(rng.u(), older.len(), shape.recent_s)])
    } else {
        None
    };
    branch.unwrap_or_else(|| {
        let reusable = state.reusable;
        let pos = zipf_s(rng.u(), reusable.len(), shape.zipf_s);
        avoiding(avoid, previous, pos, reusable.len(), |k| reusable[k])
    })
}

/// [`generate_profile`] with the default profile, [`Profile::Interactive`].
#[allow(
    dead_code,
    reason = "the default shape, for tests and the training fixture (Phase C)"
)]
pub(super) fn generate(
    store: &CmdStore,
    commands: usize,
    runs: usize,
    seed: u64,
    anchor: i64,
) -> Result<()> {
    generate_profile(store, commands, runs, seed, anchor, Profile::Interactive)
}

/// Write a seeded synthetic history of `commands` distinct commands and `runs`
/// runs, shaped by `profile`, into `store`, the newest run at `anchor - 60`.
/// Deterministic for a given `(commands, runs, seed, anchor, profile)`.
pub(super) fn generate_profile(
    store: &CmdStore,
    commands: usize,
    runs: usize,
    seed: u64,
    anchor: i64,
    profile: Profile,
) -> Result<()> {
    assert!(
        1 <= commands && commands <= runs,
        "need 1 <= commands <= runs"
    );
    let shape = profile.shape();
    let simulated = simulate(commands, runs, seed, &shape);
    let t_last = simulated.last().expect("runs >= 1").t;
    let shift = anchor - 60 - t_last;

    let world = World::new(seed);
    let texts: Vec<String> = (0..commands)
        .map(|i| world.styled_text(shape.text, i))
        .collect();
    let templates: Vec<String> = texts
        .iter()
        .map(|text| template::templatize(text))
        .collect();
    let dirs: Vec<String> = (0..DIRS).map(dir_path).collect();
    let repos: Vec<String> = (0..DIRS).map(repo_path).collect();

    for batch in simulated.chunks(GENERATE_BATCH) {
        store.transaction(|tx| {
            for run in batch {
                let session = format!("synth:{}", run.session);
                CmdStore::record_run_in(
                    tx,
                    &NewRun {
                        text: &texts[run.command],
                        template: &templates[run.command],
                        started_at: run.t + shift,
                        cwd: Some(&dirs[run.dir]),
                        repo_root: Some(&repos[run.dir]),
                        exit_code: Some(run.exit_code),
                        duration_ms: Some(run.duration_ms),
                        shell_session: &session,
                        source: Source::Hook,
                    },
                )?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Claude Code transcripts (section 4.2)
// ---------------------------------------------------------------------------

/// What `extract_agent` found, printed one line per field. No command text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct AgentReport {
    /// Transcript files read.
    pub files: u64,
    /// Bash `tool_use` blocks seen.
    pub tool_uses: u64,
    /// Runs written.
    pub kept: u64,
    pub skipped_background: u64,
    pub skipped_no_timestamp: u64,
    /// `input.command` missing or not a string.
    pub skipped_malformed: u64,
    /// Empty, too long, or psychic's own command (`track::is_recordable`).
    pub skipped_filtered: u64,
    /// Kept runs that have a matching `tool_result` (so an exit code).
    pub with_result: u64,
}

impl AgentReport {
    pub(super) fn lines(&self) -> Vec<String> {
        vec![
            format!("files: {}", self.files),
            format!("tool uses: {}", self.tool_uses),
            format!("kept: {}", self.kept),
            format!("skipped background: {}", self.skipped_background),
            format!("skipped no timestamp: {}", self.skipped_no_timestamp),
            format!("skipped malformed: {}", self.skipped_malformed),
            format!("skipped filtered: {}", self.skipped_filtered),
            format!("with result: {}", self.with_result),
        ]
    }
}

/// One Bash call to record. Holds command text: no `Debug`.
struct AgentRun {
    text: String,
    cwd: Option<String>,
    started_at: i64,
    started_ms: i64,
    session: String,
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
    /// Index of the transcript in path order, and the line in it: the
    /// tie-break of the global sort.
    file: usize,
    line: usize,
}

/// Every `*.jsonl` file at most 3 directories below `projects_dir`
/// (`<project>/<session>.jsonl` and `<project>/<session>/subagents/<agent>.jsonl`),
/// sorted by path. Symlinks are not followed. A missing directory has none.
fn transcript_files(projects_dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(projects_dir)
        .standard_filters(false)
        .follow_links(false)
        .max_depth(Some(4))
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();
    files
}

/// The milliseconds of an RFC 3339 `timestamp` field, if present and valid.
fn timestamp_ms(line: &Value) -> Option<i64> {
    line.get("timestamp")?
        .as_str()?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|ts| ts.as_millisecond())
}

/// The `message.content` blocks of a line, if it has an array of them.
fn content_blocks(line: &Value) -> &[Value] {
    line.get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Parse one transcript (section 4.2, "Algorithm"). `file_stem` names the
/// session when a line has no `sessionId`. Pure: the caller reads the file.
fn scan_transcript(
    content: &str,
    file: usize,
    file_stem: &str,
    report: &mut AgentReport,
) -> Vec<AgentRun> {
    let mut runs: Vec<AgentRun> = Vec::new();
    // tool_use id -> index into `runs`, until its result arrives.
    let mut pending: FxHashMap<String, usize> = FxHashMap::default();
    let mut last_cwd: Option<String> = None;

    for (line_no, raw) in content.lines().enumerate() {
        if !(raw.contains("\"Bash\"") || raw.contains("tool_result") || raw.contains("\"cwd\"")) {
            continue;
        }
        let Ok(line) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if let Some(cwd) = line.get("cwd").and_then(Value::as_str) {
            last_cwd = Some(cwd.to_string());
        }
        let at_ms = timestamp_ms(&line);
        match line.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                for block in content_blocks(&line) {
                    let is_bash = block.get("type").and_then(Value::as_str) == Some("tool_use")
                        && block.get("name").and_then(Value::as_str) == Some("Bash");
                    if !is_bash {
                        continue;
                    }
                    report.tool_uses += 1;
                    let input = block.get("input");
                    let background = input
                        .and_then(|input| input.get("run_in_background"))
                        .and_then(Value::as_bool)
                        == Some(true);
                    if background {
                        report.skipped_background += 1;
                        continue;
                    }
                    let Some(started_ms) = at_ms else {
                        report.skipped_no_timestamp += 1;
                        continue;
                    };
                    let Some(command) = input
                        .and_then(|input| input.get("command"))
                        .and_then(Value::as_str)
                    else {
                        report.skipped_malformed += 1;
                        continue;
                    };
                    let text = command.trim();
                    if !track::is_recordable(text, None) {
                        report.skipped_filtered += 1;
                        continue;
                    }
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        pending.insert(id.to_string(), runs.len());
                    }
                    runs.push(AgentRun {
                        text: text.to_string(),
                        cwd: last_cwd.clone(),
                        started_at: started_ms.div_euclid(1000),
                        started_ms,
                        session: session_of(&line, file_stem),
                        exit_code: None,
                        duration_ms: None,
                        file,
                        line: line_no,
                    });
                }
            }
            Some("user") => {
                for block in content_blocks(&line) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let Some(index) = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .and_then(|id| pending.remove(id))
                    else {
                        continue;
                    };
                    let run = &mut runs[index];
                    let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                    run.exit_code = Some(if is_error { 1 } else { 0 });
                    run.duration_ms = at_ms
                        .map(|at| at - run.started_ms)
                        .filter(|duration| *duration >= 0);
                }
            }
            _ => {}
        }
    }
    report.with_result += runs.iter().filter(|run| run.exit_code.is_some()).count() as u64;
    runs
}

/// `sessionId`, plus `:<agentId>` on a sub-agent's line: sub-agents share
/// their parent's `sessionId` and may run side by side, so each gets its own
/// shell session, as a second terminal would. The file stem when there is
/// no `sessionId`.
fn session_of(line: &Value, file_stem: &str) -> String {
    let session = line
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or(file_stem);
    match line.get("agentId").and_then(Value::as_str) {
        Some(agent) => format!("{session}:{agent}"),
        None => session.to_string(),
    }
}

/// Read every transcript under `projects_dir` (read-only) and record its
/// Bash calls into `store` in global time order, in one transaction.
pub(super) fn extract_agent(
    projects_dir: &Path,
    home: Option<&Path>,
    store: &CmdStore,
) -> Result<AgentReport> {
    let files = transcript_files(projects_dir);
    let mut report = AgentReport {
        files: files.len() as u64,
        ..AgentReport::default()
    };
    let mut runs = Vec::new();
    for (index, path) in files.iter().enumerate() {
        let bytes =
            std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        runs.extend(scan_transcript(
            &String::from_utf8_lossy(&bytes),
            index,
            &stem,
            &mut report,
        ));
    }
    // A stable sort keeps the block order of one line.
    runs.sort_by_key(|run| (run.started_ms, run.file, run.line));
    report.kept = runs.len() as u64;

    let mut repo_roots: FxHashMap<String, Option<String>> = FxHashMap::default();
    store.transaction(|tx| {
        for run in &runs {
            let repo_root = run.cwd.as_ref().and_then(|cwd| {
                repo_roots
                    .entry(cwd.clone())
                    .or_insert_with(|| {
                        repo::find_repo_root(Path::new(cwd), home)
                            .map(|root| root.to_string_lossy().into_owned())
                    })
                    .clone()
            });
            let template = template::templatize(&run.text);
            CmdStore::record_run_in(
                tx,
                &NewRun {
                    text: &run.text,
                    template: &template,
                    started_at: run.started_at,
                    cwd: run.cwd.as_deref(),
                    repo_root: repo_root.as_deref(),
                    exit_code: run.exit_code,
                    duration_ms: run.duration_ms,
                    shell_session: &format!("agent:{}", run.session),
                    source: Source::Hook,
                },
            )?;
        }
        Ok(())
    })?;
    Ok(report)
}

#[cfg(test)]
mod ceiling;

#[cfg(test)]
mod tests {
    use super::super::store::test_support::query_strings;
    use super::*;

    #[test]
    fn splitmix64_matches_the_reference_vectors() {
        // The reference stream seeded with 0 outputs splitmix64(k * golden)
        // for k = 0, 1, 2 (the function adds the golden ratio itself).
        let golden = 0x9E37_79B9_7F4A_7C15u64;
        assert_eq!(
            [
                splitmix64(0),
                splitmix64(golden),
                splitmix64(golden.wrapping_mul(2))
            ],
            [
                0xE220_A839_7B1D_CDAF,
                0x6E78_9E6A_A1B9_65F4,
                0x06C4_5D18_8009_454F
            ]
        );
    }

    #[test]
    fn zipf_covers_head_and_tail() {
        assert_eq!(zipf(0.0, 10), 0);
        assert_eq!(zipf(0.999_999, 10), 9);
        assert_eq!(zipf(0.5, 1), 0);
        // (11)^0.5 = 3.32 -> 3 - 1 = 2
        assert_eq!(zipf(0.5, 10), 2);
    }

    #[test]
    fn synthetic_text_and_world() {
        assert_eq!(World::text(0), "git status src/file0.rs");
        assert_eq!(World::text(41), "cargo build arg41");
        assert_eq!(World::text(1000), "git status arg1000");
        assert_eq!(dir_path(13), "/synth/repo2/dir3");
        assert_eq!(repo_path(13), "/synth/repo2");
        let world = World::new(1);
        for i in 0..500 {
            assert!(world.successor(i).is_none_or(|next| next <= i));
            assert!(world.home(i).is_none_or(|dir| dir < DIRS));
        }
    }

    #[test]
    fn recent_commands_weights_renormalise() {
        let weights = Profile::Repetitive.shape().repeat_weights;
        let mut recent = RecentCommands::default();
        assert_eq!(recent.weighted(0.5, &weights), None);
        recent.push(7);
        assert_eq!(recent.weighted(0.99, &weights), Some(7));
        recent.push(8); // newest first: 8, 7; weights 0.80, 0.12 of 0.92
        assert_eq!(recent.weighted(0.86, &weights), Some(8));
        assert_eq!(recent.weighted(0.88, &weights), Some(7));
        // Interactive repeats only the newest.
        let newest = Profile::Interactive.shape().repeat_weights;
        assert_eq!(recent.weighted(0.999, &newest), Some(8));
        recent.push(9);
        recent.push(10);
        assert_eq!(recent.newest_first, vec![10, 9, 8]);
    }

    /// The "stats" shares, computed straight from the simulation.
    fn immediate_repeat_share(runs: &[SynthRun]) -> f64 {
        let mut last: FxHashMap<u64, usize> = FxHashMap::default();
        let repeats = runs
            .iter()
            .filter(|run| last.insert(run.session, run.command) == Some(run.command))
            .count();
        repeats as f64 / runs.len() as f64
    }

    #[test]
    fn simulation_has_every_command_once_new() {
        for profile in [Profile::Interactive, Profile::Repetitive, Profile::Agent] {
            let runs = simulate(5, 5, 3, &profile.shape());
            assert_eq!(
                runs.iter().map(|run| run.command).collect::<Vec<_>>(),
                vec![0, 1, 2, 3, 4],
                "commands == runs: every run is a new command"
            );
        }
    }

    // TC-N-03
    #[test]
    fn generator_structure() {
        let store = CmdStore::in_memory();
        let anchor = 1_800_000_000;
        generate_profile(&store, 500, 20_000, 7, anchor, Profile::Repetitive).unwrap();

        let counts = store.counts().unwrap();
        assert_eq!(
            (counts.commands, counts.hook_runs, counts.import_runs),
            (500, 20_000, 0)
        );
        let runs = store.all_runs().unwrap();
        assert_eq!(runs.last().unwrap().started_at, anchor - 60);

        let stats =
            super::super::bench::CorpusStats::compute(&counts, &runs, &store.all_events().unwrap());
        assert!(
            (0.40..=0.60).contains(&stats.immediate_repeat_share),
            "immediate repeat share {}",
            stats.immediate_repeat_share
        );
        let mean_gap =
            (runs.last().unwrap().started_at - runs[0].started_at) as f64 / (runs.len() - 1) as f64;
        assert!((200.0..=400.0).contains(&mean_gap), "mean gap {mean_gap}");

        let cwds = query_strings(&store, "SELECT DISTINCT cwd FROM command_runs");
        assert!(cwds.len() <= 40, "{} cwds", cwds.len());
        let outside = query_strings(
            &store,
            "SELECT COUNT(*) FROM command_runs WHERE substr(cwd, 1, length(repo_root) + 1) != repo_root || '/'",
        );
        assert_eq!(outside, vec!["0"], "every repo_root is a prefix of its cwd");

        // The simulation agrees with what was stored.
        let simulated = simulate(500, 20_000, 7, &Profile::Repetitive.shape());
        assert_eq!(
            immediate_repeat_share(&simulated),
            stats.immediate_repeat_share
        );
    }

    #[test]
    fn generator_with_commands_equal_to_runs() {
        let store = CmdStore::in_memory();
        generate(&store, 50, 50, 2, 1_000_000).unwrap();
        let counts = store.counts().unwrap();
        assert_eq!((counts.commands, counts.hook_runs), (50, 50));
    }

    #[test]
    fn zipf_s_generalises_zipf() {
        for u in [0.0, 0.3, 0.5, 0.77, 0.999_999] {
            assert_eq!(zipf_s(u, 1000, 1.0), zipf(u, 1000));
        }
        // s = 0 is uniform: 1 + 0.5 * 10 = 6 -> 5.
        assert_eq!(zipf_s(0.5, 10, 0.0), 5);
        assert_eq!(zipf_s(0.999_999, 10, 0.0), 9);
        // s = 2: (1 + 0.5 * (1/1001 - 1))^-1 = 1.998 -> 0.
        assert_eq!(zipf_s(0.5, 1000, 2.0), 0);
        assert!(zipf_s(0.999_999, 1000, 2.0) >= 990, "the tail is reachable");
        assert_eq!(zipf_s(0.5, 1, 1.6), 0);
    }

    #[test]
    fn every_style_spells_each_command_once() {
        let world = World::new(1);
        for style in [
            TextStyle::ThreeWords,
            TextStyle::Interactive,
            TextStyle::Agent,
        ] {
            let texts: rustc_hash::FxHashSet<String> =
                (0..3000).map(|i| world.styled_text(style, i)).collect();
            assert_eq!(texts.len(), 3000);
            assert!(
                texts.iter().all(|text| !template::is_own_command(text)),
                "none is psychic's own"
            );
        }
        assert_eq!(world.styled_text(TextStyle::Interactive, 6), "ls");
        assert_eq!(world.styled_text(TextStyle::Interactive, 7), "cat");
        for i in 8..3000 {
            let text = world.styled_text(TextStyle::Interactive, i);
            let pair = format!("{} {}", TOOLS[i % 40], SUBS[(i / 40) % 25]);
            assert!(
                (i < 1000 && text == pair) || text.starts_with(&World::text(i)),
                "{i}: {text}"
            );
        }
        assert!(
            world
                .styled_text(TextStyle::Agent, 5)
                .starts_with("cd /synth/repo5 && make status arg5 --message \"")
        );
    }

    #[test]
    fn working_set_moves_to_front_and_is_bounded() {
        let mut working = WorkingSet::default();
        for command in [1, 2, 3, 2] {
            working.touch(command);
        }
        assert_eq!(working.newest_first, vec![2, 3, 1]);
        for command in 0..1000 {
            working.touch(command);
        }
        assert_eq!(working.newest_first.len(), WORKING_SET);
        assert_eq!(working.newest_first[0], 999);
    }

    /// FNV-1a over every simulated run, as text.
    fn digest(runs: &[SynthRun]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for run in runs {
            let line = format!(
                "{},{},{},{},{},{};",
                run.command, run.t, run.dir, run.session, run.exit_code, run.duration_ms
            );
            for byte in line.bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn the_repetitive_profile_is_the_d0_generator() {
        // The digest of `simulate(500, 20_000, 7)` before profiles existed
        // (commit d65f6aa); its texts are `World::text`, checked above.
        let runs = simulate(500, 20_000, 7, &Profile::Repetitive.shape());
        assert_eq!(digest(&runs), 0xd62d_4f1c_e841_ad5f);
    }

    /// `stats` of a profile's history, both blocks.
    fn profile_stats(
        profile: Profile,
        commands: usize,
        runs: usize,
    ) -> (
        super::super::bench::CorpusStats,
        super::super::bench::ShapeStats,
    ) {
        let store = CmdStore::in_memory();
        generate_profile(&store, commands, runs, 7, 1_800_000_000, profile).unwrap();
        let all_runs = store.all_runs().unwrap();
        (
            super::super::bench::CorpusStats::compute(
                &store.counts().unwrap(),
                &all_runs,
                &store.all_events().unwrap(),
            ),
            super::super::bench::ShapeStats::compute(&all_runs, &store.all_commands().unwrap()),
        )
    }

    // TC-N-12
    #[test]
    fn the_interactive_profile_is_near_the_real_history() {
        let (stats, shape) = profile_stats(Profile::Interactive, 2000, 20_000);
        assert_eq!((stats.commands, stats.runs), (2000, 20_000));
        let checks = [
            (
                "immediate repeat share",
                stats.immediate_repeat_share,
                0.04,
                0.10,
            ),
            ("top-10 share", shape.top10_share, 0.30, 0.50),
            ("commands run once", shape.once_only_commands, 0.40, 0.80),
            ("same directory", shape.same_dir_share, 0.60, 0.90),
        ];
        for (name, value, low, high) in checks {
            assert!((low..=high).contains(&value), "{name} {value}");
        }
        let [_, p50, _, p90] = shape.gap_runs;
        assert!((3..=20).contains(&p50), "gap p50 {p50}");
        assert!(p90 >= 30, "gap p90 {p90}");
        let [_, l50, _] = shape.length_chars;
        assert!((4..=20).contains(&l50), "length p50 {l50}");
    }

    // TC-N-13
    #[test]
    fn the_agent_profile_hardly_repeats() {
        let (stats, shape) = profile_stats(Profile::Agent, 1900, 2000);
        assert!(
            stats.immediate_repeat_share < 0.01,
            "{}",
            stats.immediate_repeat_share
        );
        assert!(shape.top10_share < 0.03, "{}", shape.top10_share);
        assert!(shape.length_chars[1] >= 100, "{:?}", shape.length_chars);
        // At ten runs per command it is still flat: no heavy head.
        let (stats, shape) = profile_stats(Profile::Agent, 2000, 20_000);
        assert!(
            stats.immediate_repeat_share < 0.01,
            "{}",
            stats.immediate_repeat_share
        );
        assert!(shape.top10_share < 0.10, "{}", shape.top10_share);
    }

    fn dump(store: &CmdStore) -> Vec<String> {
        query_strings(
            store,
            "SELECT c.text, r.started_at, r.cwd, r.exit_code FROM command_runs r \
             JOIN commands c ON c.id = r.command_id ORDER BY r.id",
        )
    }

    // TC-N-04
    #[test]
    fn generator_determinism() {
        let (a, b, c) = (
            CmdStore::in_memory(),
            CmdStore::in_memory(),
            CmdStore::in_memory(),
        );
        generate(&a, 200, 3000, 7, 1_800_000_000).unwrap();
        generate(&b, 200, 3000, 7, 1_800_000_000).unwrap();
        generate(&c, 200, 3000, 8, 1_800_000_000).unwrap();
        assert_eq!(dump(&a).len(), 3000);
        assert!(dump(&a) == dump(&b), "same arguments, same history");
        assert!(dump(&a) != dump(&c), "another seed, another history");
    }

    // The fixture of TC-N-05: 8 assistant lines and 2 result lines. Synthetic.
    const FIXTURE: &str = r#"{"type":"assistant","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:00:00.000Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"u1","input":{"command":"make test"}}]}}
{"type":"user","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:00:02.500Z","message":{"content":[{"type":"tool_result","tool_use_id":"u1","is_error":false}]}}
{"type":"assistant","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:01:00.000Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"u2","input":{"command":"make lint"}}]}}
{"type":"user","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:01:01.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"u2","is_error":true}]}}
{"type":"assistant","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:02:00.000Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"u3","input":{"command":"sleep 100","run_in_background":true}}]}}
{"type":"assistant","cwd":"/work/p1","sessionId":"s1","message":{"content":[{"type":"tool_use","name":"Bash","id":"u4","input":{"command":"echo no time"}}]}}
{"type":"assistant","sessionId":"s1","timestamp":"2026-11-01T10:03:00.000Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"u5","input":{"command":"ls -la"}}]}}
{"type":"assistant","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:04:00.000Z","message":{"content":[{"type":"tool_use","name":"Read","id":"u6","input":{"file_path":"/work/p1/x"}}]}}
{"type":"assistant","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:05:00.000Z","message":{"content":[{"type":"tool_use","name":"Bash" BROKEN
{"type":"assistant","cwd":"/work/p1","sessionId":"s1","timestamp":"2026-11-01T10:06:00.000Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"u8","input":{"command":"cargo build"}}]}}
"#;

    /// `(text, cwd, exit_code, duration_ms)` of each run.
    type Seen = (String, Option<String>, Option<i32>, Option<i64>);

    fn seen(runs: &[AgentRun]) -> Vec<Seen> {
        runs.iter()
            .map(|run| {
                (
                    run.text.clone(),
                    run.cwd.clone(),
                    run.exit_code,
                    run.duration_ms,
                )
            })
            .collect()
    }

    // TC-N-05
    #[test]
    fn transcript_parsing() {
        let mut report = AgentReport::default();
        let runs = scan_transcript(FIXTURE, 0, "stem", &mut report);
        let p1 = Some("/work/p1".to_string());
        assert_eq!(
            seen(&runs),
            vec![
                ("make test".to_string(), p1.clone(), Some(0), Some(2500)),
                ("make lint".to_string(), p1.clone(), Some(1), Some(1000)),
                ("ls -la".to_string(), p1.clone(), None, None),
                ("cargo build".to_string(), p1.clone(), None, None),
            ]
        );
        assert_eq!(runs[0].started_at, 1_793_527_200);
        assert_eq!(runs[0].session, "s1");
        assert_eq!(
            report,
            AgentReport {
                files: 0,
                tool_uses: 6,
                kept: 0,
                skipped_background: 1,
                skipped_no_timestamp: 1,
                skipped_malformed: 0,
                skipped_filtered: 0,
                with_result: 2,
            }
        );
    }

    #[test]
    fn transcript_edge_cases() {
        let lines = [
            // command is not a string
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-11-01T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"a","input":{"command":42}}]}}"#,
            // psychic's own command, and an empty one
            r#"{"type":"assistant","timestamp":"2026-11-01T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"b","input":{"command":"psychic history"}},{"type":"tool_use","name":"Bash","id":"c","input":{"command":"  "}}]}}"#,
            // two uses on one line, a sub-agent, no sessionId on the first
            r#"{"type":"assistant","cwd":"/w","timestamp":"2026-11-01T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"d","input":{"command":"one"}},{"type":"tool_use","name":"Bash","id":"e","input":{"command":"two"}}]}}"#,
            r#"{"type":"assistant","sessionId":"s","agentId":"a7","timestamp":"2026-11-01T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"f","input":{"command":"three"}}]}}"#,
            // a result before its use is ignored; a result timestamp before the use gives no duration
            r#"{"type":"user","timestamp":"2026-11-01T09:00:00Z","message":{"content":[{"type":"tool_result","tool_use_id":"d","is_error":false}]}}"#,
            r#"{"type":"user","message":{"content":"plain text, not blocks"}}"#,
        ];
        let mut report = AgentReport::default();
        let runs = scan_transcript(&lines.join("\n"), 0, "stem", &mut report);
        assert_eq!(
            runs.iter()
                .map(|run| (run.text.as_str(), run.session.as_str()))
                .collect::<Vec<_>>(),
            vec![("one", "stem"), ("two", "stem"), ("three", "s:a7")]
        );
        assert_eq!((runs[0].exit_code, runs[0].duration_ms), (Some(0), None));
        assert_eq!(
            (
                report.tool_uses,
                report.skipped_malformed,
                report.skipped_filtered
            ),
            (6, 1, 2)
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("psychic-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn mtime(path: &Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    // IT-N-03
    #[test]
    fn agent_extraction() {
        let root = scratch("corpus-agent");
        let projects = root.join("projects");
        let sub = projects.join("p1/s1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        let main = projects.join("p1/s1.jsonl");
        std::fs::write(&main, FIXTURE).unwrap();
        // The sub-agent works between the main transcript's first and second calls.
        let agent = sub.join("agent-a9.jsonl");
        std::fs::write(
            &agent,
            [
                r#"{"type":"assistant","cwd":"/work/p1","sessionId":"s1","agentId":"a9","timestamp":"2026-11-01T10:00:30.000Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"v1","input":{"command":"git status"}}]}}"#,
                r#"{"type":"assistant","cwd":"/work/p1","sessionId":"s1","agentId":"a9","timestamp":"2026-11-01T10:00:40.000Z","message":{"content":[{"type":"tool_use","name":"Bash","id":"v2","input":{"command":"git diff"}}]}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(projects.join("p1/notes.txt"), "not a transcript").unwrap();
        let before = (mtime(&main), mtime(&agent));

        let store = CmdStore::open(&root.join("data")).unwrap();
        let report = extract_agent(&projects, None, &store).unwrap();
        assert_eq!(
            (
                report.files,
                report.tool_uses,
                report.kept,
                report.with_result
            ),
            (2, 8, 6, 2)
        );
        assert_eq!(
            query_strings(
                &store,
                "SELECT c.text, r.shell_session, r.source, ifnull(p.text, '-') FROM command_runs r \
                 JOIN commands c ON c.id = r.command_id LEFT JOIN commands p ON p.id = r.prev_command_id \
                 ORDER BY r.started_at, r.id"
            ),
            vec![
                "make test|agent:s1|hook|-",
                "git status|agent:s1:a9|hook|-",
                "git diff|agent:s1:a9|hook|git status",
                "make lint|agent:s1|hook|make test",
                "ls -la|agent:s1|hook|make lint",
                "cargo build|agent:s1|hook|ls -la",
            ]
        );
        assert_eq!(
            before,
            (mtime(&main), mtime(&agent)),
            "transcripts untouched"
        );
        drop(store);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_projects_dir_has_no_transcripts() {
        assert!(transcript_files(Path::new("/nonexistent/psychic/projects")).is_empty());
    }
}
