//! Tests of the replay, the groups, the fit and the retrain
//! (`docs/ctrl-r/03-learn.md` section 10).

use super::test_support::{TempDir, generate_fixture, trained_fixture};
use super::*;
use crate::cmd::events::{CmdEvents, PickPlace, Shown};
use crate::cmd::index::LoadParams;
use crate::cmd::model::CmdModel;
use crate::cmd::store::NewRun;
use std::os::unix::fs::PermissionsExt;

const T0: i64 = 1_000_000;

// --- Record builders -------------------------------------------------------------

fn command(id: i64, text: &str) -> CommandRecord {
    CommandRecord {
        id,
        text: text.to_string(),
        template: crate::cmd::template::templatize(text),
    }
}

fn run(id: i64, command_id: i64, started_at: i64) -> RunRecord {
    RunRecord {
        id,
        command_id,
        started_at,
        cwd: Some("/w".to_string()),
        repo_root: None,
        exit_code: Some(0),
        shell_session: "s".to_string(),
        prev_command_id: None,
        source: Source::Hook,
    }
}

fn in_session(mut run: RunRecord, session: &str) -> RunRecord {
    run.shell_session = session.to_string();
    run
}

fn imported(mut run: RunRecord) -> RunRecord {
    run.source = Source::Import;
    run
}

fn impression(id: i64, at: i64, command_id: i64, query: &str, picker: &str) -> EventRecord {
    EventRecord {
        id,
        timestamp: at,
        is_pick: false,
        command_id,
        query: query.to_string(),
        rank: Some(1),
        session_id: picker.to_string(),
        subsession_id: 1,
        episode_queries: Vec::new(),
        cwd: None,
        repo_root: None,
        shell_session: None,
    }
}

fn pick(id: i64, at: i64, command_id: i64, query: &str, picker: &str, shell: &str) -> EventRecord {
    EventRecord {
        is_pick: true,
        episode_queries: vec![query.to_string()],
        cwd: Some("/w".to_string()),
        shell_session: Some(shell.to_string()),
        ..impression(id, at, command_id, query, picker)
    }
}

fn input(
    commands: Vec<CommandRecord>,
    runs: Vec<RunRecord>,
    events: Vec<EventRecord>,
) -> ReplayInput {
    ReplayInput {
        commands,
        runs,
        events,
    }
}

fn options(max_run_groups: usize) -> BuildOptions {
    BuildOptions {
        seed: 42,
        max_run_groups,
        until: None,
        shuffle_labels: false,
    }
}

/// Commands whose texts share their first three characters, so a synthetic
/// query of any length matches all of them.
fn git_commands(n: i64) -> Vec<CommandRecord> {
    (1..=n)
        .map(|id| command(id, &format!("git c{id}")))
        .collect()
}

fn feature(name: &str) -> usize {
    command_feature_names()
        .iter()
        .position(|n| *n == name)
        .unwrap()
}

// --- Unit tests ------------------------------------------------------------------

#[test]
fn tc_l_07_splitmix64_matches_the_published_vectors() {
    assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
    assert_eq!(splitmix64(1), 0x910A_2DEC_8902_5CC1);
}

#[test]
fn tc_l_08_query_lengths_follow_the_mix_and_the_seed() {
    let lengths = |seed: u64| -> Vec<usize> {
        (1..=10_000)
            .map(|run_id| query_length(seed, run_id))
            .collect()
    };
    let at_42 = lengths(42);
    for (length, &share) in CMD_TRAIN_QLEN_MIX.iter().enumerate() {
        let count = at_42.iter().filter(|&&l| l == length).count() as f64;
        let percent = count / 100.0;
        assert!(
            (percent - f64::from(share)).abs() <= 2.0,
            "length {length}: {percent}% against {share}%"
        );
    }
    assert_eq!(
        at_42,
        lengths(42),
        "the same ids and seed, the same lengths"
    );
    assert_ne!(at_42, lengths(43), "another seed, other lengths");
    assert_eq!(
        synthetic_query("ls", 3),
        "ls",
        "shorter text: the whole text"
    );
    assert_eq!(synthetic_query("äbcd", 2), "äb", "characters, not bytes");
}

#[test]
fn tc_l_09_a_run_does_not_see_itself() {
    let commands = vec![command(1, "make a"), command(2, "make b")];
    let mut replayer = Replayer::new(&commands);
    replayer.apply_run(&run(1, 1, T0));
    replayer.apply_run(&run(2, 2, T0 + 100));
    let third = run(3, 1, T0 + 200);
    let group = build_run_group(&replayer, &third, "", 42).unwrap();
    let positive = group.rows.iter().find(|row| row.label == 1.0).unwrap();
    assert_eq!(positive.features[feature("runs_last_hour")], 1.0);
    assert_eq!(
        positive.features[feature("log_seconds_since_last_run")],
        201f64.ln()
    );
}

#[test]
fn tc_l_10_a_first_ever_run_makes_no_group() {
    let built = build_training_set(
        &input(
            git_commands(2),
            vec![run(1, 1, T0), run(2, 2, T0 + 10)],
            vec![],
        ),
        &options(8000),
    );
    assert_eq!(built.counts, GroupCounts::default());
    assert_eq!(built.set.len(), 0);
}

