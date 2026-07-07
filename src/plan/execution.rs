//! Build resolved execution plans from target plans.

use crate::cfg_eval::CfgEvaluator;
use crate::config::{Chain, FlagConfig, ResolvedFlags, WorkspaceConfig};
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
        let excluded = Chain::workspace(
            context.workspace_config,
            &target_plan.ws_matched,
            context.raw_command,
            context.resolved_command,
        )
        .exclude_packages(&target_plans.base_exclude)?;
        for planned in &target_plan.packages {
            if excluded.contains(planned.package.name.as_str()) {
                continue;
            }
            let status = package_status
                .entry(planned.package.id.repr.clone())
                .or_insert_with(|| PackagePlanningStatus {
                    name: planned.package.name.to_string(),
                    ..PackagePlanningStatus::default()
                });

            let package_plan = match resolve_package_execution_plan(
                target_plan,
                planned,
                cli_flags,
                context,
                evaluator,
            )? {
                PackagePlanOutcome::Kept(package_plan) => package_plan,
                PackagePlanOutcome::TargetSelectionSkipped => {
                    status.target_selection_skipped = true;
                    continue;
                }
                PackagePlanOutcome::Filtered => continue,
            };
            config_show_pruned = config_show_pruned || package_plan.flags.show_pruned;
            config_show_target = config_show_target || planned.show_target;
            package_plans.push(package_plan);
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

fn resolve_package_execution_plan<'a>(
    target_plan: &TargetPlan<'a>,
    planned: &crate::plan::targets::PlannedPackage<'a>,
    cli_flags: FlagConfig,
    context: &PlanBuildContext<'_>,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<PackagePlanOutcome<'a>> {
    let pkg_matched = crate::config::resolve::matching_overrides(
        &planned.config.targets,
        &target_plan.target,
        evaluator,
    )?;
    let resolved = Chain::full(
        context.workspace_config,
        &target_plan.ws_matched,
        planned.config,
        pkg_matched,
        context.raw_command,
        context.resolved_command,
    )
    .resolve(
        crate::config::resolve::CliOverlay {
            flags: cli_flags,
            driver: context.cli_driver,
        },
        crate::config::resolve::ResolvePolicy {
            default_diagnostics_allowed: context.default_diagnostics_allowed,
            // Target planning already decided which configured assignments may
            // exist. Execution-plan resolution only lets narrower target-scoped
            // config remove those assignments.
            default_targets_enabled: true,
        },
    )?;
    let flags = resolved.flags;

    let target_selection_skipped = is_configured_target_source(planned.target.source)
        && (flags.no_targets || !resolved.targets_enabled);
    let lib_filter_skipped =
        flags.only_packages_with_lib_target && !has_lib_target(planned.package);
    if target_selection_skipped {
        return Ok(PackagePlanOutcome::TargetSelectionSkipped);
    }
    if lib_filter_skipped {
        return Ok(PackagePlanOutcome::Filtered);
    }

    let packages_only = context.matrix && flags.packages_only;
    let (combinations, pruned) = if packages_only {
        (Vec::new(), Vec::new())
    } else {
        let all_combos = planned.package.feature_combinations(&resolved.features)?;
        let prune_result = crate::implication::maybe_prune_with_resolved_flag(
            all_combos,
            &planned.package.features,
            &resolved.features,
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

    Ok(PackagePlanOutcome::Kept(PackageExecutionPlan {
        package: planned.package,
        target: planned.target.clone(),
        combinations,
        pruned,
        matrix: resolved.features.matrix,
        flags,
        driver: resolved.driver,
        ignored_diagnostics_config: resolved.ignored_diagnostics_config,
    }))
}

#[derive(Default)]
struct PackagePlanningStatus {
    name: String,
    kept: bool,
    target_selection_skipped: bool,
}

enum PackagePlanOutcome<'a> {
    Kept(PackageExecutionPlan<'a>),
    TargetSelectionSkipped,
    Filtered,
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

#[cfg(test)]
mod test {
    use super::{ExecutionPlanSet, PlanBuildContext, build_execution_plans};
    use crate::cfg_eval::CfgEvaluator;
    use crate::config::{Config, FlagConfig, ScopeConfig, TargetOverride, WorkspaceTargetOverride};
    use crate::package::Package as _;
    use crate::package::test::{effective_target, package};
    use crate::plan::targets::{PlannedPackage, TargetPlan, TargetPlans};
    use crate::target::{EffectiveTarget, TargetSource, TargetTriple};
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

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
        let ws_override = WorkspaceTargetOverride {
            settings: ScopeConfig {
                flags: FlagConfig {
                    no_targets: Some(true),
                    ..FlagConfig::default()
                },
                ..ScopeConfig::default()
            },
            ..WorkspaceTargetOverride::default()
        };
        let target_plans = TargetPlans {
            plans: vec![TargetPlan {
                target: TargetTriple("configured-target".to_string()),
                ws_matched: vec![("cfg(any())".to_string(), &ws_override)],
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
        config.targets.insert(
            "cfg(any())".to_string(),
            TargetOverride {
                settings: ScopeConfig {
                    flags: FlagConfig {
                        only_packages_with_lib_target: Some(false),
                        ..FlagConfig::default()
                    },
                    ..ScopeConfig::default()
                },
                ..TargetOverride::default()
            },
        );
        let ws_override = WorkspaceTargetOverride {
            settings: ScopeConfig {
                flags: FlagConfig {
                    only_packages_with_lib_target: Some(true),
                    ..FlagConfig::default()
                },
                ..ScopeConfig::default()
            },
            ..WorkspaceTargetOverride::default()
        };
        let target_plans = TargetPlans {
            plans: vec![TargetPlan {
                target: TargetTriple("configured-target".to_string()),
                ws_matched: vec![("cfg(any())".to_string(), &ws_override)],
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
    fn package_replace_does_not_clear_workspace_exclude_packages() -> eyre::Result<()> {
        let package = package("drop")?;
        let mut config = Config::default();
        config.base.settings.replace = true;
        let target_plans = TargetPlans {
            plans: vec![TargetPlan {
                target: TargetTriple("configured-target".to_string()),
                ws_matched: Vec::new(),
                packages: vec![PlannedPackage {
                    package: &package,
                    config: &config,
                    target: effective_target("configured-target"),
                    show_target: true,
                }],
            }],
            base_exclude: std::collections::HashSet::from(["drop".to_string()]),
            contains_configured_assignments: true,
        };

        let workspace_config = crate::config::WorkspaceConfig::default();
        let context = test_context(&workspace_config);
        let mut evaluator = StubEval;
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
                ws_matched: Vec::new(),
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
