//! Build resolved execution plans from target plans.

use crate::cfg_eval::CfgEvaluator;
use crate::config::{FlagConfig, ResolvedCommandConfig, ResolvedFlags, WorkspaceConfig};
use crate::implication::PrunedCombination;
use crate::package::{Package, has_lib_target};
use crate::plan::targets::{TargetPlan, TargetPlans};
use crate::print_warning;
use crate::target::{EffectiveTarget, TargetSource, TargetTriple};
use color_eyre::eyre;
use std::collections::BTreeMap;

/// Inputs that affect cargo-fc flag resolution while building execution plans.
pub struct PlanBuildContext<'a> {
    /// Workspace configuration read from the root manifest.
    pub workspace_config: &'a WorkspaceConfig,
    /// Literal command token from the user before alias expansion.
    pub raw_command: Option<&'a str>,
    /// Command token after cargo alias expansion.
    pub resolved_command: Option<&'a str>,
    /// Explicit `--driver <bin>` override; overlaid last in driver resolution.
    pub cli_driver: Option<&'a str>,
    /// Whether broad diagnostics config is safe for the resolved command.
    pub default_diagnostics_allowed: bool,
    /// Whether these plans are for `cargo fc matrix`.
    pub matrix: bool,
}

/// A resolved, owned execution plan for one concrete target.
///
/// After [`build_execution_plans`], execution owns a deterministic sequence of
/// resolved `(package, target, combinations, pruned)` units and needs neither a
/// [`CfgEvaluator`] nor package configs.
pub struct ExecutionPlan<'a> {
    /// The concrete target triple this plan is for.
    pub target: TargetTriple,
    /// The per-package execution plans for this target, in plan order.
    pub package_plans: Vec<PackageExecutionPlan<'a>>,
}

/// A resolved, owned execution plan for one package on one target.
pub struct PackageExecutionPlan<'a> {
    /// The package.
    pub package: &'a cargo_metadata::Package,
    /// The concrete target and where it came from.
    pub target: EffectiveTarget,
    /// Feature combinations to execute, in deterministic (sorted) order.
    pub combinations: Vec<Vec<String>>,
    /// Combinations pruned as implied by another combination.
    pub pruned: Vec<PrunedCombination>,
    /// Resolved user matrix metadata for this package-target (matrix output
    /// only; the executor ignores it).
    pub matrix: serde_json::Map<String, serde_json::Value>,
    /// Fully resolved cargo-fc flags for this package-target.
    pub flags: ResolvedFlags,
    /// Build driver resolved from config + `--driver` for this
    /// (package × target × command), before cargo-fc's cross-target default is
    /// applied. `None` means "unset"; `Some("cargo")` means explicit plain
    /// cargo. Finalized to the spawned program by [`crate`]'s driver pass.
    pub driver: Option<String>,
    /// Whether broad config requested diagnostics mode but was ignored because
    /// this command is not diagnostics-safe by default.
    pub ignored_diagnostics_config: bool,
}

/// The full set of execution plans plus display flags.
pub struct ExecutionPlanSet<'a> {
    /// Execution plans in deterministic target-plan order.
    pub plans: Vec<ExecutionPlan<'a>>,
    /// Whether pruned combinations should be shown in the summary.
    pub show_pruned: bool,
    /// Whether the `target = ...` field should be shown (not the implicit
    /// single-host default).
    pub show_target: bool,
}