#[test]
fn tc_l_11_negatives_are_random_and_rows_are_in_idx_order() {
    let commands = git_commands(40);
    let mut replayer = Replayer::new(&commands);
    for id in 1..=40 {
        replayer.apply_run(&run(id, id, T0 + id));
    }
    let again = run(41, 1, T0 + 100);
    let group = build_run_group(&replayer, &again, "", 42).unwrap();

    let idxs: Vec<u32> = group.rows.iter().map(|row| row.idx).collect();
    assert_eq!(group.rows.len(), 31);
    assert!(
        idxs.windows(2).all(|w| w[0] < w[1]),
        "ascending idx: {idxs:?}"
    );
    let labels: f32 = group.rows.iter().map(|row| row.label).sum();
    assert_eq!(labels, 1.0);
    assert_eq!(group.rows[0].label, 1.0, "command 1 is idx 0");
    // Since 2026-11-17 no fixed recent negatives (CMD_TRAIN_RECENT_NEGATIVES
    // is 0): 30 distinct random others, not only the newest.
    assert_eq!(CMD_TRAIN_RECENT_NEGATIVES, 0);
    assert_eq!(group.rows.len(), 1 + CMD_TRAIN_RANDOM_NEGATIVES);
    assert!(
        idxs.iter().filter(|&&idx| (1..25).contains(&idx)).count() > 5,
        "older commands are drawn too: {idxs:?}"
    );
    assert_eq!(group.weight, CMD_HOOK_WEIGHT);
    assert_eq!(group.timestamp, T0 + 100);

    // A query only the positive matches: no negative, no group.
    let mut commands = git_commands(3);
    commands[0].text = "zzz".to_string();
    let mut replayer = Replayer::new(&commands);
    for id in 1..=3 {
        replayer.apply_run(&run(id, id, T0 + id));
    }
    assert!(build_run_group(&replayer, &run(4, 1, T0 + 10), "zz", 42).is_none());
}

#[test]
fn a_command_that_does_not_match_its_own_query_makes_no_group() {
    let commands = git_commands(2);
    let mut replayer = Replayer::new(&commands);
    replayer.apply_run(&run(1, 1, T0));
    replayer.apply_run(&run(2, 2, T0 + 1));
    assert!(build_run_group(&replayer, &run(3, 1, T0 + 2), "xq", 42).is_none());
}

#[test]
fn tc_l_12_weights_and_the_cap() {
    let mut runs = vec![run(1, 1, T0), run(2, 2, T0 + 1)];
    for i in 0..10 {
        let id = 3 + i;
        let r = run(id, 1 + i % 2, T0 + 10 + i);
        runs.push(if i % 2 == 0 { r } else { imported(r) });
    }
    let input = input(git_commands(2), runs, vec![]);
    assert_eq!(eligible_runs(&input, None), 10);

    let built = build_groups(&input, &options(4));
    let stamps: Vec<i64> = built.groups.iter().map(|g| g.timestamp).collect();
    assert_eq!(stamps, [T0 + 16, T0 + 17, T0 + 18, T0 + 19], "the 4 newest");
    let weights: Vec<f64> = built.groups.iter().map(|g| g.weight).collect();
    assert_eq!(weights, [1.0, 0.3, 1.0, 0.3]);
    assert_eq!(
        (built.counts.hook, built.counts.import, built.counts.pick),
        (2, 2, 0)
    );

    let mut builder = SetBuilder::default();
    for group in built.groups {
        builder.push(group);
    }
    assert_eq!(
        builder.weights,
        [1.0, 1.0, 0.3, 0.3, 1.0, 1.0, 0.3, 0.3],
        "row_weights by source, every row of a group"
    );
    assert_eq!(builder.episodes, [1, 1, 2, 2, 3, 3, 4, 4]);
    let set = build_training_set(&input, &options(4));
    assert_eq!(set.set.len(), set.counts.rows);
    assert_eq!(set.set.len(), 8, "a positive and one negative per group");
}

#[test]
fn tc_l_13_a_pick_group_holds_the_impressions_of_its_episode() {
    let (a, b, c) = (1, 2, 3);
    let runs = vec![run(1, a, T0), run(2, b, T0 + 1), run(3, c, T0 + 2)];
    let t = T0 + 100;
    let events = vec![
        impression(1, t, a, "g", "tui"),
        impression(2, t, b, "g", "tui"),
        impression(3, t, c, "g", "tui"),
        impression(4, t + 1, a, "gi", "tui"),
        impression(5, t + 1, b, "gi", "tui"),
        pick(6, t + 2, a, "gi", "tui", "s"),
    ];
    let built = build_groups(&input(git_commands(3), runs, events), &options(8000));
    assert_eq!(built.counts.pick, 1);
    assert_eq!(built.groups.len(), 1);
    let group = &built.groups[0];
    let labels: Vec<f32> = group.rows.iter().map(|row| row.label).collect();
    assert_eq!(labels, [1.0, 0.0, 0.0, 1.0, 0.0]);
    assert_eq!(group.weight, CMD_PICK_WEIGHT);
    assert_eq!(group.timestamp, t + 2);
    let a_idx = built.replayer.knows(a).unwrap();
    assert_eq!(
        built
            .replayer
            .index
            .picks_in_window(a_idx, t + 10, CMD_EVENT_WINDOW),
        1
    );
    // Each row has its own query: "g" is a prefix of "git c1", "gi" too.
    let starts = feature("command_starts_with_query");
    assert!(group.rows.iter().all(|row| row.features[starts] == 1.0));
}

