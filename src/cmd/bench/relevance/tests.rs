//! Tests of the relevance benchmark: `docs/ctrl-r/04-bench.md` section 11
//! (TC-N-01, 02, 06, 07, 08, 11; IT-N-01, 02, 04, 05, 06) and the leakage
//! checks of section 5.7.

use super::super::super::corpus;
use super::super::super::features::{COMMAND_FEATURE_REGISTRY, CommandFeatureInputs, compute_all};
use super::super::super::index::{CommandIndex, LoadParams};
use super::super::super::store::test_support::hook_run;
use super::super::super::store::{CommandRecord, EventRecord, RunRecord};
use super::super::super::train::test_support::{FIXTURE_ANCHOR, TempDir};
use super::super::super::train::{build_training_set, replay};
use super::super::super::tuning::CMD_RECENCY_RANK_CAP;
use super::super::{BenchCommand, BenchEnv};
use super::*;

const ANCHOR: i64 = FIXTURE_ANCHOR;

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-4
}

fn env() -> BenchEnv {
    BenchEnv {
        now: ANCHOR,
        home: None,
        claude_config_dir: None,
    }
}

fn relevance(json: Option<PathBuf>, md: Option<PathBuf>) -> BenchCommand {
    BenchCommand::Relevance {
        seeds: 2,
        max_test_runs: 200,
        folds: false,
        json,
        md,
        label: String::new(),
        strict: false,
        shuffle_labels: false,
    }
}

