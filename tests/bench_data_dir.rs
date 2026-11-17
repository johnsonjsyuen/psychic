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
        ["data", "home", "work"].map(|name| scratch.path(name)),
        "nothing was written beside the data dir"
    );
}