const CMD_EVENT_WINDOW: i64 = crate::cmd::tuning::CMD_EVENT_WINDOW_SECS;

#[test]
fn tc_l_13_episodes_without_both_labels_make_no_group() {
    let runs = vec![run(1, 1, T0), run(2, 2, T0 + 1), run(3, 3, T0 + 2)];
    let t = T0 + 100;
    let events = vec![
        // The picked command was never shown.
        impression(1, t, 2, "g", "tui1"),
        pick(2, t + 1, 1, "g", "tui1", "s"),
        // Shown, never picked.
        impression(3, t + 2, 1, "g", "tui2"),
        impression(4, t + 2, 2, "g", "tui2"),
    ];
    let built = build_groups(&input(git_commands(3), runs, events), &options(8000));
    assert_eq!(built.counts.pick, 0);
    let idx = built.replayer.knows(1).unwrap();
    assert_eq!(
        built
            .replayer
            .index
            .picks_in_window(idx, t + 10, CMD_EVENT_WINDOW),
        1,
        "the pick is committed even without a group"
    );
}

#[test]
#[should_panic(expected = "rows are sorted by episode")]
fn tc_l_14_from_parts_wants_sorted_episodes() {
    crate::training::TrainingSet::from_parts(
        1,
        vec![0.0, 1.0],
        vec![0.0, 1.0],
        vec![2, 1],
        vec![0.0, 0.0],
        vec![1.0, 1.0],
    );
}

#[test]
#[should_panic(expected = "every row weight is finite and positive")]
fn tc_l_14_from_parts_wants_positive_weights() {
    crate::training::TrainingSet::from_parts(
        1,
        vec![0.0, 1.0],
        vec![0.0, 1.0],
        vec![1, 1],
        vec![0.0, 0.0],
        vec![1.0, 0.0],
    );
}

#[test]
#[should_panic(expected = "features hold num_features values per row")]
fn tc_l_14_from_parts_wants_whole_rows() {
    crate::training::TrainingSet::from_parts(
        2,
        vec![0.0, 1.0, 2.0],
        vec![0.0, 1.0],
        vec![1, 1],
        vec![0.0, 0.0],
        vec![1.0, 1.0],
    );
}

fn sample_stats() -> CmdModelStats {
    let counts = GroupCounts {
        pick: 3,
        hook: 100,
        import: 10,
        rows: 999,
    };
    let fitted = crate::training::Fitted {
        model: String::new(),
        best_iteration: 17,
        validation_ndcg: vec![0.5, 0.75],
        gain_importance: (0..COMMAND_FEATURE_REGISTRY.len())
            .map(|i| i as f64)
            .collect(),
        train_rows: 1,
        validation_rows: 1,
        test_rows: 1,
    };
    CmdModelStats::from_fit(
        &fitted,
        &counts,
        42,
        1.234,
        jiff::Timestamp::from_second(T0).unwrap(),
    )
}

#[test]
fn tc_l_16_the_stats_file_round_trips() {
    let stats = sample_stats();
    assert_eq!(stats.blend_examples, 3.0 * 4.0 + 100.0 + 10.0 * 0.3);
    assert_eq!(stats.training_duration_seconds, 1.23);
    assert_eq!(stats.trained_at_unix, T0);
    assert_eq!(stats.feature_names, command_feature_names());
    assert_eq!(
        stats.feature_gain[0].feature,
        *command_feature_names().last().unwrap(),
        "gain descending"
    );
    assert_eq!(stats.feature_gain.len(), COMMAND_FEATURE_REGISTRY.len());
    assert_eq!(stats.groups(), 113);
    let json = serde_json::to_string_pretty(&stats).unwrap();
    let back: CmdModelStats = serde_json::from_str(&json).unwrap();
    assert_eq!(back, stats);
}

#[test]
fn tc_l_18_a_run_right_after_its_pick_in_the_same_shell_is_not_a_group() {
    let (a, b) = (1, 2);
    let t = T0 + 100;
    let runs = vec![
        run(1, a, T0),
        run(2, b, T0 + 1),
        run(3, a, t + 5),
        in_session(run(4, a, t + 6), "u"),
        run(5, a, t + 400),
    ];
    let events = vec![pick(1, t, a, "g", "tui", "s")];
    // The same two words in both texts, so any prefix or fragment query
    // of one matches the other: every eligible run has a negative.
    let commands = vec![command(a, "git log"), command(b, "log git")];
    let input = input(commands, runs, events);
    assert_eq!(eligible_runs(&input, None), 2);
    let built = build_groups(&input, &options(8000));
    let stamps: Vec<i64> = built.groups.iter().map(|g| g.timestamp).collect();
    assert_eq!(stamps, [t + 6, t + 400]);
}

