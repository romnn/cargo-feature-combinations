//! Collapse execution plans to their maximal feature sets.
//!
//! Backs the `maximal_features` flag (`--maximal-features` on the CLI, or any
//! config scope such as `[workspace.metadata.cargo-fc.subcommands.udeps]`):
//! keep only combinations that are not a subset of another generated
//! combination, so commands that need broad feature reachability (for example
//! `cargo udeps`) run once per package-target instead of once per feature
//! interaction, without losing matrix constraints.

use super::execution::ExecutionPlanSet;
use crate::implication::PrunedCombination;
use std::collections::BTreeMap;

/// Collapse each package-target matrix with a resolved `maximal_features`
/// flag to its maximal feature sets.
///
/// The flag follows the normal scope chain (workspace, package, target, and
/// `subcommands` tables, with `--maximal-features` overlaid last), so one
/// package can collapse while another keeps its full matrix.
///
/// A purely additive matrix becomes one row whose feature set is the union of
/// all features. Matrix constraints remain authoritative: mutually exclusive
/// features, `exclude_feature_sets`, isolated feature sets, and exact
/// allowlists can leave multiple maximal rows. Target overrides are resolved
/// before this pass, so platform-incompatible features never cross target
/// boundaries here.
///
/// Pruned (implied-equivalent) combinations rejoin the search so a union row
/// that was pruned still covers its subsets. `show_pruned` stays enabled only
/// while some package-target still has pruned rows left to display.
pub fn maybe_retain_maximal_feature_sets(plan_set: &mut ExecutionPlanSet<'_>) {
    let mut collapsed_any = false;
    for plan in &mut plan_set.plans {
        for package_plan in &mut plan.package_plans {
            if !package_plan.flags.maximal_features {
                continue;
            }
            collapsed_any = true;
            let pruned = std::mem::take(&mut package_plan.pruned);
            package_plan.combinations =
                maximal_feature_sets(std::mem::take(&mut package_plan.combinations), pruned);
        }
    }
    // Only a collapse consumes pruned rows; without one, `show_pruned` keeps
    // its resolved value even when no pruned rows happen to exist.
    if collapsed_any {
        plan_set.show_pruned = plan_set.show_pruned
            && plan_set.plans.iter().any(|plan| {
                plan.package_plans
                    .iter()
                    .any(|package_plan| !package_plan.pruned.is_empty())
            });
    }
}

/// Keep only combinations that are not a strict subset of another combination.
///
/// Pruned combinations re-enter the search because a pruned union (for example
/// `[base, extended]` when `extended` implies `base`) may be the true maximal
/// set. When such a row survives, its kept representative (`[extended]`) is
/// returned instead, so execution runs the canonical spelling.
fn maximal_feature_sets(
    mut combinations: Vec<Vec<String>>,
    pruned: Vec<PrunedCombination>,
) -> Vec<Vec<String>> {
    let mut representatives = BTreeMap::new();
    for redundant in pruned {
        combinations.push(redundant.features.clone());
        representatives.insert(redundant.features, redundant.equivalent_to);
    }

    // Combination generation emits each set as a sorted vector; the binary
    // searches below rely on that order.
    debug_assert!(
        combinations
            .iter()
            .all(|combination| combination.is_sorted()),
        "feature combinations must be sorted"
    );

    // Visit larger sets first, so each combination only needs to compare
    // against already-retained, strictly larger sets.
    combinations.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    combinations.dedup();

    let mut maximal: Vec<Vec<String>> = Vec::new();
    for combination in combinations {
        let is_covered = maximal
            .iter()
            .take_while(|candidate| candidate.len() > combination.len())
            .any(|candidate| {
                combination
                    .iter()
                    .all(|feature| candidate.binary_search(feature).is_ok())
            });
        if !is_covered {
            maximal.push(combination);
        }
    }
    // Swap surviving pruned rows for their canonical representatives. Distinct
    // maximal rows can share one representative, hence the final dedup.
    let mut maximal = maximal
        .into_iter()
        .map(|combination| representatives.remove(&combination).unwrap_or(combination))
        .collect::<Vec<_>>();
    maximal.sort();
    maximal.dedup();
    maximal
}

