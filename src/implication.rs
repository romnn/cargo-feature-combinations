//! Feature implication graph and redundant-combination pruning.
//!
//! Cargo features can imply other features (e.g. `B = ["A"]`). When this
//! happens, the combination `[A, B]` resolves to the same effective feature
//! set as `[B]` alone. This module detects and prunes such redundant
//! combinations.

use petgraph::graphmap::DiGraphMap;
use petgraph::visit::Dfs;
use std::collections::{BTreeMap, BTreeSet, HashSet};

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
/// An edge `A → B` means enabling feature A also enables feature B.
/// That holds when A lists B, and when A lists `B/feature` for an optional dependency named `B`:
/// Cargo then also enables the package's own feature `B`, the implicit one for the dependency or
/// an explicit one of that name.
/// Other values enable no feature of this package and form no edge: `dep:B` only enables the
/// dependency, the weak `B?/feature` never enables it, and `B/feature` for a non-optional
/// dependency leaves a same-named feature off.
///
/// A target-specific optional dependency forms no edge either.
/// Cargo enables its feature only when the dependency is active for the target being built,
/// while this graph serves every target, and an edge Cargo does not follow would let pruning
/// merge combinations that build differently.
fn build_implication_graph(package: &cargo_metadata::Package) -> DiGraphMap<&str, ()> {
    let features = &package.features;
    let optional_dependencies = package
        .dependencies
        .iter()
        .filter(|dependency| dependency.optional && dependency.target.is_none())
        .map(|dependency| {
            dependency
                .rename
                .as_deref()
                .unwrap_or(dependency.name.as_str())
        })
        .collect::<HashSet<_>>();

    let mut graph = DiGraphMap::new();

    // Every feature is a node, even if it implies nothing — resolved_set
    // relies on DFS starting from nodes that exist in the graph.
    for key in features.keys() {
        graph.add_node(key.as_str());
    }

    for (feature_name, implied_values) in features {
        for implied in implied_values {
            let implied_feature = match implied.split_once('/') {
                Some((dependency, _)) if optional_dependencies.contains(dependency) => dependency,
                Some(_) => continue,
                None if implied.contains(':') => continue,
                None => implied.as_str(),
            };
            if let Some((implied_feature, _)) = features.get_key_value(implied_feature)
                && implied_feature != feature_name
            {
                graph.add_edge(feature_name.as_str(), implied_feature.as_str(), ());
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
fn resolved_set<'a>(
    combo: impl IntoIterator<Item = &'a String>,
    graph: &DiGraphMap<&'a str, ()>,
) -> BTreeSet<&'a str> {
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

/// The features a combination enables once Cargo applies one package's feature implications.
pub(crate) struct FeatureImplications<'a> {
    graph: DiGraphMap<&'a str, ()>,
}

impl<'a> FeatureImplications<'a> {
    pub(crate) fn new(package: &'a cargo_metadata::Package) -> Self {
        Self {
            graph: build_implication_graph(package),
        }
    }

    /// Returns `combination` plus every feature it transitively implies.
    ///
    /// Every feature in `combination` must be declared by the package the implications were
    /// built from.
    pub(crate) fn closure(
        &self,
        combination: impl IntoIterator<Item = &'a String>,
    ) -> BTreeSet<&'a str> {
        resolved_set(combination, &self.graph)
    }
}

/// Apply pruning when the caller already resolved features and pruning flags.
#[must_use]
pub fn prune_with_resolved_features<'a>(
    combos: Vec<Vec<&'a String>>,
    package: &'a cargo_metadata::Package,
    config: &crate::config::ResolvedFeatures,
    no_prune: bool,
) -> PruneResult<'a> {
    let active = !no_prune && config.allow_feature_sets.is_empty();
    maybe_prune_active(combos, package, active)
}