/// Build owned execution plans from target plans and command context.
///
/// Resolves each package assignment's target-specific config from the cached
/// `PlannedPackage::config` (never re-reading the manifest), generates and
/// prunes feature combinations, and stores owned feature strings so execution
/// borrows nothing from temporary configs.
///
/// When resolved `packages_only` is set, feature generation is skipped (matrix
/// output only needs one `"default"` row per package-target).
///
/// # Errors
///
/// Returns an error if a package's config can not be resolved or its feature
/// combinations can not be generated.
pub fn build_execution_plans<'a>(
    target_plans: &TargetPlans<'a>,
    cli_flags: FlagConfig,
    context: &PlanBuildContext<'_>,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<ExecutionPlanSet<'a>> {
    let mut plans = Vec::with_capacity(target_plans.plans.len());
    let mut config_show_pruned = false;
    let mut config_show_target = false;
    let mut package_status = BTreeMap::new();

    for target_plan in &target_plans.plans {
        let mut package_plans = Vec::with_capacity(target_plan.packages.len());
        // Command-aware workspace package exclusion for this target and command.
        let excluded = crate::plan::targets::resolve_effective_exclude_packages(
            &target_plans.base_exclude,
            context.workspace_config,
            target_plan.workspace_target_exclude_ops.as_ref(),
            target_plan.workspace_target_replace,
            &target_plan.workspace_target_subcommands,
            context.raw_command,
            context.resolved_command,
        );
        for planned in &target_plan.packages {
            if excluded.contains(planned.package.name.as_str()) {
                continue;
            }
            let resolved_config = crate::config::resolve::resolve_config_with_flag_layers(
                planned.config,
                &target_plan.target,
                evaluator,
                context.raw_command,
                context.resolved_command,
            )?;
            let config = resolved_config.config;
            let command_config = resolve_package_command_config(
                &resolved_config.flag_layers,
                target_plan,
                cli_flags,
                context,
            )?;
            let flags = command_config.flags;
            let status = package_status
                .entry(planned.package.id.repr.clone())
                .or_insert_with(|| PackagePlanningStatus {
                    name: planned.package.name.to_string(),
                    ..PackagePlanningStatus::default()
                });

            let target_selection_skipped = is_configured_target_source(planned.target.source)
                && (flags.no_targets || !command_config.targets_enabled);
            if target_selection_skipped {
                status.target_selection_skipped = true;
                continue;
            }

            if flags.only_packages_with_lib_target && !has_lib_target(planned.package) {
                continue;
            }

            config_show_pruned = config_show_pruned || flags.show_pruned;
            config_show_target = config_show_target || planned.show_target;
            let packages_only = context.matrix && flags.packages_only;

            let (combinations, pruned) = if packages_only {
                (Vec::new(), Vec::new())
            } else {
                let all_combos = planned.package.feature_combinations(&config)?;
                let prune_result = crate::implication::maybe_prune_with_resolved_flag(
                    all_combos,
                    &planned.package.features,
                    &config,
                    flags.no_prune_implied,
                );
                // Own the feature strings before the resolved config is dropped.
                let combinations: Vec<Vec<String>> = prune_result
                    .keep
                    .into_iter()
                    .map(|combo| combo.into_iter().cloned().collect())
                    .collect();
                (combinations, prune_result.pruned)
            };

            package_plans.push(PackageExecutionPlan {
                package: planned.package,
                target: planned.target.clone(),
                combinations,
                pruned,
                matrix: config.matrix,
                flags,
                driver: command_config.driver,
                ignored_diagnostics_config: command_config.ignored_diagnostics_config,
            });
            status.kept = true;
        }
        if !package_plans.is_empty() {
            plans.push(ExecutionPlan {
                target: target_plan.target.clone(),
                package_plans,
            });
        }
    }

    warn_packages_skipped_by_target_selection(&package_status);

    let show_pruned = config_show_pruned;
    let show_target = config_show_target || plans.len() > 1;

    Ok(ExecutionPlanSet {
        plans,
        show_pruned,
        show_target,
    })
}

#[derive(Default)]
struct PackagePlanningStatus {
    name: String,
    kept: bool,
    target_selection_skipped: bool,
}

fn warn_packages_skipped_by_target_selection(status: &BTreeMap<String, PackagePlanningStatus>) {
    for entry in status.values() {
        if entry.target_selection_skipped && !entry.kept {
            print_warning!(
                "not running package `{}` because target-scoped `no_targets` or `subcommands.<name>.expand_targets = false` disabled all configured target assignments",
                entry.name
            );
        }
    }
}

fn is_configured_target_source(source: TargetSource) -> bool {
    matches!(
        source,
        TargetSource::WorkspaceConfig | TargetSource::PackageConfig
    )
}