fn run_bench(data_dir: &Path, command: BenchCommand) -> String {
    let mut out = Vec::new();
    super::super::run(data_dir, command, &env(), &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// IT-N-01's corpus: 300 commands, 6,000 runs, seed 3.
fn generate(data_dir: &Path) {
    let store = CmdStore::open(data_dir).unwrap();
    corpus::generate(&store, 300, 6000, 3, ANCHOR).unwrap();
}

// --- TC-N-01, TC-N-02: metrics -------------------------------------------------

#[test]
fn tc_n_01_metrics_of_a_rank_list() {
    let m = Metrics::of(&[1, 2, 4, 11]);
    assert_eq!(m.n, 4);
    assert!(close(m.top1, 0.25) && close(m.top3, 0.5) && close(m.top10, 0.75));
    assert!(close(m.mrr, (1.0 + 0.5 + 0.25 + 1.0 / 11.0) / 4.0));
    assert!(close(m.mrr, 0.4602));

    let empty = Metrics::of(&[]);
    assert_eq!(empty.n, 0);
    assert!(empty.top1.is_nan() && empty.mrr.is_nan());
    assert_eq!(fmt3(empty.top1), "-");

    // A target that did not match is a miss in every metric.
    let missed = Metrics::of(&[NOT_MATCHED, 1]);
    assert!(close(missed.top10, 0.5) && close(missed.mrr, 0.5));
}

#[test]
fn tc_n_02_seed_aggregation() {
    let s = Spread::of(&[0.50, 0.52, 0.54]);
    assert!(close(s.mean, 0.52) && close(s.sd, 0.02));
    assert!(close(s.min, 0.50) && close(s.max, 0.54));
    let one = Spread::of(&[0.7]);
    assert_eq!((one.mean, one.sd, one.min, one.max), (0.7, 0.0, 0.7, 0.7));
}

// --- TC-N-06: systematic sample --------------------------------------------------

fn run_record(
    id: i64,
    command_id: i64,
    started_at: i64,
    cwd: Option<&str>,
    session: &str,
) -> RunRecord {
    RunRecord {
        id,
        command_id,
        started_at,
        cwd: cwd.map(str::to_string),
        repo_root: None,
        exit_code: Some(0),
        shell_session: session.to_string(),
        prev_command_id: None,
        source: Source::Hook,
    }
}

fn command(id: i64, text: &str) -> CommandRecord {
    CommandRecord {
        id,
        text: text.to_string(),
        template: text.to_string(),
    }
}

#[test]
fn tc_n_06_every_stride_th_reachable_run_is_sampled() {
    assert_eq!(stride(10, 3), 3);
    assert_eq!(stride(10, 10), 1);
    assert_eq!(stride(10, 50), 1);

    // Run 1 before the cut makes command 1 reachable; runs 2-11 are its
    // ten reachable test runs; command 2's first run is unreachable.
    let mut runs = vec![run_record(1, 1, 100, Some("/w"), "s")];
    runs.extend((0..10).map(|i| run_record(2 + i, 1, 1000 + i * 10, Some("/w"), "s")));
    runs.push(run_record(12, 2, 2000, Some("/w"), "s"));
    let input = ReplayInput {
        commands: vec![command(1, "ls"), command(2, "pwd")],
        runs,
        events: Vec::new(),
    };
    let known: FxHashSet<i64> = [1, 2].into_iter().collect();
    let (reach, sampled) = sample_window(&input, &known, 1000, None, 3);
    assert_eq!(reach.reachable, 10);
    assert_eq!(reach.unreachable_hook, 1);
    let mut ids: Vec<i64> = sampled.into_iter().collect();
    ids.sort_unstable();
    assert_eq!(ids, [2, 5, 8, 11], "ordinals 0, 3, 6, 9");

    let (_, all) = sample_window(&input, &known, 1000, None, 10);
    assert_eq!(all.len(), 10, "max_test_runs >= reachable keeps every one");
}

// --- TC-N-07: slices -------------------------------------------------------------

fn facts(target_age: i64, run_dir: Option<u32>, target_dir: Option<u32>) -> LookupFacts {
    LookupFacts {
        started_at: 10_000,
        target_last_run_at: 10_000 - target_age,
        synthetic_time: false,
        cwd_known: true,
        run_dir,
        target_last_dir: target_dir,
        source: Source::Hook,
        session_repeat: false,
    }
}

fn has(bits: u8, slice: Slice) -> bool {
    bits & slice.bit() != 0
}

#[test]
fn tc_n_07_slices() {
    let a = slices_of(&facts(601, Some(1), Some(2)));
    assert!(has(a, Slice::All) && has(a, Slice::Gt10min) && has(a, Slice::OtherDir));
    assert!(has(a, Slice::Hook) && !has(a, Slice::Import) && !has(a, Slice::Repeat));

    let b = slices_of(&facts(600, Some(1), Some(1)));
    assert!(!has(b, Slice::Gt10min) && !has(b, Slice::OtherDir));

    let synthetic = slices_of(&LookupFacts {
        synthetic_time: true,
        ..facts(5000, Some(1), Some(1))
    });
    assert!(!has(synthetic, Slice::Gt10min));

    let unknown_cwd = slices_of(&LookupFacts {
        cwd_known: false,
        ..facts(601, None, Some(2))
    });
    assert!(
        !has(unknown_cwd, Slice::OtherDir),
        "unknown run.cwd is never other_dir"
    );

    let new_dir = slices_of(&facts(601, None, Some(2)));
    assert!(
        has(new_dir, Slice::OtherDir),
        "a cwd never seen differs from the target's"
    );
    let no_previous_dir = slices_of(&facts(601, Some(1), None));
    assert!(!has(no_previous_dir, Slice::OtherDir));

    let repeat = slices_of(&LookupFacts {
        session_repeat: true,
        source: Source::Import,
        ..facts(5, Some(1), Some(1))
    });
    assert!(has(repeat, Slice::Repeat) && has(repeat, Slice::Import));
}

// --- TC-N-08: verdicts ------------------------------------------------------------

fn cell(ranker: Ranker, slice: Slice, len: usize, n: usize, top1: f64) -> Cell {
    let v = Spread::of(&[top1]);
    Cell {
        ranker,
        slice,
        style: QueryStyle::Prefix,
        len,
        n,
        values: [v, v, v, v],
    }
}

/// Every headline and R3 cell with `value` for every ranker, then overrides.
fn grid(n: usize, overrides: &[Cell]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for ranker in RANKERS {
        for slice in SLICES {
            for len in LENGTHS {
                let o = overrides
                    .iter()
                    .find(|c| c.ranker == ranker && c.slice == slice && c.len == len);
                cells.push(
                    o.cloned()
                        .unwrap_or_else(|| cell(ranker, slice, len, n, 0.5)),
                );
            }
        }
    }
    cells
}

#[test]
fn tc_n_08_verdicts() {
    let cells = grid(
        300,
        &[
            cell(Ranker::Model, Slice::Gt10min, 2, 300, 0.62),
            cell(Ranker::Model, Slice::All, 0, 300, 0.48),
        ],
    );
    let v = verdicts(&cells, 5);
    assert_eq!(v.r1.status, Status::Pass);
    assert!(close(v.r1.value, 0.12));
    assert_eq!(v.r2.status, Status::Fail);
    assert_eq!(v.r2.cell, "all/len0");
    assert!(close(v.r2.value, -0.02));
    assert_eq!(v.r3.status, Status::Pass);
    assert!(!v.all_pass());

    let low = grid(
        300,
        &[
            cell(Ranker::Model, Slice::Gt10min, 2, 150, 0.62),
            cell(Ranker::Recency, Slice::Gt10min, 2, 150, 0.5),
        ],
    );
    assert_eq!(verdicts(&low, 5).r1.status, Status::Inconclusive);
    assert_eq!(
        verdicts(
            &grid(300, &[cell(Ranker::Model, Slice::Gt10min, 2, 300, 0.62)]),
            2
        )
        .r1
        .status,
        Status::Inconclusive,
        "fewer than 3 seeds"
    );

    // A heuristic 0.001 below on one cell still passes R3 (bar -0.001); 0.002 fails.
    let r3 = |top1: f64| {
        verdicts(
            &grid(
                300,
                &[cell(Ranker::Heuristic, Slice::OtherDir, 3, 300, top1)],
            ),
            5,
        )
        .r3
    };
    assert_eq!(r3(0.4995).status, Status::Pass);
    let failed = r3(0.498);
    assert_eq!(failed.status, Status::Fail);
    assert_eq!(failed.cell, "other_dir/len3/top1");
}

#[test]
fn verdicts_read_prefix_cells_only() {
    // A fragment cell far below the baseline does not move R2 or R3.
    let mut cells = grid(300, &[cell(Ranker::Model, Slice::Gt10min, 2, 300, 0.62)]);
    for ranker in [Ranker::Model, Ranker::Heuristic] {
        cells.insert(
            0,
            Cell {
                style: QueryStyle::Fragment,
                ..cell(ranker, Slice::All, 2, 300, 0.0)
            },
        );
    }
    let v = verdicts(&cells, 5);
    assert_eq!(v.r2.status, Status::Pass);
    assert_eq!(v.r3.status, Status::Pass);
}

// --- TC-N-16: fragment queries ---------------------------

#[test]
fn tc_n_16_fragment_queries_start_a_later_word() {
    let text = "git commit --amend 'fix it'";
    // Words after the first, from their first letter or digit.
    let words = ["commit", "amend", "fix", "it"];
    for pick in 0..4u64 {
        let word = words[pick as usize];
        assert_eq!(fragment_query(text, 1, pick).unwrap(), &word[..1]);
        assert_eq!(fragment_query(text, 2, pick).unwrap(), &word[..2]);
        // The same word at every length; too short a word gives none.
        assert_eq!(
            fragment_query(text, 5, pick),
            (word.len() >= 5).then(|| word[..5].to_string())
        );
    }
    assert_eq!(fragment_query(text, 2, 4).unwrap(), "co", "pick wraps");
    assert_eq!(fragment_query("ls", 1, 0), None, "one word");
    assert_eq!(fragment_query("ls | -", 1, 0), None, "no letter or digit");
    assert_eq!(
        fragment_query("echo héllo", 2, 0).unwrap(),
        "hé",
        "characters"
    );
    assert_eq!(make_query(QueryStyle::Prefix, "ab", 3, 1), None);
    assert_eq!(make_query(QueryStyle::Prefix, "abc", 0, 1).unwrap(), "");
}

#[test]
fn the_fzf_baseline_orders_by_fzf_score_then_newest() {
    // "gi": word starts in "git log" and "tig git" (equal under the history
    // scheme), scattered in "grep -i"; "ls" does not match at all, so the
    // filter does not pass it.
    let mut index = CommandIndex::default();
    let texts = [
        ("git log", 10),
        ("tig git", 30),
        ("grep -i", 40),
        ("ls", 50),
    ];
    for (i, &(text, last_run_at)) in texts.iter().enumerate() {
        let idx = index.add_command(i as i64 + 1, text);
        index.record_run(idx, last_run_at, None, None, Some(0), None);
    }
    let ctx = crate::cmd::index::test_support::context(100);
    let candidates = filter_candidates(&index, &ctx, "gi", Scope::All, None);
    let ranks: Vec<u32> = (0..4).map(|t| fzf_rank(&index, &candidates, t)).collect();
    assert_eq!(ranks, [2, 1, 3, NOT_MATCHED]);
    assert_eq!(fzf_rank(&index, &candidates[..1], 1), NOT_MATCHED);
    // No query: newest first.
    let all = filter_candidates(&index, &ctx, "", Scope::All, None);
    assert_eq!(fzf_rank(&index, &all, 3), 1);
}

// --- TC-N-11: baselines -----------------------------------------------------------

#[test]
fn tc_n_11_baseline_orders() {
    let mut index = CommandIndex::default();
    let data = [(10, 5), (30, 5), (20, 9)];
    let mut candidates = Vec::new();
    for (i, &(last_run_at, skim)) in data.iter().enumerate() {
        let idx = index.add_command(i as i64 + 1, &format!("c{i}"));
        index.record_run(idx, last_run_at, None, None, Some(0), None);
        candidates.push(SkimScored { idx, skim });
    }
    let order = |candidates: &[SkimScored], cmp: &dyn Fn(&SkimScored, &SkimScored) -> Ordering| {
        let mut sorted = candidates.to_vec();
        sorted.sort_by(|a, b| cmp(a, b));
        sorted.iter().map(|c| c.idx).collect::<Vec<_>>()
    };
    assert_eq!(
        order(&candidates, &|a, b| recency_order(&index, a, b)),
        [1, 2, 0]
    );
    assert_eq!(
        order(&candidates, &|a, b| fzf_scored_order(&index, a, b)),
        [2, 1, 0]
    );
    assert_eq!(
        rank_by(&candidates, 0, |a, b| recency_order(&index, a, b)),
        3
    );
    assert_eq!(
        rank_by(&candidates, 2, |a, b| fzf_scored_order(&index, a, b)),
        1
    );

    // Equal last_run_at: the larger database id first.
    let tie = index.add_command(9, "c9");
    index.record_run(tie, 30, None, None, Some(0), None);
    candidates.push(SkimScored { idx: tie, skim: 5 });
    assert_eq!(
        order(&candidates, &|a, b| recency_order(&index, a, b))[..2],
        [tie, 1]
    );
}

// --- Leakage (section 5.7) --------------------------------------------------------

#[test]
fn a_lookup_is_ranked_before_its_own_run_is_applied() {
    // `ls` last ran at 100, `pwd` at 500; the test run of `ls` at 1000 must
    // see `pwd` as newer (recency rank 2). Applying the run first would make
    // it rank 1.
    let input = ReplayInput {
        commands: vec![command(1, "ls"), command(2, "pwd")],
        runs: vec![
            run_record(1, 1, 100, Some("/a"), "s"),
            run_record(2, 2, 500, Some("/b"), "s"),
            run_record(3, 1, 1000, Some("/b"), "s"),
        ],
        events: Vec::new(),
    };
    let replayer = replay(&input, Some(1000));
    let sampled: FxHashSet<i64> = [3].into_iter().collect();
    let ranks = replay_window(&input, replayer, &[], 1000, None, &sampled);
    let lookup = &ranks.lookups[0][0];
    assert_eq!((lookup.recency, lookup.heuristic), (2, 2));
    assert!(has(lookup.slices, Slice::Gt10min), "last run 900 s before");
    assert!(
        has(lookup.slices, Slice::OtherDir),
        "ran in /a, looked up in /b"
    );
    assert!(
        !has(lookup.slices, Slice::Repeat),
        "pwd was the session's last"
    );
}

#[test]
fn training_at_a_cut_sees_nothing_after_it() {
    // The same model text from the whole history cut at t as from the
    // history with everything at or after t removed.
    let dir = TempDir::new("bench-cut");
    generate(&dir.0);
    let full = ReplayInput::read(&CmdStore::open(&dir.0).unwrap()).unwrap();
    let cut = time_at(&full.runs, 0.8);
    let mut truncated = ReplayInput::read(&CmdStore::open(&dir.0).unwrap()).unwrap();
    truncated.runs.retain(|r| r.started_at < cut);
    truncated.events.clear();
    let fit = |input: &ReplayInput| {
        let options = BuildOptions {
            seed: 1,
            max_run_groups: CMD_TRAIN_MAX_RUN_GROUPS,
            until: Some(cut),
            shuffle_labels: false,
        };
        let built = build_training_set(input, &options);
        crate::cmd::train::fit_command_model(&built.set, 1)
            .unwrap()
            .model
    };
    assert_eq!(fit(&full), fit(&truncated));
}

#[test]
fn shuffled_labels_change_which_rows_are_positive_but_not_how_many() {
    let dir = TempDir::new("bench-shuffle");
    generate(&dir.0);
    let input = ReplayInput::read(&CmdStore::open(&dir.0).unwrap()).unwrap();
    let options = |shuffle_labels| BuildOptions {
        seed: 1,
        max_run_groups: 500,
        until: None,
        shuffle_labels,
    };
    let real = build_training_set(&input, &options(false));
    let shuffled = build_training_set(&input, &options(true));
    assert_eq!(real.counts, shuffled.counts);
    let real = crate::cmd::train::fit_command_model(&real.set, 1).unwrap();
    let shuffled = crate::cmd::train::fit_command_model(&shuffled.set, 1).unwrap();
    assert_ne!(real.model, shuffled.model);
}

// --- IT-N-01, IT-N-05, IT-N-06: end to end -----------------------------------------

/// Size and mtime of every file under `dir`.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let meta = entry.metadata().unwrap();
            files.push((entry.path(), meta.len(), meta.modified().unwrap()));
        }
    }
    files.sort();
    files
}

