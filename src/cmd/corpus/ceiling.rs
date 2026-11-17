//! The Bayes ceiling of a synthetic history (`docs/ctrl-r/04-bench.md`
//! section 5.5): how well the best possible ranker could do, one that knows
//! the generator's exact state and its probabilities for the next command.
//!
//! For each reused run of the holdout window (the last 20% of runs), the
//! generator's distribution over the next command is computed from the
//! state it chose from (given that the run reuses a command, as every
//! reachable lookup does). For a query of the first `len` characters of the
//! chosen command, the optimal order is that probability among the commands
//! starting with the same characters; the chosen command's rank under it
//! (ties shared) gives the metrics. Nothing a ranker can learn from history
//! can do better on average.
//!
//! Run by hand (it prints a table):
//! `CEILING_PROFILE=interactive CEILING_COMMANDS=50000 CEILING_RUNS=500000
//! CEILING_SEED=1 cargo test --release ceiling -- --ignored --nocapture`.

use super::*;

/// `P(zipf(u, n) = k)` for every `k`, exponent `s` (`zipf_s`'s inverse CDF).
fn zipf_pmf(n: usize, s: f64) -> Vec<f64> {
    let cdf = |y: f64| -> f64 {
        if s == 1.0 {
            y.ln() / ((n + 1) as f64).ln()
        } else {
            let a = 1.0 - s;
            (y.powf(a) - 1.0) / (((n + 1) as f64).powf(a) - 1.0)
        }
    };
    (0..n)
        .map(|k| {
            let upper = if k + 1 == n { 1.0 } else { cdf((k + 2) as f64) };
            upper - cdf((k + 1) as f64)
        })
        .collect()
}

/// The generator's probability of each command being the next, given that
/// the next run reuses a command (`choose_seen`): sparse, `(command, p)`.
fn next_distribution(world: &World, shape: &Shape, state: &Seen) -> FxHashMap<usize, f64> {
    let mut p: FxHashMap<usize, f64> = FxHashMap::default();
    let mut add = |command: usize, mass: f64| *p.entry(command).or_insert(0.0) += mass;
    let mut rest = 1.0;
    if let Some(failed) = state.retry {
        add(failed, shape.p_retry);
        rest = 1.0 - shape.p_retry;
    }
    let previous = state.recent.newest();
    let avoid = shape.avoid_repeat;
    let mut global = rest * (1.0 - shape.recent_below);

    // Repeat.
    let mass = rest * shape.repeat_below;
    if previous.is_some() {
        let commands = &state.recent.newest_first;
        let weights = &shape.repeat_weights[..commands.len()];
        let total: f64 = weights.iter().sum();
        for (&command, weight) in commands.iter().zip(weights) {
            add(command, mass * weight / total);
        }
    } else {
        global += mass;
    }
    // Chain.
    let mass = rest * (shape.chain_below - shape.repeat_below);
    match previous
        .and_then(|last| world.successor(last))
        .filter(|&next| world.reusable(next, shape.one_off))
        .filter(|&next| !(avoid && Some(next) == previous))
    {
        Some(next) => add(next, mass),
        None => global += mass,
    }
    // Directory.
    let mass = rest * (shape.dir_below - shape.chain_below);
    let in_dir = state.in_dir;
    if in_dir.is_empty() {
        global += mass;
    } else {
        for (pos, pk) in zipf_pmf(in_dir.len(), 1.0).into_iter().enumerate() {
            add(
                avoiding(avoid, previous, pos, in_dir.len(), |k| in_dir[k]),
                mass * pk,
            );
        }
    }
    // Recent.
    let mass = rest * (shape.recent_below - shape.dir_below);
    let older = state.working.newest_first.get(1..).unwrap_or_default();
    if older.is_empty() {
        global += mass;
    } else {
        for (pos, pk) in zipf_pmf(older.len(), shape.recent_s)
            .into_iter()
            .enumerate()
        {
            add(older[pos], mass * pk);
        }
    }
    // Global, and every branch that fell through to it.
    let reusable = state.reusable;
    for (pos, pk) in zipf_pmf(reusable.len(), shape.zipf_s)
        .into_iter()
        .enumerate()
    {
        add(
            avoiding(avoid, previous, pos, reusable.len(), |k| reusable[k]),
            global * pk,
        );
    }
    p
}

