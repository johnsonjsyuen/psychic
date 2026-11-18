//! No command text outside `events.db` (`docs/ctrl-r/01-record.md` section
//! 10.6, IT-R-07), checked on the built binary: its real logger, its real
//! `app.log`, its real error messages.
//!
//! A scratch `HOME` holds shell history files with a sentinel in every
//! command, and `--data-dir` points into the scratch dir too, so neither the
//! real history nor the real data dir is read or written. The sentinel then
//! goes through every path that handles command text: the first `--enable`
//! (import), `track-command` (success and failure), `--import`, `--status`
//! and `--purge`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_psychic");
const SENTINEL: &str = "SENTINEL_c0ffee";

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A scratch root of its own per test: tests run side by side.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("psychic-leaks-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home/.local/share/atuin")).unwrap();
        Scratch { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    /// `psychic --data-dir <data> <args>` with only the scratch environment.
    fn psychic(&self, args: &[&str]) -> Output {
        self.psychic_in(&self.data(), args)
    }

    /// `psychic --data-dir <data_dir> <args>`, and its output holds no
    /// command text.
    fn psychic_in(&self, data_dir: &Path, args: &[&str]) -> Output {
        let output = Command::new(BINARY)
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .arg("--data-dir")
            .arg(data_dir)
            .args(args)
            .output()
            .expect("psychic runs");
        for stream in [&output.stdout, &output.stderr] {
            assert!(
                !contains(stream, SENTINEL),
                "psychic {args:?} printed command text: {}",
                String::from_utf8_lossy(stream)
            );
        }
        output
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn contains(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn is_database(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("events.db"))
}

#[test]
fn no_command_text_reaches_any_file_but_the_database() {
    let scratch = Scratch::new("record");
    let home = scratch.home();
    std::fs::write(
        home.join(".bash_history"),
        format!("#1700000000\necho {SENTINEL} from bash\n#1700000001\nls\n"),
    )
    .unwrap();
    std::fs::write(
        home.join(".zsh_history"),
        format!(": 1700000002:0;echo {SENTINEL} from zsh\n"),
    )
    .unwrap();
    // Not a database: the import logs that Atuin was skipped, and why.
    std::fs::write(
        home.join(".local/share/atuin/history.db"),
        format!("echo {SENTINEL} from atuin\n"),
    )
    .unwrap();

    assert!(scratch.psychic(&["history", "--enable"]).status.success());
    let home_text = home.to_string_lossy().into_owned();
    let hook_command = format!("echo {SENTINEL} from the hook");
    let track = |data_dir: &Path| -> Output {
        scratch.psychic_in(
            data_dir,
            &[
                "track-command",
                "--cwd",
                &home_text,
                "--exit-code",
                "1",
                "--session",
                "s1",
                "--",
                &hook_command,
            ],
        )
    };
    assert!(track(&scratch.data()).status.success());
    // A failing one: the data dir is a file, so the store cannot be opened.
    let not_a_dir = scratch.root.join("not-a-dir");
    std::fs::write(&not_a_dir, "").unwrap();
    let failed = track(&not_a_dir);
    assert!(
        String::from_utf8_lossy(&failed.stderr).starts_with("Error: could not create "),
        "recording into a file fails as it should: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    // And one that fails inside SQLite: its database is a directory.
    let broken = scratch.root.join("broken");
    std::fs::create_dir_all(broken.join("events.db")).unwrap();
    let failed = track(&broken);
    assert!(
        !failed.status.success() && !failed.stderr.is_empty(),
        "opening a directory as the database fails"
    );
    assert!(scratch.psychic(&["history", "--import"]).status.success());
    assert!(scratch.psychic(&["history", "--status"]).status.success());

    let data = scratch.data();
    let databases: Vec<Vec<u8>> = files(&data)
        .into_iter()
        .filter(|p| is_database(p))
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    for source in ["from bash", "from zsh", "from the hook"] {
        let text = format!("{SENTINEL} {source}");
        assert!(
            databases.iter().any(|bytes| contains(bytes, &text)),
            "{source:?} was recorded, so it went through its path"
        );
    }
    assert!(
        scratch
            .psychic(&["history", "--purge", "--yes"])
            .status
            .success()
    );

    let log = std::fs::read_to_string(data.join("app.log")).expect("the logger wrote app.log");
    assert!(
        log.contains("atuin skipped (unreadable)"),
        "app.log has the import's lines, so a leak would show:\n{log}"
    );
    let others: Vec<PathBuf> = files(&data)
        .into_iter()
        .filter(|p| !is_database(p))
        .collect();
    assert!(others.contains(&data.join("app.log")));
    for path in others {
        assert!(
            !contains(&std::fs::read(&path).unwrap(), SENTINEL),
            "{} holds command text",
            path.display()
        );
    }
    assert!(
        !home.join(".local/share/psychic").exists(),
        "nothing was written to the default data dir"
    );
}

/// Training (`03-learn.md` section 9: counts and feature names only): the
/// report on stdout, `training.log`, `app.log` and the model files hold no
/// command text, and neither does `--status` with a model.
#[test]
fn training_and_the_model_hold_no_command_text() {
    let scratch = Scratch::new("train");
    let data = scratch.data();
    assert!(
        scratch
            .psychic(&["history", "--enable", "--no-import"])
            .status
            .success()
    );
    for i in 0..80 {
        let text = format!("echo {SENTINEL} {}", i % 9);
        let started_at = (1_700_000_000 + i * 120).to_string();
        let output = scratch.psychic(&[
            "track-command",
            "--cwd",
            "/w",
            "--exit-code",
            "0",
            "--started-at",
            &started_at,
            "--session",
            "s1",
            "--",
            &text,
        ]);
        assert!(output.status.success());
    }
    let trained = scratch.psychic(&["history", "--retrain"]);
    assert!(trained.status.success());
    let report = String::from_utf8_lossy(&trained.stdout);
    assert!(report.starts_with("Trained on "), "{report}");
    assert!(
        scratch
            .psychic(&["history", "--retrain", "--quiet"])
            .status
            .success()
    );
    let status = scratch.psychic(&["history", "--status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("model: trained "),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
    let others: Vec<PathBuf> = files(&data)
        .into_iter()
        .filter(|p| !is_database(p))
        .collect();
    for name in [
        "cmd_model.txt",
        "cmd_model_stats.json",
        "cmd_train_attempt",
        "training.log",
        "app.log",
    ] {
        assert!(others.contains(&data.join(name)), "{name} was written");
    }
    for path in others {
        assert!(
            !contains(&std::fs::read(&path).unwrap(), SENTINEL),
            "{} holds command text",
            path.display()
        );
    }
}