#[test]
fn it_n_01_and_05_relevance_end_to_end_leaves_the_real_data_dir_alone() {
    let real = crate::cli::get_default_data_dir().ok();
    let real_existed = real.as_ref().map(|r| r.exists());
    let before = real.as_deref().map(snapshot);

    let dir = TempDir::new("bench-it-n-01");
    generate(&dir.0);
    let json_path = dir.0.join("out.json");
    let first = run_bench(&dir.0, relevance(Some(json_path.clone()), None));
    assert!(first.contains("## Verdicts"));
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    let cells = value["splits"]["holdout"]["cells"].as_array().unwrap();
    for ranker in ["recency", "fzf_scored", "fzf", "heuristic", "model"] {
        for len in 0..4 {
            let cell = cells
                .iter()
                .find(|c| c["ranker"] == ranker && c["slice"] == "all" && c["len"] == len)
                .unwrap_or_else(|| panic!("{ranker} len {len}"));
            assert!(cell["n"].as_u64().unwrap() > 0);
            if ranker != "model" {
                assert_eq!(cell["top1"]["sd"], 0.0);
            }
        }
    }
    // The fragment style and the information rows are there too.
    for (ranker, style, len) in [
        ("model", "fragment", 5),
        ("stage1_recall", "prefix", 2),
        ("fzf", "fragment", 1),
    ] {
        let cell = cells
            .iter()
            .find(|c| {
                c["ranker"] == ranker
                    && c["slice"] == "all"
                    && c["style"] == style
                    && c["len"] == len
            })
            .unwrap_or_else(|| panic!("{ranker} {style} len {len}"));
        let top1 = cell["top1"]["mean"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&top1), "{ranker}: {top1}");
    }
    let unreachable = value["unreachable"]["all"].as_f64().unwrap();
    assert!((0.0..=1.0).contains(&unreachable));
    for verdict in ["R1", "R2", "R3"] {
        assert!(value["verdicts"][verdict]["status"].is_string());
    }
    assert_eq!(value["splits"]["holdout"]["seeds"], 2);
    assert!(dir.0.join("bench/holdout-seed2/cmd_model.txt").exists());

    // A second run is byte-identical apart from the date.
    let second_path = dir.0.join("second.json");
    let second = run_bench(&dir.0, relevance(Some(second_path.clone()), None));
    assert_eq!(first, second);
    let strip_date = |text: String| -> Value {
        let mut value: Value = serde_json::from_str(&text).unwrap();
        value["date"] = Value::Null;
        value
    };
    assert_eq!(
        strip_date(std::fs::read_to_string(&json_path).unwrap()),
        strip_date(std::fs::read_to_string(&second_path).unwrap())
    );

    // IT-N-05: the real data directory is untouched.
    if let Some(real) = real {
        assert_eq!(Some(real.exists()), real_existed);
        assert_eq!(Some(snapshot(&real)), before);
    }
}

