//! `bench-history twostage` and `bench-history latency`
//! (`docs/ctrl-r/04-bench.md` sections 6.5 and 6.6): the ranking measured
//! without a terminal, on the perf DB.
//!
//! Both call the production filter and ranking; neither writes anything.

use super::super::index::{CommandIndex, LoadParams};
use super::super::model::CmdModel;
use super::super::rank::{
    CommandCandidate, CommandRanking, CommandScore, Stage2, rank_commands, rank_with,
};
use super::super::repo;
use super::super::store::CmdStore;
use super::super::train::{ReplayInput, replay};
use super::super::tuning::{CMD_PAGE_ROWS, CMD_STAGE2_K};
use super::super::worker::{Scope, filter_candidates, narrowing_applies, replay_filter};
use anyhow::{Context, Result, bail};
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::Deserialize;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// B8 (00-blueprint.md section 2): mean top-10 overlap and top-1 agreement.
const B8_OVERLAP: f64 = 0.98;
const B8_TOP1: f64 = 0.995;

/// `p(q) = sorted(v)[min(len - 1, ceil(q * len) - 1)]` (section 6); NaN when empty.
pub(super) fn percentile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        f64::NAN
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// The model in `data_dir`, or an error that says how to get one.
fn load_model(data_dir: &Path) -> Result<CmdModel> {
    CmdModel::load(data_dir).with_context(|| {
        format!(
            "no usable cmd_model.txt in {}: run `psychic --data-dir {} history --retrain`",
            data_dir.display(),
            data_dir.display()
        )
    })
}

// ---------------------------------------------------------------------------
// twostage (section 6.5)
// ---------------------------------------------------------------------------

/// What one lookup of the two-stage check found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Agreement {
    /// `|top10(full) ∩ top10(two)| / min(10, matched)`.
    pub overlap: f64,
    pub same_first: bool,
}

/// Compare two rankings of the same `matched > 0` candidates.
pub(super) fn agreement(full: &[CommandScore], two: &[CommandScore]) -> Agreement {
    let matched = full.len();
    assert!(
        matched > 0 && matched == two.len(),
        "two rankings of the same candidates"
    );
    let top =
        |scores: &[CommandScore]| -> Vec<u32> { scores.iter().take(10).map(|s| s.idx).collect() };
    let (a, b) = (top(full), top(two));
    let common = a.iter().filter(|idx| b.contains(idx)).count();
    Agreement {
        overlap: common as f64 / matched.min(10) as f64,
        same_first: full[0].idx == two[0].idx,
    }
}