#[test]
fn the_timeline_merges_by_time_with_events_first_and_stops_at_until() {
    let runs = vec![run(1, 1, 10), run(2, 1, 20), run(3, 1, 30)];
    let events = vec![impression(1, 20, 1, "", "p"), impression(2, 25, 1, "", "p")];
    let input = input(git_commands(1), runs, events);
    let order = |until| -> Vec<String> {
        Timeline::new(&input, until)
            .map(|step| match step {
                Step::Run(run) => format!("r{}", run.id),
                Step::Event(event) => format!("e{}", event.id),
            })
            .collect()
    };
    assert_eq!(order(None), ["r1", "e1", "r2", "e2", "r3"]);
    assert_eq!(order(Some(25)), ["r1", "e1", "r2"]);
}

#[test]
fn recent_distinct_yields_each_command_once_newest_first_across_compaction() {
    let commands = git_commands(3);
    let mut replayer = Replayer::new(&commands);
    let mut id = 0;
    for _ in 0..1000 {
        for command_id in [1, 2, 3, 2] {
            id += 1;
            replayer.apply_run(&run(id, command_id, T0 + id));
        }
    }
    let order: Vec<u32> = replayer.recent_distinct().collect();
    assert_eq!(order, [1, 2, 0]);
    assert!(replayer.recency.len() <= 2 * 3 + 1024 + 1, "compacted");
    assert_eq!(replayer.last_cwd(0), replayer.index.dirs.get("/w"));
}

#[test]
fn the_session_tail_and_the_follower_template_come_from_the_command_records() {
    let commands = vec![command(1, "cd /x"), command(2, "make")];
    let mut replayer = Replayer::new(&commands);
    replayer.apply_run(&run(1, 1, T0));
    let mut second = run(2, 2, T0 + 5);
    second.prev_command_id = Some(1);
    replayer.apply_run(&second);
    let ctx = replayer.context(T0 + 10, Some("/w"), None, Some("s"));
    let templates = &replayer.index.templates;
    assert_eq!(
        ctx.prev_templates,
        [templates.get("make"), templates.get("cd PATH"), None]
    );
    assert_eq!(replayer.index.follower_runs(1, templates.get("cd PATH")), 1);
    let unknown = replayer.context(T0, Some("/never"), Some("/r"), Some("t"));
    assert_eq!(
        (unknown.dir, unknown.repo, unknown.prev_templates),
        (None, None, [None; 3])
    );
}

#[test]
fn e_l_11_events_of_a_command_that_never_ran_are_dropped() {
    let mut replayer = Replayer::new(&git_commands(2));
    replayer.apply_run(&run(1, 1, T0));
    assert!(
        replayer
            .apply_event(&impression(1, T0, 2, "g", "p"))
            .is_none()
    );
    assert!(
        replayer
            .apply_event(&impression(2, T0, 1, "g", "p"))
            .is_none()
    );
    assert!(
        replayer
            .apply_event(&pick(3, T0, 2, "g", "p", "s"))
            .is_none(),
        "a pick of a command that never ran"
    );
    assert!(replayer.pending.is_empty(), "its episode ended with it");
    // A run of a command missing from the command list is skipped too.
    replayer.apply_run(&run(2, 99, T0 + 1));
    assert_eq!(replayer.knows(99), None);
}

#[test]
fn e_l_12_runs_are_read_in_time_order_whatever_their_ids() {
    let store = CmdStore::in_memory();
    for (at, text) in [(300, "c"), (100, "a"), (200, "b")] {
        store
            .record_run(&crate::cmd::store::test_support::hook_run(
                text,
                "s",
                at,
                Some(0),
            ))
            .unwrap();
    }
    let input = ReplayInput::read(&store).unwrap();
    let times: Vec<i64> = input.runs.iter().map(|r| r.started_at).collect();
    assert_eq!(times, [100, 200, 300]);
}

#[test]
fn e_l_02_a_split_with_an_empty_part_is_not_enough_data() {
    // Two groups: the time split cannot give validation and test rows.
    let set = crate::training::TrainingSet::from_parts(
        COMMAND_FEATURE_REGISTRY.len(),
        vec![0.5; 4 * COMMAND_FEATURE_REGISTRY.len()],
        vec![1.0, 0.0, 1.0, 0.0],
        vec![1, 1, 2, 2],
        vec![0.0; 4],
        vec![1.0; 4],
    );
    let error = crate::training::fit_seeded(&set, &command_feature_specs(), 42).unwrap_err();
    assert!(is_not_enough_for_a_split(&error), "{error}");
    assert!(!is_not_enough_for_a_split(&anyhow::anyhow!(
        "LightGBM error"
    )));
}

