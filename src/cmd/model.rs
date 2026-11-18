//! The command model at serving time (`docs/ctrl-r/03-learn.md` section 7.1):
//! whether `cmd_model.txt` may be used, its blend weight, and prediction.
//!
//! LightGBM trains the model (`crate::training`) and writes it in its text
//! format; serving reads that text into a [`Forest`] and walks the trees in
//! Rust. Measured on the perf DB's model (132 trees): LightGBM's own
//! prediction ran on one thread (the `lightgbm3-sys` build has OpenMP off),
//! about 4 us a row, 8.5 ms for the 2,000 stage-2 rows of one keystroke;
//! parsing the file took it 13-17 ms; and a `lightgbm3::Booster` is neither
//! `Send` nor `Sync`, with two predictions on one booster racing on its
//! predictor state. A `Forest` is plain data: read and parsed once in about
//! 4 ms, shared by every rayon thread, no `unsafe`.
//!
//! The walk is `GBDT::PredictRaw` and `Tree::NumericalDecision` of LightGBM
//! 4.7.0 (`src/boosting/gbdt_prediction.cpp`, `include/LightGBM/tree.h`),
//! summed in the same order, so the scores are LightGBM's bit for bit (the
//! tests compare them). Only what command training produces is accepted:
//! one tree per iteration, numerical splits, no linear trees, no averaged
//! output. Anything else is refused, and ranking stays on the heuristic.

use super::features::{COMMAND_FEATURE_REGISTRY, command_feature_names};
use super::train::CmdModelStats;
use super::tuning::CMD_BLEND_SCALE;
use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;
use std::path::Path;

/// From this many rows a prediction runs on the rayon pool.
const PAR_PREDICT_MIN: usize = 256;

/// LightGBM's `kZeroThreshold` (`1e-35f`, a float widened to double):
/// values this close to 0 count as zero for a split whose missing type is Zero.
const ZERO_THRESHOLD: f64 = 1e-35_f32 as f64;

/// The command model of `cmd_model.txt`, with its stats and blend weight.
pub(super) struct CmdModel {
    forest: Forest,
    pub stats: CmdModelStats,
    /// How much of a stage-2 row's score is the model's (section 7.3).
    pub ml_weight: f64,
}