fn resolve_package_command_config(
    package_layers: &crate::config::resolve::PackageFlagLayers,
    target_plan: &TargetPlan<'_>,
    cli_flags: FlagConfig,
    context: &PlanBuildContext<'_>,
) -> eyre::Result<ResolvedCommandConfig> {
    crate::config::resolve_command_config(crate::config::ResolveCommandConfigArgs {
        workspace: context.workspace_config,
        workspace_target_flags: target_plan.workspace_target_flags,
        workspace_target_replace: target_plan.workspace_target_replace,
        workspace_target_driver: target_plan.workspace_target_driver.as_deref(),
        workspace_target_subcommands: &target_plan.workspace_target_subcommands,
        package_flags: package_layers.package_flags,
        package_replace: package_layers.package_replace,
        package_driver: package_layers.package_driver.as_deref(),
        package_subcommands: &package_layers.package_subcommands,
        package_target_flags: package_layers.target_flags,
        package_target_replace: package_layers.target_replace,
        package_target_driver: package_layers.target_driver.as_deref(),
        package_target_subcommands: &package_layers.target_subcommands,
        raw_command: context.raw_command,
        resolved_command: context.resolved_command,
        cli_flags,
        cli_driver: context.cli_driver,
        default_diagnostics_allowed: context.default_diagnostics_allowed,
        // Target planning already decided which configured assignments may
        // exist. Execution-plan resolution only lets narrower target-scoped
        // config remove those assignments.
        default_targets_enabled: true,
    })
}

#[cfg(test)]
mod test {
    use super::{ExecutionPlanSet, PlanBuildContext, build_execution_plans};
    use crate::cfg_eval::CfgEvaluator;
    use crate::config::{FlagConfig, TargetOverride};
    use crate::package::Package as _;
    use crate::package::test::{effective_target, package};
    use crate::plan::targets::{PlannedPackage, TargetPlan, TargetPlans};
    use crate::target::{EffectiveTarget, TargetSource, TargetTriple};
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct StubEval;

    impl CfgEvaluator for StubEval {
        fn matches(&mut self, _cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            Ok(false)
        }
    }

    struct MatchAllEval;

    impl CfgEvaluator for MatchAllEval {
        fn matches(&mut self, _cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            Ok(true)
        }
    }

    fn string_vec(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(String::from).collect()
    }