#[test]
fn it_l_06_fit_is_fit_seeded_with_42() {
    // The CSV of `test_from_csv_drops_unclicked_episodes_and_sorts_stably`,
    // grown to 40 episodes.
    let mut csv = String::from("label,episode_id,timestamp,query,b,a\n");
    for episode in 0..40 {
        for row in 0..3 {
            let label = u8::from(row == episode % 3);
            let b = (episode * 7 + row * 3) % 11;
            let a = if (episode + row) % 5 == 0 {
                String::new()
            } else {
                ((episode * 13 + row) % 17).to_string()
            };
            csv.push_str(&format!("{label},{episode},{},x,{b},{a}\n", 100 + episode));
        }
    }
    let specs = [
        crate::training::FeatureSpec {
            name: "a",
            monotone: 0,
        },
        crate::training::FeatureSpec {
            name: "b",
            monotone: 1,
        },
    ];
    let set = crate::training::TrainingSet::from_csv(csv.as_bytes(), &specs).unwrap();
    let plain = crate::training::fit(&set, &specs).unwrap();
    let seeded = crate::training::fit_seeded(&set, &specs, 42).unwrap();
    assert_eq!(plain.model, seeded.model);
    let other = crate::training::fit_seeded(&set, &specs, 7).unwrap();
    assert!(other.model.contains("seed: 7]"), "the seed is a parameter");
}

// --- Integration tests -----------------------------------------------------------

fn record(
    store: &CmdStore,
    text: &str,
    at: i64,
    place: (&str, Option<&str>, &str),
    exit: Option<i32>,
) {
    let (cwd, repo_root, session) = place;
    store
        .record_run(&NewRun {
            text,
            template: &crate::cmd::template::templatize(text),
            started_at: at,
            cwd: Some(cwd),
            repo_root,
            exit_code: exit,
            duration_ms: None,
            shell_session: session,
            source: Source::Hook,
        })
        .unwrap();
}

/// Feature vectors equal as bits: NaN equals NaN.
fn same_bits(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

#[test]
fn it_l_01_serving_and_replay_compute_the_same_features() {
    let dir = TempDir::new("parity");
    let store = CmdStore::open(&dir.0).unwrap();
    let base = jiff::Timestamp::now().as_second();
    let texts: Vec<String> = (0..12)
        .map(|i| match i % 3 {
            0 => format!("git commit -m {i}"),
            1 => format!("cd /src/p{i}"),
            _ => format!("make t{i}"),
        })
        .collect();
    let dirs = ["/w/a", "/w/b", "/home"];
    let repos = [Some("/w"), Some("/r"), None];
    let sessions = ["s1", "s2", "s3"];
    for i in 0..60i64 {
        let n = i as usize;
        // Pairs 600 s apart in one session: the second has a previous command.
        let at = base - 20 * 86_400 + (i / 2) * 54_000 + (i % 2) * 600;
        let exit = match i % 5 {
            0 => Some(1),
            1 => None,
            _ => Some(0),
        };
        record(
            &store,
            &texts[(n * 7) % 12],
            at,
            (dirs[n % 3], repos[(n / 3) % 3], sessions[(n / 2) % 3]),
            exit,
        );
    }
    let id = |text: &str| crate::cmd::store::test_support::command_id(&store, text).unwrap();
    for (episode, picked) in [0usize, 3, 4, 7].into_iter().enumerate() {
        let place = PickPlace {
            cwd: dirs[episode % 3].to_string(),
            repo_root: repos[episode % 3].map(str::to_string),
            shell_session: Some(sessions[episode % 3].to_string()),
        };
        let mut events = CmdEvents::new(
            format!("tui{episode}"),
            place,
            Some(CmdStore::open(&dir.0).unwrap()),
        );
        for (query_id, query) in [(2, "g"), (3, "gi")] {
            events.new_subsession(query_id, query.to_string());
            assert!(events.wants_impressions(true));
            let shown: Vec<Shown> = [picked, (picked + 1) % 12]
                .iter()
                .enumerate()
                .map(|(rank, &n)| Shown {
                    command_id: id(&texts[n]),
                    rank: rank + 1,
                })
                .collect();
            events.log_impressions(&shown).unwrap();
        }
        events
            .log_pick(Shown {
                command_id: id(&texts[picked]),
                rank: 1,
            })
            .unwrap();
    }

    let now = base + 100;
    let (cwd, repo, session) = ("/w/a", Some("/w"), "s1");
    let (served, served_ctx) = CommandIndex::load(
        &store,
        &LoadParams {
            now,
            cwd: Some(cwd.to_string()),
            repo_root: repo.map(str::to_string),
            shell_session: Some(session.to_string()),
        },
    )
    .unwrap();
    let input = ReplayInput::read(&store).unwrap();
    let mut replayer = Replayer::new(&input.commands);
    for step in Timeline::new(&input, None) {
        match step {
            Step::Run(run) => replayer.apply_run(run),
            Step::Event(event) => {
                if let Some(episode) = replayer.apply_event(event) {
                    replayer.commit_pick(&episode);
                }
            }
        }
    }
    let replay_ctx = replayer.context(now, Some(cwd), repo, Some(session));
    assert!(
        replay_ctx.prev_templates[0].is_some(),
        "the session has runs"
    );

    let mut compared = 0;
    for entry in &served.entries {
        let replay_idx = replayer.knows(entry.db_id).unwrap();
        let served_idx = served.idx_of(entry.db_id).unwrap();
        for query in ["", "g"] {
            let score =
                crate::cmd::rank::fzf_score(&crate::cmd::fzf::FzfQuery::new(query), &entry.text);
            let newest = newest_matching(&replayer, query);
            let a = feature_row(&served, &served_ctx, query, &newest, served_idx, score);
            let b = feature_row(
                &replayer.index,
                &replay_ctx,
                query,
                &newest,
                replay_idx,
                score,
            );
            assert!(same_bits(&a, &b), "{query:?}: {a:?} != {b:?}");
            compared += 1;
        }
    }
    assert_eq!(compared, 24);
    let picked = served.idx_of(id(&texts[0])).unwrap();
    assert!(
        served.picks_in_window(picked, now, CMD_EVENT_WINDOW) > 0,
        "picks are compared too"
    );
}

#[test]
fn it_l_02_retrain_end_to_end() {
    let dir = TempDir::new("retrain");
    let stats = trained_fixture(&dir.0);
    let model = dir.0.join(MODEL_FILE);
    let stats_file = dir.0.join(STATS_FILE);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&model), mode(&stats_file)), (0o600, 0o600));
    assert_eq!(stats.feature_names, command_feature_names());
    assert!(stats.hook_groups > CMD_TRAIN_MIN_GROUPS);
    assert!(CmdModel::load(&dir.0).is_some());
    assert!(!dir.0.join(LOCK_FILE).exists());
    assert!(dir.0.join(ATTEMPT_FILE).exists());
    let log = std::fs::read_to_string(dir.0.join(TRAINING_LOG)).unwrap();
    assert!(log.contains("=== Command model training run at "), "{log}");
    assert!(log.contains(" groups (0 picks, "), "{log}");

    let first = std::fs::read(&model).unwrap();
    assert!(matches!(
        retrain(&dir.0, 42, true).unwrap(),
        RetrainOutcome::Trained(_)
    ));
    assert_eq!(std::fs::read(&model).unwrap(), first, "byte for byte");
    assert!(!dir.0.join(LOCK_FILE).exists());
}