/// Expected rank metrics of `target` when every command with a higher
/// probability comes first and the ones with an equal probability share
/// the places after them: `(top1, top3, top10, mrr)`.
fn expected_metrics(better: usize, tied: usize) -> [f64; 4] {
    let places = tied + 1;
    let share = |k: usize| (k.saturating_sub(better)).min(places) as f64 / places as f64;
    let mrr = (better + 1..=better + places)
        .map(|r| 1.0 / r as f64)
        .sum::<f64>()
        / places as f64;
    [share(1), share(3), share(10), mrr]
}

#[test]
fn zipf_pmf_matches_the_samplers() {
    for (n, s) in [(1, 1.0), (7, 1.0), (50, 1.5), (300, 1.8), (20, 0.0)] {
        let pmf = zipf_pmf(n, s);
        assert!((pmf.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        let mut counts = vec![0usize; n];
        let mut rng = Stream { state: 11 };
        let draws = 200_000;
        for _ in 0..draws {
            counts[zipf_s(rng.u(), n, s)] += 1;
        }
        for k in 0..n {
            let observed = counts[k] as f64 / draws as f64;
            assert!(
                (observed - pmf[k]).abs() < 0.005,
                "n {n} s {s} k {k}: {observed} vs {}",
                pmf[k]
            );
        }
    }
    assert_eq!(expected_metrics(0, 0), [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(expected_metrics(2, 1)[..3], [0.0, 0.5, 1.0]);
}

#[test]
fn the_distribution_predicts_what_the_generator_chooses() {
    // Over many runs, the mean probability given to the chosen command
    // equals the mean of the probabilities' squares (both estimate
    // sum p^2), which fails if the distribution is not the generator's.
    let shape = Profile::Interactive.shape();
    let (mut chosen_p, mut expected_p, mut n) = (0.0, 0.0, 0);
    simulate_observed(500, 20_000, 3, &shape, |run, world, state, chosen| {
        if run % 7 != 0 {
            return;
        }
        let p = next_distribution(world, &shape, state);
        let total: f64 = p.values().sum();
        assert!((total - 1.0).abs() < 1e-9, "a distribution");
        chosen_p += p.get(&chosen).copied().unwrap_or(0.0);
        expected_p += p.values().map(|v| v * v).sum::<f64>();
        n += 1;
    });
    let (chosen_p, expected_p) = (chosen_p / n as f64, expected_p / n as f64);
    assert!(
        (chosen_p - expected_p).abs() < 0.01,
        "{chosen_p} vs {expected_p}"
    );
}

#[test]
#[ignore = "an analysis, run by hand (see the module comment)"]
fn bayes_ceiling() {
    let var = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
    let profile = match var("CEILING_PROFILE", "interactive").as_str() {
        "repetitive" => Profile::Repetitive,
        "agent" => Profile::Agent,
        _ => Profile::Interactive,
    };
    let commands: usize = var("CEILING_COMMANDS", "2000").parse().unwrap();
    let runs: usize = var("CEILING_RUNS", "20000").parse().unwrap();
    let seed: u64 = var("CEILING_SEED", "1").parse().unwrap();
    let shape = profile.shape();
    let world = World::new(seed);
    let texts: Vec<Vec<char>> = (0..commands)
        .map(|i| world.styled_text(shape.text, i).chars().take(3).collect())
        .collect();
    let from = runs * 8 / 10;
    let stride = ((runs - from) / 2000).max(1);
    let mut sums = [[0.0f64; 4]; 4];
    let mut n = 0usize;
    simulate_observed(commands, runs, seed, &shape, |run, world, state, chosen| {
        if run < from || !(run - from).is_multiple_of(stride) {
            return;
        }
        let p = next_distribution(world, &shape, state);
        let target = p.get(&chosen).copied().unwrap_or(0.0);
        for (len, sum) in sums.iter_mut().enumerate() {
            let prefix = &texts[chosen][..len.min(texts[chosen].len())];
            let (mut better, mut tied) = (0, 0);
            for (&command, &pc) in &p {
                if command == chosen || texts[command].get(..prefix.len()) != Some(prefix) {
                    continue;
                }
                if pc > target {
                    better += 1;
                } else if pc == target {
                    tied += 1;
                }
            }
            for (s, m) in sum.iter_mut().zip(expected_metrics(better, tied)) {
                *s += m;
            }
        }
        n += 1;
    });
    println!(
        "Bayes ceiling, {profile:?} {commands}/{runs} seed {seed}: {n} reused runs of the last 20%"
    );
    println!("| len | top1 | top3 | top10 | mrr |");
    for (len, sum) in sums.iter().enumerate() {
        let m: Vec<String> = sum.iter().map(|s| format!("{:.3}", s / n as f64)).collect();
        println!("| {len} | {} |", m.join(" | "));
    }
}