    fn test_context(workspace_config: &crate::config::WorkspaceConfig) -> PlanBuildContext<'_> {
        PlanBuildContext {
            workspace_config,
            raw_command: None,
            resolved_command: None,
            cli_driver: None,
            default_diagnostics_allowed: false,
            matrix: false,
        }
    }

    #[test]
    fn target_scoped_no_targets_skips_configured_assignments() -> eyre::Result<()> {
        let package = package("a")?;
        let config = crate::config::Config::default();
        let target_plans = TargetPlans {
            plans: vec![TargetPlan {
                target: TargetTriple("configured-target".to_string()),
                workspace_target_flags: FlagConfig {
                    no_targets: Some(true),
                    ..FlagConfig::default()
                },
                workspace_target_replace: false,
                workspace_target_driver: None,
                workspace_target_exclude_ops: None,
                workspace_target_subcommands: BTreeMap::new(),
                packages: vec![PlannedPackage {
                    package: &package,
                    config: &config,
                    target: effective_target("configured-target"),
                    show_target: true,
                }],
            }],
            base_exclude: std::collections::HashSet::new(),
            contains_configured_assignments: true,
        };
        let mut evaluator = StubEval;

        let workspace_config = crate::config::WorkspaceConfig::default();
        let context = test_context(&workspace_config);
        let plan_set = build_execution_plans(
            &target_plans,
            FlagConfig::default(),
            &context,
            &mut evaluator,
        )?;

        assert!(plan_set.plans.is_empty());
        Ok(())
    }

    #[test]
    fn target_scoped_lib_filter_false_can_reinclude_package() -> eyre::Result<()> {
        let mut package = package("bin-only")?;
        package.targets.clear();
        let mut config = crate::config::Config::default();
        config.target_overrides.insert(
            "cfg(any())".to_string(),
            TargetOverride {
                flags: FlagConfig {
                    only_packages_with_lib_target: Some(false),
                    ..FlagConfig::default()
                },
                ..TargetOverride::default()
            },
        );
        let target_plans = TargetPlans {
            plans: vec![TargetPlan {
                target: TargetTriple("configured-target".to_string()),
                workspace_target_flags: FlagConfig {
                    only_packages_with_lib_target: Some(true),
                    ..FlagConfig::default()
                },
                workspace_target_replace: false,
                workspace_target_driver: None,
                workspace_target_exclude_ops: None,
                workspace_target_subcommands: BTreeMap::new(),
                packages: vec![PlannedPackage {
                    package: &package,
                    config: &config,
                    target: effective_target("configured-target"),
                    show_target: true,
                }],
            }],
            base_exclude: std::collections::HashSet::new(),
            contains_configured_assignments: true,
        };

        let workspace_config = crate::config::WorkspaceConfig::default();
        let context = test_context(&workspace_config);
        let mut evaluator = MatchAllEval;
        let plan_set = build_execution_plans(
            &target_plans,
            FlagConfig::default(),
            &context,
            &mut evaluator,
        )?;

        let [plan] = plan_set.plans.as_slice() else {
            eyre::bail!("expected one execution plan, got {}", plan_set.plans.len());
        };
        assert_eq!(plan.package_plans.len(), 1);
        Ok(())
    }

    #[test]
    fn build_execution_plans_keeps_pruned_entries_for_summary() -> eyre::Result<()> {
        let mut package = crate::package::test::package_with_metadata(
            &["A", "B", "C"],
            "cargo-fc",
            &serde_json::json!({ "show_pruned": true }),
        )?;
        let Some(implied_features) = package.features.get_mut("B") else {
            eyre::bail!("test package should contain feature B");
        };
        implied_features.push("A".to_string());

        let config = package.config()?;
        let target_plans = TargetPlans {
            plans: vec![TargetPlan {
                target: TargetTriple("test-target".to_string()),
                workspace_target_flags: crate::config::FlagConfig::default(),
                workspace_target_replace: false,
                workspace_target_driver: None,
                workspace_target_exclude_ops: None,
                workspace_target_subcommands: BTreeMap::new(),
                packages: vec![PlannedPackage {
                    package: &package,
                    config: &config,
                    target: EffectiveTarget {
                        triple: TargetTriple("test-target".to_string()),
                        source: TargetSource::Cli,
                    },
                    show_target: true,
                }],
            }],
            base_exclude: std::collections::HashSet::new(),
            contains_configured_assignments: false,
        };

        let mut evaluator = StubEval;
        let workspace_config = crate::config::WorkspaceConfig::default();
        let context = test_context(&workspace_config);
        let plan_set = build_execution_plans(
            &target_plans,
            FlagConfig::default(),
            &context,
            &mut evaluator,
        )?;

        assert_pruned_plan(&plan_set)?;
        Ok(())
    }

    fn assert_pruned_plan(plan_set: &ExecutionPlanSet<'_>) -> eyre::Result<()> {
        assert!(plan_set.show_pruned);
        let [plan] = plan_set.plans.as_slice() else {
            eyre::bail!("expected one execution plan, got {}", plan_set.plans.len());
        };
        let [pp] = plan.package_plans.as_slice() else {
            eyre::bail!(
                "expected one package execution plan, got {}",
                plan.package_plans.len()
            );
        };
        sim_assert_eq!(
            &pp.combinations,
            &vec![
                string_vec(&[]),
                string_vec(&["A"]),
                string_vec(&["A", "C"]),
                string_vec(&["B"]),
                string_vec(&["B", "C"]),
                string_vec(&["C"]),
            ],
        );

        let pruned: Vec<_> = pp
            .pruned
            .iter()
            .map(|p| (p.features.clone(), p.equivalent_to.clone()))
            .collect();
        sim_assert_eq!(
            pruned,
            vec![
                (string_vec(&["A", "B"]), string_vec(&["B"])),
                (string_vec(&["A", "B", "C"]), string_vec(&["B", "C"])),
            ],
        );
        Ok(())
    }
}
