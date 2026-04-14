//! Feature implication graph and redundant-combination pruning.
//!
//! Cargo features can imply other features (e.g. `B = ["A"]`). When this
//! happens, the combination `[A, B]` resolves to the same effective feature
//! set as `[B]` alone. This module detects and prunes such redundant
//! combinations.

use petgraph::graphmap::DiGraphMap;
use petgraph::visit::Dfs;
use std::collections::{BTreeMap, BTreeSet};

/// A feature combination that was pruned because another (smaller)
/// combination resolves to the same effective feature set.
#[derive(Debug, Clone)]
pub struct PrunedCombination {
    /// The feature names in this pruned combination.
    pub features: Vec<String>,
    /// The feature names in the representative (kept) combination that has
    /// the same resolved set.
    pub equivalent_to: Vec<String>,
}

/// Result of partitioning feature combinations via implied-feature pruning.
#[derive(Debug)]
pub struct PruneResult<'a> {
    /// Combinations to actually execute.
    pub keep: Vec<Vec<&'a String>>,
    /// Combinations that were pruned, with their equivalence info.
    pub pruned: Vec<PrunedCombination>,
}

/// Build a directed implication graph from a package's feature definitions.
///
/// An edge `A → B` means enabling feature A also enables feature B. Only
/// plain feature names are considered — dependency activations (`dep:X`,
/// `crate/feature`, `?dep:X/feature`) are filtered out.
fn build_implication_graph(features: &BTreeMap<String, Vec<String>>) -> DiGraphMap<&str, ()> {
    let mut graph = DiGraphMap::new();

    // Every feature is a node, even if it implies nothing — resolved_set
    // relies on DFS starting from nodes that exist in the graph.
    for key in features.keys() {
        graph.add_node(key.as_str());
    }

    for (feature_name, implied_values) in features {
        for implied in implied_values {
            // Feature values can be:
            //   "other_feature"          → intra-crate implication (what we want)
            //   "dep:name"               → optional dep activation
            //   "crate/feature"          → dep feature activation
            //   "?dep:name/feature"      → weak dep feature
            // Only plain names that match a key in the feature map form edges.
            if implied.contains('/') || implied.contains(':') {
                continue;
            }
            if features.contains_key(implied) && implied != feature_name {
                graph.add_edge(feature_name.as_str(), implied.as_str(), ());
            }
        }
    }

    graph
}

/// Compute the resolved feature set for a single combination.
///
/// The resolved set is the combination itself plus all features transitively
/// reachable via the implication graph. Uses `&str` references into the
/// features map to avoid allocations.
fn resolved_set<'a>(combo: &[&'a String], graph: &DiGraphMap<&'a str, ()>) -> BTreeSet<&'a str> {
    let mut resolved = BTreeSet::new();
    for feature in combo {
        if resolved.insert(feature.as_str()) {
            let mut dfs = Dfs::new(graph, feature.as_str());
            while let Some(reachable) = dfs.next(graph) {
                resolved.insert(reachable);
            }
        }
    }
    resolved
}

/// Apply implied-feature pruning if the config enables it, otherwise return
/// combos unchanged.
///
/// Callers that only need the kept combinations can use `.keep` on the result
/// and ignore `.pruned`.
#[must_use]
pub fn maybe_prune<'a>(
    combos: Vec<Vec<&'a String>>,
    features: &'a BTreeMap<String, Vec<String>>,
    config: &crate::config::Config,
    no_prune: bool,
) -> PruneResult<'a> {
    // allow_feature_sets is an explicit allowlist where the user declared the
    // exact sets they care about — pruning would silently drop entries they
    // asked for.
    let active = config.prune_implied && !no_prune && config.allow_feature_sets.is_empty();
    if active {
        prune_implied_combinations(combos, features)
    } else {
        PruneResult {
            keep: combos,
            pruned: Vec::new(),
        }
    }
}