/// A weighted group count: finite and not negative.
fn is_count(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

/// The file-mode blend weight for `blend_examples` weighted groups, scaled
/// down by `CMD_BLEND_SCALE` (ADR-14).
pub(super) fn ml_weight(blend_examples: f64) -> f64 {
    assert!(
        is_count(blend_examples),
        "blend_examples is a non-negative count"
    );
    crate::ranker::Ranker::compute_blend_weights((blend_examples / CMD_BLEND_SCALE).round() as usize)
        .1
}

impl CmdModel {
    /// The model in `dir`, or `None` (ranking stays heuristic) when either
    /// file is missing or unreadable, or the model was trained with another
    /// feature list than this build's (ADR-15).
    pub(super) fn load(dir: &Path) -> Option<CmdModel> {
        let model_path = dir.join("cmd_model.txt");
        let stats_path = dir.join("cmd_model_stats.json");
        if !model_path.exists() || !stats_path.exists() {
            return None;
        }
        let stats: CmdModelStats = match std::fs::read_to_string(&stats_path)
            .map_err(anyhow::Error::from)
            .and_then(|json| Ok(serde_json::from_str::<CmdModelStats>(&json)?))
        {
            Ok(stats) if is_count(stats.blend_examples) => stats,
            _ => {
                log::warn!("command model stats unreadable; using the heuristic");
                return None;
            }
        };
        let forest = match std::fs::read_to_string(&model_path)
            .map_err(anyhow::Error::from)
            .and_then(|text| Forest::parse(&text))
        {
            Ok(forest) => forest,
            Err(_) => {
                log::warn!("command model unreadable; using the heuristic");
                return None;
            }
        };
        if !fits_registry(&forest.feature_names, &stats.feature_names) {
            log::info!(
                "command model was trained with a different feature set; \
                 using the heuristic until the next retrain"
            );
            return None;
        }
        Some(CmdModel {
            forest,
            ml_weight: ml_weight(stats.blend_examples),
            stats,
        })
    }

    /// Raw scores, one per row of the row-major `flat`, in row order. `Err`
    /// when the rows are not as wide as the model's input.
    pub(super) fn predict(&self, flat: &[f64], num_features: usize) -> Result<Vec<f64>> {
        ensure!(
            num_features == self.forest.feature_names.len(),
            "rows of {num_features} features for a model of {}",
            self.forest.feature_names.len()
        );
        assert!(
            flat.len().is_multiple_of(num_features),
            "whole rows of features"
        );
        let rows = flat.chunks_exact(num_features);
        Ok(if rows.len() >= PAR_PREDICT_MIN {
            flat.par_chunks_exact(num_features)
                .map(|row| self.forest.predict(row))
                .collect()
        } else {
            rows.map(|row| self.forest.predict(row)).collect()
        })
    }

    /// A model from a model text and stats that need not match, for tests
    /// of what happens when prediction fails.
    #[cfg(test)]
    pub(super) fn from_parts(model_text: &str, stats: CmdModelStats) -> Self {
        CmdModel {
            forest: Forest::parse(model_text).unwrap(),
            ml_weight: ml_weight(stats.blend_examples),
            stats,
        }
    }
}

/// Names compared in order, not only the count: replacing a feature keeps
/// the count and would silently repoint a column (ADR-15). Both the stats
/// and the model file name the features; both must match the registry.
fn fits_registry(model_names: &[String], stats_names: &[String]) -> bool {
    let registry = command_feature_names();
    assert_eq!(registry.len(), COMMAND_FEATURE_REGISTRY.len());
    model_names
        .iter()
        .map(String::as_str)
        .eq(registry.iter().copied())
        && stats_names
            .iter()
            .map(String::as_str)
            .eq(registry.iter().copied())
}

// ---------------------------------------------------------------------------
// The forest
// ---------------------------------------------------------------------------

/// The trees of a LightGBM model text, in order.
struct Forest {
    feature_names: Vec<String>,
    trees: Vec<Tree>,
}

/// One regression tree. Node `i < leaves - 1` is internal; a child below 0
/// is the leaf `!child`. Every internal child has a larger index than its
/// parent (checked at parse), so a walk always ends.
struct Tree {
    split_feature: Vec<usize>,
    threshold: Vec<f64>,
    decision_type: Vec<u8>,
    left_child: Vec<i32>,
    right_child: Vec<i32>,
    leaf_value: Vec<f64>,
}

/// `decision_type` bit 1: missing values go left.
const DEFAULT_LEFT: u8 = 2;
/// `decision_type` bit 0: a categorical split (refused at parse).
const CATEGORICAL: u8 = 1;
/// `(decision_type >> 2) & 3`.
const MISSING_ZERO: u8 = 1;
const MISSING_NAN: u8 = 2;

impl Forest {
    /// Read a LightGBM v4 model text.
    fn parse(text: &str) -> Result<Forest> {
        let (header, rest) = text
            .split_once("\nTree=")
            .context("the model has no trees")?;
        // LightGBM writes this flag as a bare line, without `=`.
        ensure!(
            !header.lines().any(|line| line.trim() == "average_output"),
            "an averaged model"
        );
        let header = key_values(header);
        let get = |key: &str| header.get(key).copied();
        ensure!(get("version") == Some("v4"), "not a LightGBM v4 model");
        ensure!(get("num_class") == Some("1"), "not a single-output model");
        ensure!(
            get("num_tree_per_iteration") == Some("1"),
            "not one tree per iteration"
        );
        let num_features: usize = get("max_feature_idx")
            .context("no max_feature_idx")?
            .parse::<usize>()?
            + 1;
        let feature_names: Vec<String> = get("feature_names")
            .context("no feature_names")?
            .split(' ')
            .map(str::to_owned)
            .collect();
        ensure!(
            feature_names.len() == num_features,
            "feature names and count disagree"
        );

        let (trees, end) = rest
            .split_once("end of trees")
            .context("the model text is cut short")?;
        ensure!(!end.is_empty(), "the model text is cut short");
        let trees = format!("Tree={trees}")
            .split("\nTree=")
            .map(|block| Tree::parse(block, num_features))
            .collect::<Result<Vec<Tree>>>()?;
        ensure!(!trees.is_empty(), "the model has no trees");
        Ok(Forest {
            feature_names,
            trees,
        })
    }

    /// The raw score of one row: the leaf values summed, tree by tree, from 0.
    fn predict(&self, row: &[f64]) -> f64 {
        let mut score = 0.0;
        for tree in &self.trees {
            score += tree.predict(row);
        }
        score
    }
}

impl Tree {
    fn parse(block: &str, num_features: usize) -> Result<Tree> {
        let fields = key_values(block);
        let get = |key: &str| fields.get(key).copied().unwrap_or("");
        let leaves: usize = get("num_leaves").parse().context("num_leaves")?;
        ensure!(leaves >= 1, "a tree without leaves");
        ensure!(get("num_cat") == "0", "categorical splits");
        ensure!(get("is_linear") == "0", "a linear tree");
        let leaf_value: Vec<f64> = numbers(get("leaf_value"))?;
        ensure!(leaf_value.len() == leaves, "leaf values");
        if leaves == 1 {
            return Ok(Tree {
                split_feature: Vec::new(),
                threshold: Vec::new(),
                decision_type: Vec::new(),
                left_child: Vec::new(),
                right_child: Vec::new(),
                leaf_value,
            });
        }
        let internal = leaves - 1;
        let tree = Tree {
            split_feature: numbers(get("split_feature"))?,
            threshold: numbers(get("threshold"))?,
            decision_type: numbers(get("decision_type"))?,
            left_child: numbers(get("left_child"))?,
            right_child: numbers(get("right_child"))?,
            leaf_value,
        };
        for (name, len) in [
            ("split_feature", tree.split_feature.len()),
            ("threshold", tree.threshold.len()),
            ("decision_type", tree.decision_type.len()),
            ("left_child", tree.left_child.len()),
            ("right_child", tree.right_child.len()),
        ] {
            ensure!(
                len == internal,
                "{name} has {len} entries for {internal} splits"
            );
        }
        for node in 0..internal {
            ensure!(
                tree.split_feature[node] < num_features,
                "a split on no feature"
            );
            ensure!(
                tree.decision_type[node] & CATEGORICAL == 0,
                "a categorical split"
            );
            for child in [tree.left_child[node], tree.right_child[node]] {
                let fits = if child >= 0 {
                    (child as usize) < internal && child as usize > node
                } else {
                    (!child as usize) < leaves
                };
                ensure!(fits, "a child out of order");
            }
        }
        Ok(tree)
    }

    fn predict(&self, row: &[f64]) -> f64 {
        if self.leaf_value.len() == 1 {
            return self.leaf_value[0];
        }
        let mut node = 0i32;
        while node >= 0 {
            let at = node as usize;
            node = self.decide(row[self.split_feature[at]], at);
        }
        self.leaf_value[!node as usize]
    }

    /// LightGBM's `Tree::NumericalDecision`, after its dense-row input
    /// conversion, which drops every value within `ZERO_THRESHOLD` of 0
    /// (`RowPairFunctionFromDenseMatric`), so the tree sees an exact 0.
    fn decide(&self, value: f64, node: usize) -> i32 {
        let decision = self.decision_type[node];
        let missing = (decision >> 2) & 3;
        let value = if (value.is_nan() && missing != MISSING_NAN) || value.abs() <= ZERO_THRESHOLD {
            0.0
        } else {
            value
        };
        let is_missing = (missing == MISSING_ZERO
            && (-ZERO_THRESHOLD..=ZERO_THRESHOLD).contains(&value))
            || (missing == MISSING_NAN && value.is_nan());
        let left = if is_missing {
            decision & DEFAULT_LEFT != 0
        } else {
            value <= self.threshold[node]
        };
        if left {
            self.left_child[node]
        } else {
            self.right_child[node]
        }
    }
}

/// The `key=value` lines of a block.
fn key_values(block: &str) -> std::collections::HashMap<&str, &str> {
    block
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect()
}

/// A space-separated list.
fn numbers<T: std::str::FromStr>(list: &str) -> Result<Vec<T>> {
    if list.is_empty() {
        bail!("an empty list");
    }
    list.split(' ')
        .map(|item| item.parse::<T>().ok().context("not a number"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::train::test_support::{TempDir, trained_fixture};

    #[test]
    fn tc_l_15_blend_table() {
        for (examples, expected) in [
            (0.0, 0.018),
            (150.0, 0.119),
            (300.0, 0.5),
            (450.0, 0.881),
            (600.0, 0.982),
        ] {
            let w = ml_weight(examples);
            assert!((w - expected).abs() <= 1e-3, "{examples}: {w}");
        }
    }

    #[test]
    fn a_model_trained_on_other_features_does_not_fit() {
        let names: Vec<String> = command_feature_names()
            .iter()
            .map(|n| n.to_string())
            .collect();
        assert!(fits_registry(&names, &names));
        assert!(!fits_registry(&names[1..], &names), "count");
        let mut renamed = names.clone();
        renamed.swap(0, 1);
        assert!(!fits_registry(&renamed, &names), "names in order (ADR-15)");
        assert!(!fits_registry(&names, &renamed), "the stats too");
    }

    /// Rows that reach every branch: NaN, zero, values on either side of
    /// LightGBM's zero threshold, negatives, and the spread of real features.
    fn awkward_rows(rows: usize, width: usize) -> Vec<f64> {
        (0..rows * width)
            .map(|i| match (i * 7919) % 13 {
                0 => f64::NAN,
                1 => 0.0,
                2 => -0.0,
                3 => 1e-36,
                4 => -1e-36,
                5 => 2e-35,
                6 => -7.5,
                _ => ((i * 104_729) % 1000) as f64 / 37.0,
            })
            .collect()
    }

    fn lightgbm(text: &str, flat: &[f64], width: usize) -> Vec<f64> {
        lightgbm3::Booster::from_string(text)
            .unwrap()
            .predict_with_params(flat, width as i32, true, "num_threads=1")
            .unwrap()
    }

    fn assert_same_bits(ours: &[f64], theirs: &[f64]) {
        assert_eq!(ours.len(), theirs.len());
        for (row, (a, b)) in ours.iter().zip(theirs).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "row {row}: {a} != {b}");
        }
    }

    #[test]
    fn the_forest_predicts_what_lightgbm_predicts_bit_for_bit() {
        let dir = TempDir::new("forest");
        trained_fixture(&dir.0);
        let text = std::fs::read_to_string(dir.0.join("cmd_model.txt")).unwrap();
        let width = COMMAND_FEATURE_REGISTRY.len();
        let flat = awkward_rows(600, width);
        let model = CmdModel::load(&dir.0).unwrap();
        // 600 rows: the parallel path; 10: the sequential one.
        assert_same_bits(
            &model.predict(&flat, width).unwrap(),
            &lightgbm(&text, &flat, width),
        );
        let few = &flat[..10 * width];
        assert_same_bits(
            &model.predict(few, width).unwrap(),
            &lightgbm(&text, few, width),
        );
        assert!(model.predict(&[], width).unwrap().is_empty());
    }

    #[test]
    fn every_missing_value_rule_matches_lightgbm() {
        let dir = TempDir::new("forest-missing");
        trained_fixture(&dir.0);
        let text = std::fs::read_to_string(dir.0.join("cmd_model.txt")).unwrap();
        // Every split gets one of the six numerical decision types in turn:
        // missing type None, Zero, NaN, each with missing values going
        // right or left. Without `tree_sizes` LightGBM parses the trees in
        // order, so the lines may change length.
        let mut turn = 0;
        let rewritten: Vec<String> = text
            .lines()
            .filter(|line| !line.starts_with("tree_sizes="))
            .map(|line| match line.strip_prefix("decision_type=") {
                Some(types) => {
                    let types: Vec<String> = types
                        .split(' ')
                        .map(|_| {
                            turn += 1;
                            ["0", "2", "4", "6", "8", "10"][turn % 6].to_string()
                        })
                        .collect();
                    format!("decision_type={}", types.join(" "))
                }
                None => line.to_string(),
            })
            .collect();
        let rewritten = rewritten.join("\n") + "\n";
        let width = COMMAND_FEATURE_REGISTRY.len();
        let flat = awkward_rows(400, width);
        let forest = Forest::parse(&rewritten).unwrap();
        let ours: Vec<f64> = flat.chunks(width).map(|row| forest.predict(row)).collect();
        assert_same_bits(&ours, &lightgbm(&rewritten, &flat, width));
    }

    #[test]
    fn what_command_training_does_not_produce_is_refused() {
        let dir = TempDir::new("forest-refused");
        trained_fixture(&dir.0);
        let text = std::fs::read_to_string(dir.0.join("cmd_model.txt")).unwrap();
        assert!(Forest::parse(&text).is_ok());
        let first_tree = text.find("num_cat=0").unwrap();
        let mut categorical = text.clone();
        categorical.replace_range(first_tree..first_tree + 9, "num_cat=1");
        assert!(Forest::parse(&categorical).is_err());
        assert!(Forest::parse(&text.replacen("num_class=1", "num_class=3", 1)).is_err());
        assert!(Forest::parse(&text.replacen("is_linear=0", "is_linear=1", 1)).is_err());
        assert!(Forest::parse(&text[..text.len() / 2]).is_err(), "cut short");
        let averaged = text.replacen("objective=", "average_output\nobjective=", 1);
        assert!(Forest::parse(&averaged).is_err(), "a random forest");
        // A child that points back up would loop forever.
        let looping = text.replacen("left_child=1 ", "left_child=0 ", 1);
        assert_ne!(looping, text);
        assert!(Forest::parse(&looping).is_err());
    }

    #[test]
    fn values_near_zero_are_zero_as_lightgbm_sees_them() {
        let dir = TempDir::new("forest-near-zero");
        trained_fixture(&dir.0);
        let text = std::fs::read_to_string(dir.0.join("cmd_model.txt")).unwrap();
        // Every split's threshold between 0 and LightGBM's zero threshold:
        // a row of 1e-36 is above it, but LightGBM reads it as 0.
        let rewritten: Vec<String> = text
            .lines()
            .filter(|line| !line.starts_with("tree_sizes="))
            .map(|line| match line.strip_prefix("threshold=") {
                Some(list) => {
                    let tiny: Vec<&str> = list.split(' ').map(|_| "5e-37").collect();
                    format!("threshold={}", tiny.join(" "))
                }
                None => line.to_string(),
            })
            .collect();
        let rewritten = rewritten.join("\n") + "\n";
        let width = COMMAND_FEATURE_REGISTRY.len();
        let flat = awkward_rows(400, width);
        let forest = Forest::parse(&rewritten).unwrap();
        let ours: Vec<f64> = flat.chunks(width).map(|row| forest.predict(row)).collect();
        assert_same_bits(&ours, &lightgbm(&rewritten, &flat, width));
    }

    #[test]
    fn stats_with_a_negative_weight_are_unreadable() {
        let dir = TempDir::new("forest-negative");
        trained_fixture(&dir.0);
        let path = dir.0.join("cmd_model_stats.json");
        let mut stats: CmdModelStats =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        stats.blend_examples = -1.0;
        std::fs::write(&path, serde_json::to_string(&stats).unwrap()).unwrap();
        assert!(CmdModel::load(&dir.0).is_none());
    }

    #[test]
    fn a_prediction_with_the_wrong_width_fails() {
        let dir = TempDir::new("forest-width");
        trained_fixture(&dir.0);
        let model = CmdModel::load(&dir.0).unwrap();
        assert!(model.predict(&[0.0; 10], 2).is_err());
    }
}