/// 10 runs of 3 commands.
fn ten_runs(data_dir: &Path) {
    let store = CmdStore::open(data_dir).unwrap();
    for i in 0..10 {
        record(
            &store,
            ["ls", "make", "git status"][i % 3],
            T0 + i as i64,
            ("/w", None, "s"),
            Some(0),
        );
    }
}

#[test]
fn it_l_03_too_few_groups_train_nothing() {
    let dir = TempDir::new("few");
    ten_runs(&dir.0);
    match retrain(&dir.0, 42, true).unwrap() {
        RetrainOutcome::NotEnoughData { groups } => assert!(groups < CMD_TRAIN_MIN_GROUPS),
        _ => panic!("not enough data"),
    }
    assert!(!dir.0.join(MODEL_FILE).exists());
    assert!(!dir.0.join(LOCK_FILE).exists());
    let log = std::fs::read_to_string(dir.0.join(TRAINING_LOG)).unwrap();
    assert!(log.contains("Nothing to train on yet: "), "{log}");
    assert!(
        log.contains(" groups (need 50). Ranking stays on the heuristic."),
        "{log}"
    );
}

#[test]
fn it_l_04_a_stale_or_broken_model_is_refused() {
    let dir = TempDir::new("refuse");
    trained_fixture(&dir.0);
    let model = std::fs::read(dir.0.join(MODEL_FILE)).unwrap();
    let stats = std::fs::read_to_string(dir.0.join(STATS_FILE)).unwrap();
    let restore = || {
        std::fs::write(dir.0.join(MODEL_FILE), &model).unwrap();
        std::fs::write(dir.0.join(STATS_FILE), &stats).unwrap();
    };
    assert!(CmdModel::load(&dir.0).is_some());

    let renamed = stats.replacen("\"fuzzy_score\"", "\"fuzzy_score_v0\"", 1);
    assert_ne!(renamed, stats);
    std::fs::write(dir.0.join(STATS_FILE), renamed).unwrap();
    assert!(CmdModel::load(&dir.0).is_none(), "a feature was renamed");

    restore();
    std::fs::remove_file(dir.0.join(STATS_FILE)).unwrap();
    assert!(CmdModel::load(&dir.0).is_none(), "no stats");

    restore();
    std::fs::write(dir.0.join(MODEL_FILE), &model[..model.len() / 3]).unwrap();
    assert!(CmdModel::load(&dir.0).is_none(), "a truncated model");

    restore();
    std::fs::write(dir.0.join(STATS_FILE), "{").unwrap();
    assert!(CmdModel::load(&dir.0).is_none(), "unreadable stats");
}