/// Partition feature combinations into kept and pruned groups.
///
/// Combinations with identical resolved feature sets are grouped together.
/// From each group the smallest combination (fewest features, then
/// lexicographic) is kept as the representative; the rest are pruned.
fn prune_implied_combinations<'a>(
    combos: Vec<Vec<&'a String>>,
    features: &'a BTreeMap<String, Vec<String>>,
) -> PruneResult<'a> {
    let graph = build_implication_graph(features);

    // Group combos by their resolved feature set — the full set of features
    // that Cargo would enable after applying all transitive implications.
    // Combos in the same group produce identical compiled output.
    let mut groups: BTreeMap<BTreeSet<&'a str>, Vec<Vec<&'a String>>> = BTreeMap::new();
    for combo in combos {
        let resolved = resolved_set(&combo, &graph);
        groups.entry(resolved).or_default().push(combo);
    }

    let mut keep = Vec::new();
    let mut pruned = Vec::new();

    for (_resolved, mut group) in groups {
        // Within each equivalence group, the smallest combo (fewest explicit
        // features) is the canonical representative — it's the one that
        // actually needs to be tested. Ties are broken lexicographically for
        // deterministic output.
        group.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));

        let representative = group.remove(0);
        let representative_features: Vec<String> =
            representative.iter().copied().cloned().collect();

        for redundant in group {
            pruned.push(PrunedCombination {
                features: redundant.iter().copied().cloned().collect(),
                equivalent_to: representative_features.clone(),
            });
        }

        keep.push(representative);
    }

    keep.sort();
    pruned.sort_by(|a, b| a.features.cmp(&b.features));

    PruneResult { keep, pruned }
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn features(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_string(),
                    v.iter().copied().map(String::from).collect(),
                )
            })
            .collect()
    }

    fn vs(strs: &[&str]) -> Vec<String> {
        strs.iter().copied().map(String::from).collect()
    }

    type Kept = Vec<Vec<String>>;
    type Pruned = Vec<(Vec<String>, Vec<String>)>;

    fn run_prune(feat: &BTreeMap<String, Vec<String>>) -> eyre::Result<(Kept, Pruned)> {
        use crate::config::Config;
        use crate::package::Package;

        let mut pkg = crate::package::test::package_with_features(&[])?;
        pkg.features = feat.clone();

        let config = Config {
            prune_implied: true,
            ..Config::default()
        };
        let combos = pkg.feature_combinations(&config)?;
        let result = prune_implied_combinations(combos, &pkg.features);

        let kept: Vec<Vec<String>> = result
            .keep
            .iter()
            .map(|c| c.iter().map(|s| (*s).clone()).collect())
            .collect();
        let pruned: Vec<(Vec<String>, Vec<String>)> = result
            .pruned
            .iter()
            .map(|p| (p.features.clone(), p.equivalent_to.clone()))
            .collect();
        Ok((kept, pruned))
    }

    #[test]
    fn no_implications_no_pruning() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &[]), ("C", &[])]);
        let (kept, pruned) = run_prune(&feat)?;
        sim_assert_eq!(pruned.len(), 0);
        sim_assert_eq!(kept.len(), 8); // 2^3 = 8
        Ok(())
    }

    #[test]
    fn simple_implication_b_implies_a() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &["A"])]);
        let (kept, pruned) = run_prune(&feat)?;

        // [A, B] has the same resolved set as [B]: {A, B}
        sim_assert_eq!(kept, vec![vs(&[]), vs(&["A"]), vs(&["B"])]);
        sim_assert_eq!(pruned, vec![(vs(&["A", "B"]), vs(&["B"]))]);
        Ok(())
    }

    #[test]
    fn transitive_chain_c_b_a() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &["A"]), ("C", &["B"])]);
        let (kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(kept, vec![vs(&[]), vs(&["A"]), vs(&["B"]), vs(&["C"])]);
        sim_assert_eq!(pruned.len(), 4);
        Ok(())
    }

    #[test]
    fn dep_syntax_ignored() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &["dep:some_dep", "A"])]);
        let (kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(kept, vec![vs(&[]), vs(&["A"]), vs(&["B"])]);
        sim_assert_eq!(pruned, vec![(vs(&["A", "B"]), vs(&["B"]))]);
        Ok(())
    }

    #[test]
    fn slash_syntax_ignored() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &["some-crate/feature", "A"])]);
        let (_kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(pruned, vec![(vs(&["A", "B"]), vs(&["B"]))]);
        Ok(())
    }

    #[test]
    fn weak_dep_syntax_ignored() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &["?dep:x/y", "A"])]);
        let (_kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(pruned, vec![(vs(&["A", "B"]), vs(&["B"]))]);
        Ok(())
    }

    #[test]
    fn nonexistent_implied_feature_ignored() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &["NonExistent"])]);
        let (kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(pruned.len(), 0);
        sim_assert_eq!(kept.len(), 4); // 2^2 = 4
        Ok(())
    }

    #[test]
    fn diamond_graph() -> eyre::Result<()> {
        let feat = features(&[("A", &[]), ("B", &["A"]), ("C", &["A"]), ("D", &["B", "C"])]);
        let (kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(
            kept,
            vec![
                vs(&[]),
                vs(&["A"]),
                vs(&["B"]),
                vs(&["B", "C"]),
                vs(&["C"]),
                vs(&["D"]),
            ]
        );
        sim_assert_eq!(pruned.len(), 10);
        Ok(())
    }

    #[test]
    fn mutual_implication_lexicographic_tiebreak() -> eyre::Result<()> {
        let feat = features(&[("A", &["B"]), ("B", &["A"])]);
        let (kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(kept, vec![vs(&[]), vs(&["A"])]);
        sim_assert_eq!(pruned.len(), 2);
        Ok(())
    }

    #[test]
    fn resolved_set_correctness() {
        let feat = features(&[("A", &[]), ("B", &["A"]), ("C", &["B"])]);
        let graph = build_implication_graph(&feat);

        let a = "A".to_string();
        let b = "B".to_string();
        let c = "C".to_string();

        sim_assert_eq!(resolved_set(&[], &graph), BTreeSet::<&str>::new());
        sim_assert_eq!(resolved_set(&[&a], &graph), BTreeSet::from(["A"]));
        sim_assert_eq!(resolved_set(&[&b], &graph), BTreeSet::from(["A", "B"]));
        sim_assert_eq!(resolved_set(&[&c], &graph), BTreeSet::from(["A", "B", "C"]));
        sim_assert_eq!(resolved_set(&[&a, &b], &graph), BTreeSet::from(["A", "B"]));
    }

    #[test]
    fn self_referencing_feature_no_crash() -> eyre::Result<()> {
        let feat = features(&[("A", &["A"]), ("B", &[])]);
        let (kept, pruned) = run_prune(&feat)?;

        sim_assert_eq!(pruned.len(), 0);
        sim_assert_eq!(kept.len(), 4);
        Ok(())
    }

    #[test]
    fn empty_features_no_pruning() {
        let feat: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let result = prune_implied_combinations(Vec::new(), &feat);
        sim_assert_eq!(result.keep.len(), 0);
        sim_assert_eq!(result.pruned.len(), 0);
    }
}