#[test]
fn it_n_06_no_command_text_leaves_the_benchmark() {
    const SENTINEL: &str = "SENTINEL_c0ffee";
    let dir = TempDir::new("bench-it-n-06");
    generate(&dir.0);
    {
        let store = CmdStore::open(&dir.0).unwrap();
        // Spread over the timeline, so it is trained on and sampled.
        for k in 0..40 {
            store
                .record_run(&hook_run(
                    SENTINEL,
                    "synth:sentinel",
                    ANCHOR - 50 - k * 30_000,
                    Some(0),
                ))
                .unwrap();
        }
    }
    let (json, md) = (dir.0.join("out.json"), dir.0.join("out.md"));
    let stdout = run_bench(&dir.0, relevance(Some(json.clone()), Some(md.clone())));
    assert!(!stdout.contains(SENTINEL));
    assert!(!std::fs::read_to_string(&json).unwrap().contains(SENTINEL));
    assert!(!std::fs::read_to_string(&md).unwrap().contains(SENTINEL));
}

// --- IT-N-02: the replay state equals a fresh load ----------------------------------

#[test]
fn it_n_02_replay_state_equals_a_fresh_load() {
    let dir = TempDir::new("bench-it-n-02");
    generate(&dir.0);
    let store = CmdStore::open(&dir.0).unwrap();
    let input = ReplayInput::read(&store).unwrap();
    let replayer = replay(&input, None);
    let newest = input.runs.last().unwrap();
    let now = newest.started_at + 1;
    let (cwd, repo, session) = (
        newest.cwd.clone().unwrap(),
        newest.repo_root.clone(),
        newest.shell_session.clone(),
    );
    let (loaded, loaded_ctx) = CommandIndex::load(
        &store,
        &LoadParams {
            now,
            cwd: Some(cwd.clone()),
            repo_root: repo.clone(),
            shell_session: Some(session.clone()),
        },
    )
    .unwrap();
    let replay_ctx = replayer.context(now, Some(&cwd), repo.as_deref(), Some(&session));
    for query in ["", "g"] {
        let order = |index: &CommandIndex, ctx: &RankContext| -> Vec<i64> {
            let candidates = filter_candidates(index, ctx, query, Scope::All, None);
            rank_commands(query, &candidates, index, ctx, usize::MAX, None)
                .scores
                .iter()
                .map(|s| index.entries[s.idx as usize].db_id)
                .collect()
        };
        let from_replay = order(&replayer.index, &replay_ctx);
        assert!(from_replay.len() > 10, "query {query:?} matches");
        assert_eq!(from_replay, order(&loaded, &loaded_ctx), "query {query:?}");

        // The newest matching commands as training finds them (a walk of
        // the replay's recency order) equal what serving computes from its
        // candidates.
        let newest = |index: &CommandIndex, ctx: &RankContext| {
            let candidates = filter_candidates(index, ctx, query, Scope::All, None);
            crate::cmd::rank::newest_of(
                candidates.iter().map(|c| {
                    let e = &index.entries[c.idx as usize];
                    (e.last_run_at, e.db_id)
                }),
                CMD_RECENCY_RANK_CAP,
            )
        };
        let served = newest(&loaded, &loaded_ctx);
        assert_eq!(served.len(), CMD_RECENCY_RANK_CAP);
        assert_eq!(newest(&replayer.index, &replay_ctx), served);
        assert_eq!(
            crate::cmd::train::newest_matching(&replayer, query),
            served,
            "query {query:?}"
        );

        // Every feature the model reads, per command, is the same too.
        let features = |index: &CommandIndex, ctx: &RankContext| -> Vec<(i64, Vec<f64>)> {
            let query_lower = query.to_lowercase();
            let newest = newest(index, ctx);
            let candidates = filter_candidates(index, ctx, query, Scope::All, None);
            let mut rows: Vec<(i64, Vec<f64>)> = candidates
                .iter()
                .map(|c| {
                    let entry = &index.entries[c.idx as usize];
                    let inputs = CommandFeatureInputs {
                        query,
                        query_lower: &query_lower,
                        idx: c.idx,
                        entry,
                        fzf_score: c.fzf,
                        recency_rank: crate::cmd::rank::recency_rank(
                            &newest,
                            (entry.last_run_at, entry.db_id),
                        ),
                        index,
                        ctx,
                    };
                    let mut row = vec![0.0; COMMAND_FEATURE_REGISTRY.len()];
                    compute_all(&inputs, &mut row);
                    (entry.db_id, row)
                })
                .collect();
            rows.sort_by_key(|(id, _)| *id);
            rows
        };
        assert_eq!(
            features(&replayer.index, &replay_ctx),
            features(&loaded, &loaded_ctx),
            "features for query {query:?}"
        );
    }
}