#[test]
fn it_l_05_a_fresh_lock_is_respected_and_a_stale_one_taken_over() {
    let dir = TempDir::new("lock");
    ten_runs(&dir.0);
    let lock = dir.0.join(LOCK_FILE);
    std::fs::write(&lock, "").unwrap();
    assert!(matches!(
        retrain(&dir.0, 42, true).unwrap(),
        RetrainOutcome::AlreadyRunning
    ));
    assert!(lock.exists(), "another training's lock is left alone");

    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(1000))
        .unwrap();
    assert!(matches!(
        retrain(&dir.0, 42, true).unwrap(),
        RetrainOutcome::NotEnoughData { .. }
    ));
    assert!(!lock.exists());
}

#[test]
fn e_l_03_a_failure_under_quiet_is_returned_and_logged() {
    let dir = TempDir::new("broken");
    std::fs::write(
        dir.0.join("events.db"),
        "not a database, but long enough to be read as one",
    )
    .unwrap();
    assert!(retrain(&dir.0, 42, true).is_err());
    let log = std::fs::read_to_string(dir.0.join(TRAINING_LOG)).unwrap();
    assert!(log.contains("Training failed: "), "{log}");
    assert!(!dir.0.join(LOCK_FILE).exists());
}

#[test]
fn e_l_10_a_failed_stats_write_leaves_the_new_model_and_errs() {
    let dir = TempDir::new("stats-fail");
    generate_fixture(&dir.0);
    // A non-empty directory where the stats file goes: the rename fails.
    std::fs::create_dir_all(dir.0.join(STATS_FILE).join("x")).unwrap();
    assert!(retrain(&dir.0, 42, true).is_err());
    assert!(
        dir.0.join(MODEL_FILE).exists(),
        "the model was written first"
    );
    assert!(CmdModel::load(&dir.0).is_none());
}

#[test]
fn the_retrain_is_due_after_the_interval_and_without_a_fresh_lock() {
    let dir = TempDir::new("due");
    let now = SystemTime::now();
    assert!(retrain_due(&dir.0, now), "never attempted");
    let attempt = dir.0.join(ATTEMPT_FILE);
    std::fs::write(&attempt, "1\n").unwrap();
    assert!(!retrain_due(&dir.0, now), "attempted just now");
    let interval = Duration::from_secs(CMD_RETRAIN_MIN_INTERVAL_SECS);
    assert!(retrain_due(&dir.0, now + interval + Duration::from_secs(5)));

    std::fs::File::options()
        .write(true)
        .open(&attempt)
        .unwrap()
        .set_modified(now - interval - Duration::from_secs(5))
        .unwrap();
    assert!(retrain_due(&dir.0, now), "attempted over an interval ago");
    std::fs::write(dir.0.join(LOCK_FILE), "").unwrap();
    assert!(!retrain_due(&dir.0, now), "a training is running");
    let stale = Duration::from_secs(CMD_TRAIN_LOCK_STALE_SECS + 5);
    assert!(retrain_due(&dir.0, now + stale), "its lock is stale");
}

#[test]
fn a_model_with_an_older_feature_list_is_refused_and_retrained_at_once() {
    let dir = TempDir::new("old-features");
    let now = SystemTime::now();
    // A model of the release before `fzf_score` and `recency_rank`: the
    // registry's first 23 names.
    let mut stats = sample_stats();
    stats.feature_names.truncate(23);
    std::fs::write(
        dir.0.join(STATS_FILE),
        serde_json::to_string(&stats).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.0.join(MODEL_FILE), "tree\nversion=v4\n").unwrap();
    assert!(
        crate::cmd::model::CmdModel::load(&dir.0).is_none(),
        "refused: the heuristic ranks"
    );
    // An attempt an hour's interval would normally block, made before the
    // model was written: the retrain is due anyway.
    let attempt = dir.0.join(ATTEMPT_FILE);
    std::fs::write(&attempt, "1\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&attempt)
        .unwrap()
        .set_modified(now - Duration::from_secs(60))
        .unwrap();
    assert!(retrain_due(&dir.0, now));
    // An attempt after it (one that found too little to train) backs off.
    std::fs::File::options()
        .write(true)
        .open(&attempt)
        .unwrap()
        .set_modified(now + Duration::from_secs(1))
        .unwrap();
    assert!(!retrain_due(&dir.0, now + Duration::from_secs(2)));
    // A model with the current list waits for the interval as before.
    std::fs::write(
        dir.0.join(STATS_FILE),
        serde_json::to_string(&sample_stats()).unwrap(),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&attempt)
        .unwrap()
        .set_modified(now - Duration::from_secs(60))
        .unwrap();
    assert!(!retrain_due(&dir.0, now));
}

#[test]
fn e_l_09_a_failed_spawn_is_logged_and_ignored() {
    let dir = TempDir::new("spawn");
    let now = SystemTime::now();
    let missing = dir.0.join("no-such-binary");
    assert!(spawn_retrain(&missing, &dir.0).is_err());
    assert!(start_retrain_if_due(&dir.0, now, false, || Ok(missing.clone())).is_none());
    assert!(
        start_retrain_if_due(&dir.0, now, false, || Err(std::io::Error::other("no exe"))).is_none()
    );
}

#[test]
fn psychic_no_retrain_and_a_recent_attempt_start_nothing() {
    let dir = TempDir::new("no-retrain");
    let now = SystemTime::now();
    let never = || -> std::io::Result<PathBuf> { panic!("nothing is started") };
    assert!(start_retrain_if_due(&dir.0, now, true, never).is_none());
    std::fs::write(dir.0.join(ATTEMPT_FILE), "1\n").unwrap();
    assert!(start_retrain_if_due(&dir.0, now, false, never).is_none());
}

#[test]
fn a_held_lock_is_ours_alone_and_kept_fresh() {
    let dir = TempDir::new("lock-token");
    let path = dir.0.join(LOCK_FILE);
    let lock = TrainLock::take(&dir.0, SystemTime::now()).unwrap().unwrap();
    let token = std::fs::read_to_string(&path).unwrap();
    assert!(token.starts_with(&format!("{} ", std::process::id())));
    assert!(
        TrainLock::take(&dir.0, SystemTime::now())
            .unwrap()
            .is_none()
    );
    // Another training took the lock over (ours looked stale): dropping ours
    // leaves theirs.
    std::fs::write(&path, "someone else").unwrap();
    drop(lock);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "someone else");
    std::fs::remove_file(&path).unwrap();
    let lock = TrainLock::take(&dir.0, SystemTime::now()).unwrap().unwrap();
    drop(lock);
    assert!(!path.exists(), "our own lock goes with us");
}

