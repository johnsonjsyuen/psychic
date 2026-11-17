//! IT-N-05 (`docs/ctrl-r/04-bench.md` section 11.2): the history benchmark
//! writes only into the `--data-dir` it is given, never into the data
//! directory of `HOME`.
//!
//! The built binary runs with a scratch `HOME` and a cleared environment, so
//! the "real" data directory it would resolve is `<scratch>/home/.local/
//! share/psychic`, and the test does not depend on (or touch) the data
//! directory of the machine running it.

mod support;

use std::path::{Path, PathBuf};
use support::Scratch;

/// Every file and directory under `dir`, recursively, sorted.
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Run the built binary with the scratch `HOME`, a cleared environment and
/// exactly `args` (no `--data-dir` unless `args` has one). Returns whether it
/// succeeded and its stderr.
fn run_bare(scratch: &Scratch, args: &[&str]) -> (bool, String) {
    let output = std::process::Command::new(support::BINARY)
        .env_clear()
        .env("HOME", scratch.path("home"))
        .current_dir(scratch.path("work"))
        .args(args)
        .output()
        .expect("psychic runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// TC-N-09 end to end: a refused benchmark run creates nothing, not even the
/// data directory or the app.log the logger would open there.
#[test]
fn it_n_05_a_refused_benchmark_creates_nothing_under_home() {
    let scratch = Scratch::empty("bench", "refused");
    let default_dir = scratch.display("home/.local/share/psychic");
    for args in [
        vec!["internal", "bench-history", "stats"],
        vec![
            "--data-dir",
            &default_dir,
            "internal",
            "bench-history",
            "stats",
        ],
    ] {
        let (succeeded, stderr) = run_bare(&scratch, &args);
        assert!(!succeeded, "{args:?} is refused");
        assert!(
            stderr.contains("bench-history needs an explicit --data-dir"),
            "{args:?} names the guard"
        );
        assert_eq!(
            tree(&scratch.path("home")),
            Vec::<PathBuf>::new(),
            "{args:?} wrote nothing under HOME"
        );
    }
}

#[test]
fn it_n_05_the_benchmark_writes_only_into_its_data_dir() {
    let scratch = Scratch::empty("bench", "data-dir");
    let bench = |args: &[&str]| {
        let mut full = vec!["internal", "bench-history"];
        full.extend_from_slice(args);
        scratch.psychic(&full)
    };
    let json = scratch.display("data/out.json");

    bench(&[
        "generate",
        "--commands",
        "100",
        "--runs",
        "2000",
        "--seed",
        "3",
    ]);
    let report = bench(&[
        "relevance",
        "--seeds",
        "1",
        "--max-test-runs",
        "100",
        "--json",
        &json,
    ]);
    bench(&["stats"]);

    assert!(
        String::from_utf8_lossy(&report).contains("## Verdicts"),
        "the benchmark ran"
    );
    assert!(Path::new(&json).exists());
    assert_eq!(
        tree(&scratch.path("home")),
        Vec::<PathBuf>::new(),
        "nothing was written under HOME, so not into its data directory"
    );
    let mut top: Vec<PathBuf> = std::fs::read_dir(&scratch.root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    top.sort();
    assert_eq!(
        top,
        ["bin", "data", "home", "work"].map(|name| scratch.path(name)),
        "nothing was written beside the data dir"
    );
}