#[cfg(test)]
mod test {
    use super::{maximal_feature_sets, maybe_retain_maximal_feature_sets};
    use crate::implication::PrunedCombination;
    use crate::package::test::{effective_target, package};
    use crate::plan::execution::{ExecutionPlan, ExecutionPlanSet, PackageExecutionPlan};
    use crate::target::TargetTriple;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn string_vec(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(String::from).collect()
    }

    fn package_plan(
        package: &cargo_metadata::Package,
        combinations: Vec<Vec<String>>,
    ) -> PackageExecutionPlan<'_> {
        PackageExecutionPlan {
            package,
            target: effective_target("test-target"),
            combinations,
            pruned: Vec::new(),
            matrix: serde_json::Map::new(),
            flags: crate::config::ResolvedFlags {
                maximal_features: true,
                ..crate::config::ResolvedFlags::default()
            },
            driver: None,
            env: crate::config::ResolvedEnv::default(),
            ignored_diagnostics_config: false,
        }
    }

    fn only_combinations<'plan>(
        plan_set: &'plan ExecutionPlanSet<'_>,
    ) -> eyre::Result<&'plan [Vec<String>]> {
        let [plan] = plan_set.plans.as_slice() else {
            eyre::bail!("expected one target plan, got {}", plan_set.plans.len());
        };
        let [package_plan] = plan.package_plans.as_slice() else {
            eyre::bail!(
                "expected one package plan, got {}",
                plan.package_plans.len()
            );
        };
        Ok(&package_plan.combinations)
    }

    #[test]
    fn maximal_features_collapses_an_additive_powerset_to_its_union() -> eyre::Result<()> {
        let package = package("additive")?;
        let mut plan_set = ExecutionPlanSet {
            plans: vec![ExecutionPlan {
                target: TargetTriple("test-target".to_string()),
                package_plans: vec![package_plan(
                    &package,
                    vec![
                        Vec::new(),
                        string_vec(&["a"]),
                        string_vec(&["b"]),
                        string_vec(&["a", "b"]),
                    ],
                )],
            }],
            show_pruned: true,
            show_target: false,
        };

        maybe_retain_maximal_feature_sets(&mut plan_set);

        sim_assert_eq!(only_combinations(&plan_set)?, [string_vec(&["a", "b"])]);
        assert!(!plan_set.show_pruned);
        Ok(())
    }

    #[test]
    fn maximal_features_keeps_incompatible_maximal_sets_separate() -> eyre::Result<()> {
        let package = package("alternatives")?;
        let mut plan_set = ExecutionPlanSet {
            plans: vec![ExecutionPlan {
                target: TargetTriple("test-target".to_string()),
                package_plans: vec![package_plan(
                    &package,
                    vec![
                        Vec::new(),
                        string_vec(&["cpu"]),
                        string_vec(&["cuda"]),
                        string_vec(&["logging"]),
                        string_vec(&["cpu", "logging"]),
                        string_vec(&["cuda", "logging"]),
                    ],
                )],
            }],
            show_pruned: false,
            show_target: false,
        };

        maybe_retain_maximal_feature_sets(&mut plan_set);

        sim_assert_eq!(
            only_combinations(&plan_set)?,
            [
                string_vec(&["cpu", "logging"]),
                string_vec(&["cuda", "logging"])
            ]
        );
        Ok(())
    }

    #[test]
    fn maximal_features_keep_one_row_for_each_of_five_alternatives() {
        let combinations = vec![
            Vec::new(),
            string_vec(&["shared"]),
            string_vec(&["backend-a"]),
            string_vec(&["backend-b"]),
            string_vec(&["backend-c"]),
            string_vec(&["backend-d"]),
            string_vec(&["backend-e"]),
            string_vec(&["backend-a", "shared"]),
            string_vec(&["backend-b", "shared"]),
            string_vec(&["backend-c", "shared"]),
            string_vec(&["backend-d", "shared"]),
            string_vec(&["backend-e", "shared"]),
        ];

        let maximal = maximal_feature_sets(combinations, Vec::new());

        sim_assert_eq!(
            maximal,
            vec![
                string_vec(&["backend-a", "shared"]),
                string_vec(&["backend-b", "shared"]),
                string_vec(&["backend-c", "shared"]),
                string_vec(&["backend-d", "shared"]),
                string_vec(&["backend-e", "shared"]),
            ]
        );
    }

    #[test]
    fn maximal_features_keep_shorter_rows_when_they_are_not_subsets() {
        let combinations = vec![
            Vec::new(),
            string_vec(&["a"]),
            string_vec(&["b"]),
            string_vec(&["c"]),
            string_vec(&["b", "c"]),
        ];

        let maximal = maximal_feature_sets(combinations, Vec::new());

        sim_assert_eq!(maximal, vec![string_vec(&["a"]), string_vec(&["b", "c"])]);
    }

    #[test]
    fn maximal_features_cross_independent_alternative_groups() {
        let combinations = vec![
            Vec::new(),
            string_vec(&["postgres"]),
            string_vec(&["sqlite"]),
            string_vec(&["tokio"]),
            string_vec(&["async-std"]),
            string_vec(&["tracing"]),
            string_vec(&["postgres", "tokio", "tracing"]),
            string_vec(&["async-std", "postgres", "tracing"]),
            string_vec(&["sqlite", "tokio", "tracing"]),
            string_vec(&["async-std", "sqlite", "tracing"]),
        ];

        let maximal = maximal_feature_sets(combinations, Vec::new());

        sim_assert_eq!(
            maximal,
            vec![
                string_vec(&["async-std", "postgres", "tracing"]),
                string_vec(&["async-std", "sqlite", "tracing"]),
                string_vec(&["postgres", "tokio", "tracing"]),
                string_vec(&["sqlite", "tokio", "tracing"]),
            ]
        );
    }

    /// The flag resolves per package-target: only flagged packages collapse,
    /// and `show_pruned` survives while an uncollapsed package still has
    /// pruned rows to display.
    #[test]
    fn maximal_features_apply_per_package_scope() -> eyre::Result<()> {
        let flagged = package("flagged")?;
        let unflagged = package("unflagged")?;
        let mut unflagged_plan = package_plan(
            &unflagged,
            vec![Vec::new(), string_vec(&["base"]), string_vec(&["extended"])],
        );
        unflagged_plan.flags.maximal_features = false;
        unflagged_plan.pruned = vec![PrunedCombination {
            features: string_vec(&["base", "extended"]),
            equivalent_to: string_vec(&["extended"]),
        }];
        let mut plan_set = ExecutionPlanSet {
            plans: vec![ExecutionPlan {
                target: TargetTriple("test-target".to_string()),
                package_plans: vec![
                    package_plan(
                        &flagged,
                        vec![Vec::new(), string_vec(&["a"]), string_vec(&["b"])],
                    ),
                    unflagged_plan,
                ],
            }],
            show_pruned: true,
            show_target: false,
        };

        maybe_retain_maximal_feature_sets(&mut plan_set);

        let [plan] = plan_set.plans.as_slice() else {
            eyre::bail!("expected one target plan, got {}", plan_set.plans.len());
        };
        let [flagged_plan, unflagged_plan] = plan.package_plans.as_slice() else {
            eyre::bail!(
                "expected two package plans, got {}",
                plan.package_plans.len()
            );
        };
        // The flagged package collapses; a and b are incompatible only if the
        // matrix says so, and here no [a, b] row exists, so both rows remain.
        sim_assert_eq!(
            flagged_plan.combinations,
            vec![string_vec(&["a"]), string_vec(&["b"])]
        );
        // The unflagged package keeps its full matrix and its pruned rows.
        sim_assert_eq!(
            unflagged_plan.combinations,
            vec![Vec::new(), string_vec(&["base"]), string_vec(&["extended"])]
        );
        sim_assert_eq!(unflagged_plan.pruned.len(), 1);
        assert!(plan_set.show_pruned);
        Ok(())
    }

    #[test]
    fn maximal_features_uses_the_canonical_implied_feature_representative() -> eyre::Result<()> {
        let package = package("implied")?;
        let mut package_plan = package_plan(
            &package,
            vec![Vec::new(), string_vec(&["base"]), string_vec(&["extended"])],
        );
        package_plan.pruned = vec![PrunedCombination {
            features: string_vec(&["base", "extended"]),
            equivalent_to: string_vec(&["extended"]),
        }];
        let mut plan_set = ExecutionPlanSet {
            plans: vec![ExecutionPlan {
                target: TargetTriple("test-target".to_string()),
                package_plans: vec![package_plan],
            }],
            show_pruned: true,
            show_target: false,
        };

        maybe_retain_maximal_feature_sets(&mut plan_set);

        sim_assert_eq!(only_combinations(&plan_set)?, [string_vec(&["extended"])]);
        Ok(())
    }
}