fn maybe_prune_active<'a>(
    combos: Vec<Vec<&'a String>>,
    package: &'a cargo_metadata::Package,
    active: bool,
) -> PruneResult<'a> {
    if active {
        prune_implied_combinations(combos, package)
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
    package: &'a cargo_metadata::Package,
) -> PruneResult<'a> {
    let graph = build_implication_graph(package);

    // Group combos by their resolved feature set — the full set of features
    // that Cargo would enable after applying all transitive implications.
    // Combos in the same group produce identical compiled output.
    let mut groups: BTreeMap<BTreeSet<&'a str>, Vec<Vec<&'a String>>> = BTreeMap::new();
    for combo in combos {
        let resolved = resolved_set(combo.iter().copied(), &graph);
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
        use crate::config::{Config, FlagConfig, ResolvedFeatures};
        use crate::package::Package;

        let mut pkg = crate::package::test::package_with_features(&[])?;
        pkg.features = feat.clone();

        let mut config = Config::default();
        config.base.settings.flags = FlagConfig {
            prune_implied: Some(true),
            ..FlagConfig::default()
        };
        let resolved = ResolvedFeatures::from_config(&config);
        let combos = pkg.feature_combinations(&resolved)?;
        let result = prune_implied_combinations(combos, &pkg);

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
    fn maybe_prune_uses_resolved_no_prune_flag_only() -> eyre::Result<()> {
        use crate::config::{Config, FlagConfig, ResolvedFeatures};
        use crate::package::Package;

        let mut pkg = crate::package::test::package_with_features(&[])?;
        pkg.features = features(&[("A", &[]), ("B", &["A"])]);
        let mut config = Config::default();
        config.base.settings.flags = FlagConfig {
            prune_implied: Some(false),
            ..FlagConfig::default()
        };
        let resolved = ResolvedFeatures::from_config(&config);
        let combos = pkg.feature_combinations(&resolved)?;
        let result = prune_with_resolved_features(combos, &pkg, &resolved, false);

        sim_assert_eq!(result.keep.len(), 3);
        sim_assert_eq!(result.pruned.len(), 1);
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
    fn resolved_set_correctness() -> eyre::Result<()> {
        let package = package_with(features(&[("A", &[]), ("B", &["A"]), ("C", &["B"])]), &[])?;
        let graph = build_implication_graph(&package);

        let a = "A".to_string();
        let b = "B".to_string();
        let c = "C".to_string();

        sim_assert_eq!(resolved_set([], &graph), BTreeSet::<&str>::new());
        sim_assert_eq!(resolved_set([&a], &graph), BTreeSet::from(["A"]));
        sim_assert_eq!(resolved_set([&b], &graph), BTreeSet::from(["A", "B"]));
        sim_assert_eq!(resolved_set([&c], &graph), BTreeSet::from(["A", "B", "C"]));
        sim_assert_eq!(resolved_set([&a, &b], &graph), BTreeSet::from(["A", "B"]));
        Ok(())
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
    fn empty_features_no_pruning() -> eyre::Result<()> {
        let package = package_with(BTreeMap::new(), &[])?;
        let result = prune_implied_combinations(Vec::new(), &package);
        sim_assert_eq!(result.keep.len(), 0);
        sim_assert_eq!(result.pruned.len(), 0);
        Ok(())
    }

    /// A dependency named `name` in the manifest, renamed when `package` differs from it, and
    /// declared under `[target.'<target>'.dependencies]` when `target` is set.
    fn dependency(
        name: &str,
        package: &str,
        optional: bool,
        target: Option<&str>,
    ) -> eyre::Result<cargo_metadata::Dependency> {
        Ok(serde_json::from_value(serde_json::json!({
            "name": package,
            "rename": (name != package).then_some(name),
            "optional": optional,
            "source": null,
            "req": "*",
            "kind": null,
            "uses_default_features": true,
            "features": [],
            "target": target,
            "registry": null,
            "path": null,
        }))?)
    }

    fn package_with(
        features: BTreeMap<String, Vec<String>>,
        dependencies: &[cargo_metadata::Dependency],
    ) -> eyre::Result<cargo_metadata::Package> {
        let mut package = crate::package::test::package_with_features(&[])?;
        package.features = features;
        package.dependencies = dependencies.to_vec();
        Ok(package)
    }

    /// The features `tls` enables, given a package with `features` and `dependencies`.
    fn enabled_by_tls(
        features_table: &[(&str, &[&str])],
        dependencies: &[cargo_metadata::Dependency],
    ) -> eyre::Result<BTreeSet<String>> {
        let package = package_with(features(features_table), dependencies)?;
        let tls = "tls".to_string();
        Ok(FeatureImplications::new(&package)
            .closure([&tls])
            .into_iter()
            .map(String::from)
            .collect())
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().copied().map(String::from).collect()
    }

    // The cases below mirror what `cargo tree -e features` reports Cargo enabling for
    // `--no-default-features --features tls`.

    #[test]
    fn dependency_feature_enables_the_implicit_optional_dependency_feature() -> eyre::Result<()> {
        let enabled = enabled_by_tls(
            &[("foo", &["dep:foo"]), ("tls", &["foo/bar"])],
            &[dependency("foo", "foo", true, None)?],
        )?;
        sim_assert_eq!(enabled, names(&["foo", "tls"]));
        Ok(())
    }

    #[test]
    fn dependency_feature_enables_an_explicit_feature_named_after_the_dependency()
    -> eyre::Result<()> {
        // The explicit `foo` feature hides the implicit one but is enabled the same way, and its
        // own implications follow.
        let enabled = enabled_by_tls(
            &[
                ("foo", &["dep:foo", "extra"]),
                ("extra", &[]),
                ("tls", &["foo/bar"]),
            ],
            &[dependency("foo", "foo", true, None)?],
        )?;
        sim_assert_eq!(enabled, names(&["extra", "foo", "tls"]));
        Ok(())
    }

    #[test]
    fn dependency_feature_uses_the_renamed_dependency_name() -> eyre::Result<()> {
        let enabled = enabled_by_tls(
            &[("baz", &["dep:baz"]), ("tls", &["baz/bar"])],
            &[dependency("baz", "foo", true, None)?],
        )?;
        sim_assert_eq!(enabled, names(&["baz", "tls"]));
        Ok(())
    }

    #[test]
    fn target_specific_optional_dependency_forms_no_edge() -> eyre::Result<()> {
        // Cargo enables `foo` here only when building for Windows, so the graph, which serves
        // every target, must not assume it.
        let enabled = enabled_by_tls(
            &[("foo", &["dep:foo"]), ("tls", &["foo/bar"])],
            &[dependency("foo", "foo", true, Some("cfg(windows)"))?],
        )?;
        sim_assert_eq!(enabled, names(&["tls"]));
        Ok(())
    }

    #[test]
    fn untargeted_listing_forms_the_edge_beside_a_target_specific_one() -> eyre::Result<()> {
        // The untargeted listing is active on every target, whatever the other listing says.
        let enabled = enabled_by_tls(
            &[("foo", &["dep:foo"]), ("tls", &["foo/bar"])],
            &[
                dependency("foo", "foo", true, Some("cfg(windows)"))?,
                dependency("foo", "foo", true, None)?,
            ],
        )?;
        sim_assert_eq!(enabled, names(&["foo", "tls"]));
        Ok(())
    }

    #[test]
    fn weak_dependency_feature_enables_no_feature() -> eyre::Result<()> {
        let enabled = enabled_by_tls(
            &[("foo", &["dep:foo"]), ("tls", &["foo?/bar"])],
            &[dependency("foo", "foo", true, None)?],
        )?;
        sim_assert_eq!(enabled, names(&["tls"]));
        Ok(())
    }

    #[test]
    fn dependency_feature_enables_nothing_without_a_feature_of_that_name() -> eyre::Result<()> {
        // `dep:foo` hides the implicit feature, and no explicit `foo` replaces it.
        let enabled = enabled_by_tls(
            &[("use-foo", &["dep:foo"]), ("tls", &["foo/bar"])],
            &[dependency("foo", "foo", true, None)?],
        )?;
        sim_assert_eq!(enabled, names(&["tls"]));
        Ok(())
    }

    #[test]
    fn non_optional_dependency_feature_leaves_a_same_named_feature_off() -> eyre::Result<()> {
        let enabled = enabled_by_tls(
            &[("foo", &[]), ("tls", &["foo/bar"])],
            &[dependency("foo", "foo", false, None)?],
        )?;
        sim_assert_eq!(enabled, names(&["tls"]));
        Ok(())
    }
}