#[test]
fn e_l_15_a_model_of_deleted_history_is_not_left_behind() {
    let dir = TempDir::new("shrunk");
    ten_runs(&dir.0);
    let store = CmdStore::open(&dir.0).unwrap();
    let before = store.counts().unwrap();
    let model = dir.0.join(MODEL_FILE);
    let new: &[(&str, &[u8])] = &[(MODEL_FILE, b"new model"), (STATS_FILE, b"new stats")];
    assert!(write_unless_shrunk(&dir.0, before, new).unwrap());
    assert_eq!(std::fs::read(&model).unwrap(), b"new model");

    // The history shrank (a delete, or a purge) while training: the new
    // files are dropped and the model in place stays.
    std::fs::write(&model, "previous model").unwrap();
    let id = crate::cmd::store::test_support::command_id(&store, "ls").unwrap();
    store.delete_command(id, "ls", T0 + 100).unwrap();
    assert!(!write_unless_shrunk(&dir.0, before, new).unwrap());
    assert_eq!(std::fs::read(&model).unwrap(), b"previous model");
    let leftovers: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".pending"))
        .collect();
    assert!(leftovers.is_empty(), "no pending file is left");
}

#[test]
fn a_quarter_of_non_empty_training_queries_are_fragments() {
    let text = "git commit --amend";
    let (mut non_empty, mut fragments) = (0, 0);
    for run_id in 0..4000 {
        let query = training_query(42, run_id, text);
        let length = query_length(42, run_id);
        assert_eq!(query.chars().count(), length);
        if length > 0 {
            non_empty += 1;
            if !text.starts_with(&query) {
                fragments += 1;
                assert!(
                    ["commit", "amend"].iter().any(|w| w.starts_with(&query)),
                    "a later word's start: {query}"
                );
            }
        }
    }
    let share = f64::from(fragments) / f64::from(non_empty);
    assert!(
        (share - f64::from(CMD_TRAIN_FRAGMENT_SHARE) / 100.0).abs() < 0.03,
        "{share}"
    );
    // A one-word text has no fragment: always the prefix.
    assert!((0..200).all(|id| "make".starts_with(&training_query(42, id, "make"))));
}

#[test]
fn empty_query_negatives_come_from_the_newest_that_serving_scores() {
    // 2,100 commands run once each, oldest first; then the oldest again.
    // For "" serving's model sees the CMD_STAGE2_K newest (stage 1 is
    // newest first), so every negative is one of them: idx >= 100.
    let total = CMD_STAGE2_K as i64 + 100;
    let commands = git_commands(total);
    let mut replayer = Replayer::new(&commands);
    for id in 1..=total {
        replayer.apply_run(&run(id, id, T0 + id));
    }
    let again = run(total + 1, 1, T0 + total + 10);
    let group = build_run_group(&replayer, &again, "", 42).unwrap();
    let negatives: Vec<u32> = group
        .rows
        .iter()
        .filter(|row| row.label == 0.0)
        .map(|row| row.idx)
        .collect();
    assert_eq!(negatives.len(), CMD_TRAIN_RANDOM_NEGATIVES);
    assert!(negatives.iter().all(|&idx| idx >= 100), "{negatives:?}");
    // A non-empty query draws from every matching command, old ones too.
    assert!((1..=10).any(|seed| {
        let group = build_run_group(&replayer, &again, "g", seed).unwrap();
        group
            .rows
            .iter()
            .any(|row| row.label == 0.0 && row.idx < 100)
    }));
}