pub(super) fn twostage(data_dir: &Path, queries: usize, out: &mut dyn Write) -> Result<()> {
    if queries == 0 {
        bail!("--queries must be at least 1");
    }
    let model = load_model(data_dir)?;
    let input = {
        let store = CmdStore::open(data_dir)?;
        ReplayInput::read(&store)?
    };
    if input.runs.is_empty() {
        bail!("--data-dir holds no runs");
    }
    let replayer = replay(&input, None);
    let index = &replayer.index;
    let now = input.runs.last().expect("not empty").started_at + 1;
    let matcher = SkimMatcherV2::default();
    let predict = |flat: &[f64], width: usize| model.predict(flat, width);

    let first = (input.runs.len() as f64 * 0.8).floor() as usize;
    let tail = &input.runs[first..];
    let stride = (tail.len() as f64 / queries as f64).max(f64::MIN_POSITIVE);
    let (mut overlaps, mut firsts, mut over_k, mut skipped) = (Vec::new(), 0usize, 0usize, 0usize);
    let (mut full_ms, mut two_ms) = (Vec::new(), Vec::new());
    for j in 0..queries {
        let run = &tail[((j as f64 * stride).floor() as usize).min(tail.len() - 1)];
        let Some(idx) = replayer.knows(run.command_id) else {
            skipped += 1;
            continue;
        };
        let query: String = index.entries[idx as usize]
            .text
            .chars()
            .take(j % 4)
            .collect();
        let ctx = replayer.context(
            now,
            run.cwd.as_deref(),
            run.repo_root.as_deref(),
            Some(&run.shell_session),
        );
        let candidates = filter_candidates(index, &ctx, &matcher, &query, Scope::All, None);
        if candidates.is_empty() {
            skipped += 1;
            continue;
        }
        let stage2 = |k: usize| Stage2 {
            k,
            ml_weight: model.ml_weight,
            predict: &predict,
        };
        // (rows scored by the model, rows sorted) -> the ranking and its time.
        let timed = |k: usize, head: usize| {
            let started = Instant::now();
            let ranking = rank_with(&query, &candidates, index, &ctx, head, Some(stage2(k)));
            (ranking, started.elapsed().as_secs_f64() * 1000.0)
        };
        // Alternate which goes first, so neither always gets the warm caches.
        let ((full, full_t), (two, two_t)) = if j % 2 == 0 {
            let full = timed(usize::MAX, usize::MAX);
            (full, timed(CMD_STAGE2_K, CMD_PAGE_ROWS))
        } else {
            let two = timed(CMD_STAGE2_K, CMD_PAGE_ROWS);
            (timed(usize::MAX, usize::MAX), two)
        };
        full_ms.push(full_t);
        two_ms.push(two_t);
        if full.model_failed || two.model_failed {
            bail!("the model's prediction failed");
        }
        let agreed = agreement(&full.scores, &two.scores);
        overlaps.push(agreed.overlap);
        firsts += usize::from(agreed.same_first);
        over_k += usize::from(candidates.len() > CMD_STAGE2_K);
    }
    let n = overlaps.len();
    if n == 0 {
        bail!("no lookup had a match");
    }
    let overlap = mean(&overlaps);
    let top1 = firsts as f64 / n as f64;
    let verdict = if overlap >= B8_OVERLAP && top1 >= B8_TOP1 {
        "PASS"
    } else {
        "FAIL"
    };
    writeln!(out, "lookups: {n} (skipped {skipped})")?;
    writeln!(out, "mean top-10 overlap: {overlap:.4}")?;
    writeln!(out, "top-1 agreement: {top1:.4}")?;
    writeln!(
        out,
        "share with matched > K ({CMD_STAGE2_K}): {:.3}",
        over_k as f64 / n as f64
    )?;
    writeln!(
        out,
        "rank time, full scoring (k = all, every row sorted) p50/p95: {:.2} / {:.2} ms",
        percentile(&full_ms, 0.5),
        percentile(&full_ms, 0.95)
    )?;
    writeln!(
        out,
        "rank time, two-stage (k = {CMD_STAGE2_K}, head {CMD_PAGE_ROWS}) p50/p95: {:.2} / {:.2} ms",
        percentile(&two_ms, 0.5),
        percentile(&two_ms, 0.95)
    )?;
    writeln!(
        out,
        "B8 {overlap:.4} / {top1:.4} (target {B8_OVERLAP} / {B8_TOP1}) {verdict}"
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// latency (section 6.6)
// ---------------------------------------------------------------------------

/// `bench-context.json`, written by `bench/history.py setup`.
#[derive(Deserialize)]
struct BenchContext {
    cwd: String,
    session: String,
}

/// The keys of section 6.3: six words, each typed and then deleted with six
/// Backspaces. Each word starts with another letter, so its first key is a
/// full scan as at the start of a real search, not a reuse of the last one.
const KEY_CYCLE: &[u8] = b"git st\x7f\x7f\x7f\x7f\x7f\x7f\
cargo \x7f\x7f\x7f\x7f\x7f\x7f\
docker\x7f\x7f\x7f\x7f\x7f\x7f\
make t\x7f\x7f\x7f\x7f\x7f\x7f\
npm in\x7f\x7f\x7f\x7f\x7f\x7f\
kubect\x7f\x7f\x7f\x7f\x7f\x7f";

/// The query after `key`.
fn apply_key(query: &mut String, key: u8) {
    if key == 0x7F {
        query.pop();
    } else {
        query.push(char::from(key));
    }
}

/// Reads one stage's time from a keystroke's times.
type Stage = fn(&KeyTimes) -> f64;

/// One keystroke's stage times, in ms.
#[derive(Default, Clone, Copy)]
struct KeyTimes {
    filter: f64,
    heuristic: f64,
    features: f64,
    predict: f64,
    blend: f64,
    sort: f64,
    total: f64,
    matched: usize,
    stage2: usize,
    query_len: usize,
}

/// How `latency` runs.
pub(super) struct LatencyOptions {
    /// Keys recorded, after one unrecorded cycle.
    pub keystrokes: usize,
    /// Slept before each recorded key, as a typist pauses.
    pub pause: Duration,
    /// Where to write one line per key (warm-up cycle included) with
    /// digests of what the worker's filter answers and its ranking (ids and
    /// scores, never text), to compare two builds made with the same Rust
    /// toolchain (the digest is std's `DefaultHasher`); `None` = no dump.
    pub dump: Option<PathBuf>,
}

/// One dump line: the key's number, query length, matches, a digest of the
/// candidates (idx, fuzzy score) in order, how many rows are sorted, and a
/// digest of every score in ranking order.
fn dump_line(
    key: usize,
    query_len: usize,
    candidates: &[CommandCandidate],
    ranking: &CommandRanking,
) -> String {
    let mut matched = DefaultHasher::new();
    for c in candidates {
        (c.idx, c.fuzzy_score).hash(&mut matched);
    }
    let mut ranked = DefaultHasher::new();
    for s in &ranking.scores {
        (
            s.idx,
            s.score.to_bits(),
            s.heuristic.to_bits(),
            s.ml_score.map(f64::to_bits),
            s.fuzzy_score,
        )
            .hash(&mut ranked);
    }
    format!(
        "{key}\t{query_len}\t{}\t{:016x}\t{}\t{:016x}",
        candidates.len(),
        matched.finish(),
        ranking.sorted,
        ranked.finish()
    )
}

pub(super) fn latency(
    data_dir: &Path,
    options: &LatencyOptions,
    home: Option<&Path>,
    now: i64,
    out: &mut dyn Write,
) -> Result<()> {
    let (keystrokes, pause) = (options.keystrokes, options.pause);
    let context_path = data_dir.join("bench-context.json");
    let Ok(context) = std::fs::read_to_string(&context_path) else {
        bail!("run ./bench/history.py setup first");
    };
    let context: BenchContext = serde_json::from_str(&context)
        .with_context(|| format!("{} is not a bench context", context_path.display()))?;
    // As the picker resolves it (app.rs): from the file system.
    let repo_root = repo::find_repo_root(Path::new(&context.cwd), home)
        .map(|root| root.to_string_lossy().into_owned());
    let params = LoadParams {
        now,
        cwd: Some(context.cwd.clone()),
        repo_root,
        shell_session: Some(context.session.clone()),
    };
    let started = Instant::now();
    let store = CmdStore::open(data_dir)?;
    let (index, ctx) = CommandIndex::load(&store, &params)?;
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let model = CmdModel::load(data_dir);
    let model_ms = started.elapsed().as_secs_f64() * 1000.0;
    let matcher = SkimMatcherV2::default();

    // One cycle first, not recorded: it warms the thread pool and the caches
    // as the picker's first query and start-up do.
    let mut query = String::new();
    let mut previous: Vec<CommandCandidate> = Vec::new();
    let mut previous_query = String::new();
    let mut times = Vec::with_capacity(keystrokes);
    for i in 0..KEY_CYCLE.len() + keystrokes {
        apply_key(&mut query, KEY_CYCLE[i % KEY_CYCLE.len()]);
        if i >= KEY_CYCLE.len() && !pause.is_zero() {
            std::thread::sleep(pause);
        }
        let started = Instant::now();
        // The worker's narrowing rule; nothing earlier is reused.
        let base: Option<Vec<u32>> =
            narrowing_applies(&query, Scope::All, &previous_query, Scope::All)
                .then(|| previous.iter().map(|c| c.idx).collect());
        let candidates =
            filter_candidates(&index, &ctx, &matcher, &query, Scope::All, base.as_deref());
        let filter = started.elapsed().as_secs_f64() * 1000.0;
        let ranking = rank_commands(
            &query,
            &candidates,
            &index,
            &ctx,
            CMD_PAGE_ROWS,
            model.as_ref(),
        );
        let total = started.elapsed().as_secs_f64() * 1000.0;
        let t = ranking.timings;
        if i >= KEY_CYCLE.len() {
            times.push(KeyTimes {
                filter,
                heuristic: t.heuristic_ms,
                features: t.features_ms.unwrap_or(0.0),
                predict: t.predict_ms.unwrap_or(0.0),
                blend: t.blend_ms.unwrap_or(0.0),
                sort: t.sort_ms,
                total,
                matched: candidates.len(),
                stage2: t.stage2,
                query_len: query.chars().count(),
            });
        }
        previous = candidates;
        previous_query.clone_from(&query);
    }

    writeln!(
        out,
        "commands: {}, index load {load_ms:.1} ms, model {} ({model_ms:.1} ms)",
        index.entries.len(),
        if model.is_some() {
            "loaded"
        } else {
            "absent: heuristic only"
        }
    )?;
    writeln!(
        out,
        "keystrokes: {} after a {}-key warm-up, {} ms pause before each; filter + rank on this thread, head {CMD_PAGE_ROWS}",
        times.len(),
        KEY_CYCLE.len(),
        pause.as_millis()
    )?;
    writeln!(out, "stage (ms)          mean      p50      p95      max")?;
    let column =
        |pick: fn(&KeyTimes) -> f64, of: &[KeyTimes]| -> Vec<f64> { of.iter().map(pick).collect() };
    let stages: [(&str, Stage); 7] = [
        ("filter", |k| k.filter),
        ("heuristic", |k| k.heuristic),
        ("sort (h.+blend)", |k| k.sort),
        ("features", |k| k.features),
        ("predict", |k| k.predict),
        ("blend", |k| k.blend),
        ("total", |k| k.total),
    ];
    for (name, pick) in stages {
        let v = column(pick, &times);
        writeln!(
            out,
            "{name:<16} {:8.3} {:8.3} {:8.3} {:8.3}",
            mean(&v),
            percentile(&v, 0.5),
            percentile(&v, 0.95),
            v.iter().copied().fold(f64::NAN, f64::max)
        )?;
    }
    writeln!(
        out,
        "(sort = selecting and sorting the heuristic head plus sorting the stage-2 rows, already inside heuristic and blend; total = filter + rank, without building the result rows the worker also counts in total_ms)"
    )?;
    for len in 0..=6 {
        let of: Vec<KeyTimes> = times
            .iter()
            .copied()
            .filter(|k| k.query_len == len)
            .collect();
        if of.is_empty() {
            continue;
        }
        let total = column(|k| k.total, &of);
        let filter = column(|k| k.filter, &of);
        let matched: Vec<f64> = of.iter().map(|k| k.matched as f64).collect();
        writeln!(
            out,
            "len {len}: n {:3}  total p50 {:7.3} p95 {:7.3}  filter p50 {:7.3} p95 {:7.3}  matched p50 {:6.0}  stage2 {}",
            of.len(),
            percentile(&total, 0.5),
            percentile(&total, 0.95),
            percentile(&filter, 0.5),
            percentile(&filter, 0.95),
            percentile(&matched, 0.5),
            of[0].stage2
        )?;
    }
    if let Some(path) = &options.dump {
        // The same keys again, through the worker's own filter (seeds,
        // trail, reuse), each answer ranked as the worker ranks it.
        let mut dump = std::io::BufWriter::new(
            std::fs::File::create(path)
                .with_context(|| format!("could not create {}", path.display()))?,
        );
        let mut query = String::new();
        let queries: Vec<String> = (0..KEY_CYCLE.len() + keystrokes)
            .map(|i| {
                apply_key(&mut query, KEY_CYCLE[i % KEY_CYCLE.len()]);
                query.clone()
            })
            .collect();
        let mut key = 0;
        let mut failed = Ok(());
        replay_filter(index, ctx, queries, |index, ctx, query, candidates| {
            let ranking =
                rank_commands(query, candidates, index, ctx, CMD_PAGE_ROWS, model.as_ref());
            let line = dump_line(key, query.chars().count(), candidates, &ranking);
            if failed.is_ok() {
                failed = writeln!(dump, "{line}");
            }
            key += 1;
        });
        failed?;
        dump.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_follows_the_spec_formula() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.5), 10.0);
        assert_eq!(percentile(&v, 0.95), 19.0);
        assert_eq!(percentile(&v, 1.0), 20.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
        assert!(percentile(&[], 0.5).is_nan());
        // Unsorted input.
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 0.5), 2.0);
    }

    #[test]
    fn the_key_cycle_covers_lengths_0_to_6_both_ways() {
        let mut query = String::new();
        let mut lengths = Vec::new();
        for &key in KEY_CYCLE {
            apply_key(&mut query, key);
            lengths.push(query.len());
        }
        assert_eq!(KEY_CYCLE.len(), 72);
        assert_eq!(lengths[..12], [1, 2, 3, 4, 5, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(lengths[..12], lengths[60..]);
        // Each word starts from the empty query with a new first letter.
        let firsts: Vec<u8> = KEY_CYCLE.chunks(12).map(|word| word[0]).collect();
        assert_eq!(firsts, b"gcdmnk");
    }
}