// --- Picks around the cut --------------------------------------------------------

fn event(id: i64, timestamp: i64, is_pick: bool, command_id: i64) -> EventRecord {
    EventRecord {
        id,
        timestamp,
        is_pick,
        command_id,
        query: "l".into(),
        rank: Some(1),
        session_id: "picker-1".into(),
        subsession_id: 1,
        episode_queries: vec!["l".into()],
        cwd: Some("/a".into()),
        repo_root: None,
        shell_session: Some("s".into()),
    }
}

#[test]
fn a_pick_after_the_cut_is_applied_and_its_execution_is_not_a_lookup() {
    // An impression before the cut, its pick after it, then the run that
    // executes the pick (not a lookup) and a later run (a lookup).
    let input = ReplayInput {
        commands: vec![command(1, "ls"), command(2, "pwd")],
        runs: vec![
            run_record(1, 1, 100, Some("/a"), "s"),
            run_record(2, 2, 200, Some("/a"), "s"),
            run_record(3, 1, 1020, Some("/a"), "s"),
            run_record(4, 1, 5000, Some("/a"), "s"),
        ],
        events: vec![event(1, 900, false, 1), event(2, 1010, true, 1)],
    };
    let replayer = replay(&input, Some(1000));
    let sampled: FxHashSet<i64> = [3, 4].into_iter().collect();
    let ranks = replay_window(&input, replayer, &[], 1000, None, &sampled);
    assert_eq!(ranks.after_pick, 1, "run 3 executes the pick");
    let lengths: usize = ranks.lookups.iter().map(Vec::len).sum();
    assert_eq!(lengths, 3, "run 4 at lengths 0, 1, 2");
    assert_eq!(ranks.lookups[0][0].recency, 1, "ls ran last, at 1020");
}

// --- IT-N-04: two-stage check ------------------------------------------------------

#[test]
fn it_n_04_twostage_agrees_fully_when_k_covers_every_command() {
    let dir = TempDir::new("bench-it-n-04");
    generate(&dir.0);
    super::super::super::train::retrain(&dir.0, 42, true).unwrap();
    let out = run_bench(&dir.0, BenchCommand::Twostage { queries: 40 });
    assert!(out.contains("mean top-10 overlap: 1.0000"), "{out}");
    assert!(out.contains("top-1 agreement: 1.0000"), "{out}");
    assert!(out.contains("PASS"), "{out}");
}
