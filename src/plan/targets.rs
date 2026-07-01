//! Target selection and target-plan construction.
//!
//! This is the outer execution axis: it decides which target triples each
//! selected package is visited for, deduplicated by triple for stable
//! scheduling and output, while every package-target assignment carries its
//! [`TargetSource`] so injection and output decisions stay local to the
//! assignment.
//!
//! Target *selection* (this module) is kept separate from target-specific
//! *config resolution* ([`crate::config::resolve`]): target lists choose the
//! outer axis; `target.'cfg(...)'` overrides shape the effective feature matrix
//! for one already-selected target.

use crate::cfg_eval::CfgEvaluator;
use crate::config::patch::combine_string_set_patches;
use crate::config::{
    CommandCapabilities, Config, FlagConfig, WorkspaceConfig, WorkspaceTargetOverride,
};
use crate::target::{EffectiveTarget, TargetEnvironment, TargetSource, TargetTriple};
use color_eyre::eyre::{self, WrapErr};
use std::collections::{BTreeMap, HashSet};

/// A package selected for processing together with its cached base config.
///
/// Configs are loaded once before planning so neither planning nor
/// execution-plan construction re-reads the manifest (which would duplicate
/// deprecation warnings).
pub struct SelectedPackage<'a> {
    /// The selected package.
    pub package: &'a cargo_metadata::Package,
    /// The cached base cargo-fc config for this package.
    pub config: &'a Config,
    /// Whether configured target lists should be ignored for this package.
    pub ignore_configured_targets: bool,
    /// Whether the target-selection decision came from an explicit cargo-fc
    /// flag or subcommand override rather than the built-in command default.
    pub target_decision_explicit: bool,
}

/// A package assigned to one concrete target.
pub struct PlannedPackage<'a> {
    /// The package.
    pub package: &'a cargo_metadata::Package,
    /// The cached base cargo-fc config for this package.
    pub config: &'a Config,
    /// The concrete target and where it came from.
    pub target: EffectiveTarget,
    /// Whether surviving output for this package-target should show target
    /// attribution.
    pub show_target: bool,
}

/// All package assignments for one concrete target triple.
pub struct TargetPlan<'a> {
    /// The concrete target triple this plan is for.
    pub target: TargetTriple,
    /// Workspace flags from matching workspace target overrides.
    pub workspace_target_flags: FlagConfig,
    /// Workspace subcommand overrides from matching workspace target overrides.
    pub workspace_target_subcommands: BTreeMap<String, CommandCapabilities>,
    /// The package assignments for this target, in selected-package order.
    pub packages: Vec<PlannedPackage<'a>>,
}

/// The full set of target plans for an invocation.
pub struct TargetPlans<'a> {
    /// Target plans in deterministic order (workspace target order, then
    /// package-only targets, then the fallback target).
    pub plans: Vec<TargetPlan<'a>>,
    /// Whether target selection was influenced by configured target metadata
    /// or an explicit `--target` (anything other than the implicit
    /// host/`CARGO_BUILD_TARGET` single-target fallback).
    ///
    /// Includes the package `targets = []` opt-out, even if the resulting
    /// concrete source is `Host`/`CargoBuildTargetEnv`. Execution-plan output
    /// display is decided from surviving [`PlannedPackage::show_target`]
    /// values, after package-target filters run.
    pub contains_configured_assignments: bool,
}

/// Trim, reject empty, and deduplicate a configured target list, preserving
/// first-occurrence order.
fn normalize_targets(raw: &[String]) -> eyre::Result<Vec<TargetTriple>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for triple in raw {
        let triple = triple.trim();
        if triple.is_empty() {
            eyre::bail!("empty target triple in configured `targets` list");
        }
        if seen.insert(triple.to_string()) {
            out.push(TargetTriple(triple.to_string()));
        }
    }
    Ok(out)
}

/// Resolve the single fallback target: `CARGO_BUILD_TARGET`, then host.
fn resolve_fallback(
    env: &impl TargetEnvironment,
    cache: &mut Option<EffectiveTarget>,
) -> eyre::Result<EffectiveTarget> {
    if let Some(target) = cache {
        return Ok(target.clone());
    }
    let target = if let Some(triple) = env.cargo_build_target() {
        EffectiveTarget {
            triple: TargetTriple(triple),
            source: TargetSource::CargoBuildTargetEnv,
        }
    } else {
        EffectiveTarget {
            triple: env.host_target()?,
            source: TargetSource::Host,
        }
    };
    *cache = Some(target.clone());
    Ok(target)
}

fn fallback_assignment(
    env: &impl TargetEnvironment,
    cache: &mut Option<EffectiveTarget>,
    show_target: bool,
) -> eyre::Result<PackageTargetAssignment> {
    Ok(PackageTargetAssignment {
        target: resolve_fallback(env, cache)?,
        show_target,
    })
}

/// Per-package resolved effective target list.
struct PackageTargets<'a> {
    package: &'a cargo_metadata::Package,
    config: &'a Config,
    targets: Vec<PackageTargetAssignment>,
}

/// One selected target for one package before workspace exclusions run.
#[derive(Clone)]
struct PackageTargetAssignment {
    target: EffectiveTarget,
    show_target: bool,
}

/// Resolve one selected package's effective target list using the configured
/// precedence (CLI handled by the caller as a global override).
fn package_target_list(
    selected: &SelectedPackage<'_>,
    workspace_targets: &[TargetTriple],
    env: &impl TargetEnvironment,
    fallback_cache: &mut Option<EffectiveTarget>,
) -> eyre::Result<Vec<PackageTargetAssignment>> {
    if selected.ignore_configured_targets {
        return Ok(vec![fallback_assignment(env, fallback_cache, false)?]);
    }

    match &selected.config.package_targets {
        // Package-level list present.
        Some(list) if !list.is_empty() => {
            let triples = normalize_targets(list)?;
            Ok(triples
                .into_iter()
                .map(|triple| EffectiveTarget {
                    triple,
                    source: TargetSource::PackageConfig,
                })
                .map(|target| PackageTargetAssignment {
                    target,
                    show_target: true,
                })
                .collect())
        }
        // Package-level opt-out (`targets = []`): use the fallback single target.
        Some(_) => Ok(vec![fallback_assignment(env, fallback_cache, true)?]),
        // No package-level list: inherit workspace targets, else fallback.
        None => {
            if workspace_targets.is_empty() {
                Ok(vec![fallback_assignment(env, fallback_cache, false)?])
            } else {
                Ok(workspace_targets
                    .iter()
                    .map(|triple| EffectiveTarget {
                        triple: triple.clone(),
                        source: TargetSource::WorkspaceConfig,
                    })
                    .map(|target| PackageTargetAssignment {
                        target,
                        show_target: true,
                    })
                    .collect())
            }
        }
    }
}

fn cli_package_targets<'a>(
    selected: &[SelectedPackage<'a>],
    cli: &str,
) -> eyre::Result<Vec<PackageTargets<'a>>> {
    let triple = cli.trim();
    if triple.is_empty() {
        eyre::bail!("empty `--target` value");
    }
    let cli_target = EffectiveTarget {
        triple: TargetTriple(triple.to_string()),
        source: TargetSource::Cli,
    };
    let cli_assignment = PackageTargetAssignment {
        target: cli_target,
        show_target: true,
    };
    Ok(selected
        .iter()
        .map(|s| PackageTargets {
            package: s.package,
            config: s.config,
            targets: vec![cli_assignment.clone()],
        })
        .collect())
}

fn configured_package_targets<'a>(
    selected: &[SelectedPackage<'a>],
    workspace_targets: &[TargetTriple],
    env: &impl TargetEnvironment,
    fallback_cache: &mut Option<EffectiveTarget>,
) -> eyre::Result<Vec<PackageTargets<'a>>> {
    let mut out = Vec::with_capacity(selected.len());
    for s in selected {
        let targets = package_target_list(s, workspace_targets, env, fallback_cache)?;
        out.push(PackageTargets {
            package: s.package,
            config: s.config,
            targets,
        });
    }
    Ok(out)
}

fn fallback_package_targets<'a>(
    selected: &[SelectedPackage<'a>],
    env: &impl TargetEnvironment,
    fallback_cache: &mut Option<EffectiveTarget>,
) -> eyre::Result<Vec<PackageTargets<'a>>> {
    let fallback = resolve_fallback(env, fallback_cache)?;
    let fallback_assignment = PackageTargetAssignment {
        target: fallback,
        show_target: false,
    };
    Ok(selected
        .iter()
        .map(|s| PackageTargets {
            package: s.package,
            config: s.config,
            targets: vec![fallback_assignment.clone()],
        })
        .collect())
}

fn target_order(
    workspace_targets: &[TargetTriple],
    package_targets: &[PackageTargets<'_>],
) -> Vec<TargetTriple> {
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    let used: HashSet<TargetTriple> = package_targets
        .iter()
        .flat_map(|pt| {
            pt.targets
                .iter()
                .map(|assignment| assignment.target.triple.clone())
        })
        .collect();

    for triple in workspace_targets {
        if used.contains(triple) && seen.insert(triple.clone()) {
            order.push(triple.clone());
        }
    }
    for pt in package_targets {
        for assignment in &pt.targets {
            if seen.insert(assignment.target.triple.clone()) {
                order.push(assignment.target.triple.clone());
            }
        }
    }

    order
}

/// Resolve the effective workspace target override state for one target.
struct WorkspaceTargetConfig {
    exclude_packages: HashSet<String>,
    flags: FlagConfig,
    subcommands: BTreeMap<String, CommandCapabilities>,
}

fn resolve_workspace_target_config(
    base: &HashSet<String>,
    overrides: &BTreeMap<String, WorkspaceTargetOverride>,
    triple: &TargetTriple,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<WorkspaceTargetConfig> {
    let mut matched = Vec::new();
    for (expr, ov) in overrides {
        let is_match = evaluator
            .matches(expr, triple)
            .wrap_err_with(|| format!("failed to evaluate cfg expression `{expr}`"))?;
        if !is_match {
            continue;
        }
        matched.push((expr.as_str(), ov));
    }

    let exclude_packages = combine_string_set_patches(
        "exclude_packages",
        "workspace target override",
        matched
            .iter()
            .filter_map(|(expr, ov)| ov.exclude_packages.as_ref().map(|patch| (*expr, patch))),
    )?
    .map_or_else(|| base.clone(), |ops| ops.apply_to(base));

    Ok(WorkspaceTargetConfig {
        exclude_packages,
        flags: crate::config::combine_flag_configs(
            None,
            "workspace target override",
            matched.iter().map(|(expr, ov)| (*expr, ov.flags)),
        )?,
        subcommands: crate::config::combine_command_capability_maps(
            "workspace target override",
            matched
                .iter()
                .map(|(expr, ov)| (*expr, &ov.subcommand_overrides)),
        )?,
    })
}

/// Build the target plans for an invocation.
///
/// When `capability_allowed` is false, configured workspace/package target
/// lists are ignored and planning falls back to the single effective target
/// (`--target`, then `CARGO_BUILD_TARGET`, then host). Workspace target
/// overrides (`exclude_packages` patches) still apply to every concrete target,
/// including single-target invocations.
///
/// # Errors
///
/// Returns an error if a target list contains an empty triple, if workspace
/// target overrides conflict, or if cfg evaluation fails.
#[expect(
    clippy::implicit_hasher,
    reason = "callers always pass the std default-hasher HashSet from base_workspace_exclude_packages"
)]
pub fn build_target_plans<'a>(
    selected: &[SelectedPackage<'a>],
    workspace_config: &WorkspaceConfig,
    base_exclude: &HashSet<String>,
    cli_target: Option<&str>,
    capability_allowed: bool,
    env: &impl TargetEnvironment,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<TargetPlans<'a>> {
    if selected.is_empty() {
        return Ok(TargetPlans {
            plans: Vec::new(),
            contains_configured_assignments: false,
        });
    }

    let mut fallback_cache: Option<EffectiveTarget> = None;
    let workspace_targets = if cli_target.is_none() && capability_allowed {
        normalize_targets(&workspace_config.workspace_targets)?
    } else {
        Vec::new()
    };

    let package_targets = if let Some(cli) = cli_target {
        // Explicit `--target` wins globally. Cargo already received the flag
        // from the user, so the source is `Cli` (no injection).
        cli_package_targets(selected, cli)?
    } else if capability_allowed {
        configured_package_targets(selected, &workspace_targets, env, &mut fallback_cache)?
    } else {
        // Capability denied: ignore configured lists, use the fallback single
        // target for every package.
        fallback_package_targets(selected, env, &mut fallback_cache)?
    };
    let contains_configured_assignments = package_targets
        .iter()
        .flat_map(|pt| &pt.targets)
        .any(|assignment| assignment.show_target);

    // Build the global target order: workspace target order first, then
    // package-only targets in selected-package order, then the fallback target
    // (which appears via the packages that use it).
    let order = target_order(&workspace_targets, &package_targets);

    // For each target in order, attach packages whose effective list contains
    // it (preserving each package's source), apply the effective workspace
    // exclude set for that target, and drop empty plans.
    let mut plans = Vec::new();
    for triple in &order {
        let workspace_target_config = resolve_workspace_target_config(
            base_exclude,
            &workspace_config.target_overrides,
            triple,
            evaluator,
        )?;
        let mut packages = Vec::new();
        for pt in &package_targets {
            if workspace_target_config
                .exclude_packages
                .contains(pt.package.name.as_str())
            {
                continue;
            }
            if let Some(assignment) = pt
                .targets
                .iter()
                .find(|assignment| &assignment.target.triple == triple)
            {
                packages.push(PlannedPackage {
                    package: pt.package,
                    config: pt.config,
                    target: assignment.target.clone(),
                    show_target: assignment.show_target,
                });
            }
        }
        if !packages.is_empty() {
            plans.push(TargetPlan {
                target: triple.clone(),
                workspace_target_flags: workspace_target_config.flags,
                workspace_target_subcommands: workspace_target_config.subcommands,
                packages,
            });
        }
    }

    Ok(TargetPlans {
        plans,
        contains_configured_assignments,
    })
}

#[cfg(test)]
mod test {
    use super::{SelectedPackage, build_target_plans};
    use crate::cfg_eval::CfgEvaluator;
    use crate::config::patch::StringSetPatch;
    use crate::config::{
        CommandCapabilities, Config, FlagConfig, WorkspaceConfig, WorkspaceTargetOverride,
    };
    use crate::package::test::package;
    use crate::target::{TargetEnvironment, TargetSource, TargetTriple};
    use color_eyre::eyre;
    use std::collections::{BTreeMap, HashSet};

    struct TestEnv {
        build_target: Option<String>,
        host: String,
    }

    impl TestEnv {
        fn host(host: &str) -> Self {
            Self {
                build_target: None,
                host: host.to_string(),
            }
        }
    }

    impl TargetEnvironment for TestEnv {
        fn cargo_build_target(&self) -> Option<String> {
            self.build_target.clone()
        }
        fn host_target(&self) -> eyre::Result<TargetTriple> {
            Ok(TargetTriple(self.host.clone()))
        }
    }

    struct FailIfUsedEnv;

    impl TargetEnvironment for FailIfUsedEnv {
        fn cargo_build_target(&self) -> Option<String> {
            panic!("empty selection should not read CARGO_BUILD_TARGET");
        }
        fn host_target(&self) -> eyre::Result<TargetTriple> {
            panic!("empty selection should not resolve the host target");
        }
    }

    #[derive(Default)]
    struct StubEval {
        matches: HashSet<String>,
    }

    impl CfgEvaluator for StubEval {
        fn matches(&mut self, cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            Ok(self.matches.contains(cfg_expr))
        }
    }

    struct FailIfUsedEval;

    impl CfgEvaluator for FailIfUsedEval {
        fn matches(&mut self, _cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            panic!("empty selection should not evaluate target overrides");
        }
    }

    struct FailOnTargetEval(&'static str);

    impl CfgEvaluator for FailOnTargetEval {
        fn matches(&mut self, _cfg_expr: &str, target: &TargetTriple) -> eyre::Result<bool> {
            if target.0 == self.0 {
                eyre::bail!("unexpected cfg evaluation for {}", self.0);
            }
            Ok(false)
        }
    }

    /// Evaluator that matches a different cfg per target triple, for the
    /// target-specific workspace exclude test.
    struct PerTargetEval;

    impl CfgEvaluator for PerTargetEval {
        fn matches(&mut self, cfg_expr: &str, target: &TargetTriple) -> eyre::Result<bool> {
            Ok(match target.0.as_str() {
                "linux" => cfg_expr == "cfg(target_os = \"linux\")",
                "wasm" => cfg_expr == "cfg(target_arch = \"wasm32\")",
                _ => false,
            })
        }
    }

    fn config_with_targets(targets: Option<&[&str]>) -> Config {
        Config {
            package_targets: targets.map(|t| t.iter().map(|s| (*s).to_string()).collect()),
            ..Config::default()
        }
    }

    fn workspace_targets(targets: &[&str]) -> WorkspaceConfig {
        WorkspaceConfig {
            workspace_targets: targets.iter().map(|s| (*s).to_string()).collect(),
            ..WorkspaceConfig::default()
        }
    }

    fn selected<'a>(
        package: &'a cargo_metadata::Package,
        config: &'a Config,
    ) -> SelectedPackage<'a> {
        SelectedPackage {
            package,
            config,
            ignore_configured_targets: false,
            target_decision_explicit: false,
        }
    }

    fn triples(plan_targets: &[&super::TargetPlan<'_>]) -> Vec<String> {
        plan_targets.iter().map(|p| p.target.0.clone()).collect()
    }

    #[test]
    fn empty_selection_skips_target_resolution() -> eyre::Result<()> {
        let ws = WorkspaceConfig {
            workspace_targets: vec!["linux".to_string()],
            target_overrides: BTreeMap::from([(
                "cfg(target_os = \"linux\")".to_string(),
                WorkspaceTargetOverride::default(),
            )]),
            ..WorkspaceConfig::default()
        };
        let env = FailIfUsedEnv;
        let mut eval = FailIfUsedEval;

        let plans = build_target_plans(&[], &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert!(plans.plans.is_empty());
        assert!(!plans.contains_configured_assignments);
        Ok(())
    }

    #[test]
    fn no_config_falls_back_to_host_single_target() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = Config::default();
        let selected = vec![selected(&pkg, &cfg)];
        let ws = WorkspaceConfig::default();
        let env = TestEnv::host("host-triple");
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert!(!plans.contains_configured_assignments);
        assert_eq!(plans.plans.len(), 1);
        let plan = &plans.plans[0];
        assert_eq!(plan.target.0, "host-triple");
        assert_eq!(plan.packages.len(), 1);
        assert_eq!(plan.packages[0].target.source, TargetSource::Host);
        Ok(())
    }

    #[test]
    fn workspace_targets_expand_in_order() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = Config::default();
        let selected = vec![selected(&pkg, &cfg)];
        let ws = workspace_targets(&["linux", "windows"]);
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert!(plans.contains_configured_assignments);
        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec!["linux".to_string(), "windows".to_string()]
        );
        for plan in &plans.plans {
            assert_eq!(
                plan.packages[0].target.source,
                TargetSource::WorkspaceConfig
            );
        }
        Ok(())
    }

    #[test]
    fn package_targets_override_workspace_and_keep_order_after() -> eyre::Result<()> {
        // `web` opts into wasm only; `core` inherits the workspace list.
        let web = package("web")?;
        let core = package("core")?;
        let web_cfg = config_with_targets(Some(&["wasm32-unknown-unknown"]));
        let core_cfg = config_with_targets(None);
        let selected = vec![selected(&web, &web_cfg), selected(&core, &core_cfg)];
        let ws = workspace_targets(&["linux", "windows"]);
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        // Workspace targets first, then the package-only wasm target.
        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec![
                "linux".to_string(),
                "windows".to_string(),
                "wasm32-unknown-unknown".to_string()
            ]
        );

        let names = |plan: &super::TargetPlan<'_>| {
            plan.packages
                .iter()
                .map(|p| p.package.name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&plans.plans[0]), vec!["core".to_string()]);
        assert_eq!(names(&plans.plans[1]), vec!["core".to_string()]);
        assert_eq!(names(&plans.plans[2]), vec!["web".to_string()]);
        Ok(())
    }

    #[test]
    fn unused_workspace_target_does_not_evaluate_overrides() -> eyre::Result<()> {
        let pkg = package("web")?;
        let cfg = config_with_targets(Some(&["wasm"]));
        let selected = vec![selected(&pkg, &cfg)];
        let mut ws = workspace_targets(&["linux"]);
        ws.target_overrides.insert(
            "cfg(target_os = \"linux\")".to_string(),
            WorkspaceTargetOverride::default(),
        );
        let env = TestEnv::host("host");
        let mut eval = FailOnTargetEval("linux");

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec!["wasm".to_string()]
        );
        Ok(())
    }

    #[test]
    fn package_opt_out_uses_fallback() -> eyre::Result<()> {
        let pkg = package("native")?;
        let cfg = config_with_targets(Some(&[]));
        let selected = vec![selected(&pkg, &cfg)];
        let ws = workspace_targets(&["linux"]);
        let env = TestEnv::host("host-triple");
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        // Opt-out package uses the host fallback, not the workspace list. But
        // because a configured list exists in the workspace, the host target
        // plan is still produced for this package only.
        assert!(plans.contains_configured_assignments);
        // The workspace `linux` target has no participating package (the only
        // package opted out), so it is dropped.
        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec!["host-triple".to_string()]
        );
        assert_eq!(plans.plans[0].packages[0].target.source, TargetSource::Host);
        Ok(())
    }

    #[test]
    fn explicit_cli_target_overrides_everything() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = config_with_targets(Some(&["wasm32-unknown-unknown"]));
        let selected = vec![selected(&pkg, &cfg)];
        let ws = workspace_targets(&["linux", "windows"]);
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();

        let plans = build_target_plans(
            &selected,
            &ws,
            &HashSet::new(),
            Some("aarch64-apple-darwin"),
            true,
            &env,
            &mut eval,
        )?;

        assert!(plans.contains_configured_assignments);
        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec!["aarch64-apple-darwin".to_string()]
        );
        assert_eq!(plans.plans[0].packages[0].target.source, TargetSource::Cli);
        Ok(())
    }

    #[test]
    fn duplicate_targets_deduped_preserving_order() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = config_with_targets(Some(&["linux", "windows", "linux"]));
        let selected = vec![selected(&pkg, &cfg)];
        let ws = WorkspaceConfig::default();
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec!["linux".to_string(), "windows".to_string()]
        );
        Ok(())
    }

    #[test]
    fn same_triple_keeps_distinct_sources_per_package() -> eyre::Result<()> {
        // `a` inherits the workspace `linux`; `b` lists `linux` itself.
        let a = package("a")?;
        let b = package("b")?;
        let a_cfg = config_with_targets(None);
        let b_cfg = config_with_targets(Some(&["linux"]));
        let selected = vec![selected(&a, &a_cfg), selected(&b, &b_cfg)];
        let ws = workspace_targets(&["linux"]);
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert_eq!(plans.plans.len(), 1);
        let plan = &plans.plans[0];
        assert_eq!(plan.target.0, "linux");
        let by_name = |name: &str| {
            plan.packages
                .iter()
                .find(|p| p.package.name.as_str() == name)
                .map(|p| p.target.source)
        };
        assert_eq!(by_name("a"), Some(TargetSource::WorkspaceConfig));
        assert_eq!(by_name("b"), Some(TargetSource::PackageConfig));
        Ok(())
    }

    #[test]
    fn capability_denied_ignores_configured_lists() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = config_with_targets(Some(&["wasm32-unknown-unknown"]));
        let selected = vec![selected(&pkg, &cfg)];
        let ws = workspace_targets(&["linux", "windows"]);
        let env = TestEnv::host("host-triple");
        let mut eval = StubEval::default();

        let plans = build_target_plans(
            &selected,
            &ws,
            &HashSet::new(),
            None,
            false,
            &env,
            &mut eval,
        )?;

        assert!(!plans.contains_configured_assignments);
        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec!["host-triple".to_string()]
        );
        assert_eq!(plans.plans[0].packages[0].target.source, TargetSource::Host);
        Ok(())
    }

    #[test]
    fn ignored_configured_targets_do_not_mark_target_display() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = Config::default();
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
            ignore_configured_targets: true,
            target_decision_explicit: true,
        }];
        let ws = workspace_targets(&["linux", "windows"]);
        let env = TestEnv::host("host-triple");
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert!(!plans.contains_configured_assignments);
        assert_eq!(
            triples(&plans.plans.iter().collect::<Vec<_>>()),
            vec!["host-triple".to_string()]
        );
        assert_eq!(plans.plans[0].packages[0].target.source, TargetSource::Host);
        Ok(())
    }

    #[test]
    fn cargo_build_target_used_as_fallback_source() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = Config::default();
        let selected = vec![selected(&pkg, &cfg)];
        let ws = WorkspaceConfig::default();
        let env = TestEnv {
            build_target: Some("aarch64-unknown-linux-gnu".to_string()),
            host: "host".to_string(),
        };
        let mut eval = StubEval::default();

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert!(!plans.contains_configured_assignments);
        assert_eq!(plans.plans[0].target.0, "aarch64-unknown-linux-gnu");
        assert_eq!(
            plans.plans[0].packages[0].target.source,
            TargetSource::CargoBuildTargetEnv
        );
        Ok(())
    }

    #[test]
    fn workspace_target_override_excludes_only_matching_targets() -> eyre::Result<()> {
        let native = package("native-cli")?;
        let wasm_app = package("wasm-app")?;
        let native_cfg = Config::default();
        let wasm_cfg = Config::default();
        let selected = vec![
            selected(&native, &native_cfg),
            selected(&wasm_app, &wasm_cfg),
        ];

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_arch = \"wasm32\")".to_string(),
            WorkspaceTargetOverride {
                exclude_packages: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: HashSet::from(["native-cli".to_string()]),
                    remove: HashSet::new(),
                }),
                ..WorkspaceTargetOverride::default()
            },
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            WorkspaceTargetOverride {
                exclude_packages: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: HashSet::from(["wasm-app".to_string()]),
                    remove: HashSet::new(),
                }),
                ..WorkspaceTargetOverride::default()
            },
        );
        let ws = WorkspaceConfig {
            workspace_targets: vec!["linux".to_string(), "wasm".to_string()],
            target_overrides: target,
            ..WorkspaceConfig::default()
        };

        let env = TestEnv::host("host");

        // The exclude set is target-specific, so use an evaluator that matches a
        // different cfg per target triple.
        let mut eval = PerTargetEval;

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert_eq!(plans.plans.len(), 2);
        let names = |plan: &super::TargetPlan<'_>| {
            plan.packages
                .iter()
                .map(|p| p.package.name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(plans.plans[0].target.0, "linux");
        assert_eq!(names(&plans.plans[0]), vec!["native-cli".to_string()]);
        assert_eq!(plans.plans[1].target.0, "wasm");
        assert_eq!(names(&plans.plans[1]), vec!["wasm-app".to_string()]);
        Ok(())
    }

    #[test]
    fn workspace_target_override_applies_to_single_target() -> eyre::Result<()> {
        // No configured target list, single host target, but a workspace target
        // override still excludes a package for the matching host cfg.
        let keep = package("keep")?;
        let drop = package("drop")?;
        let keep_cfg = Config::default();
        let drop_cfg = Config::default();
        let selected = vec![selected(&keep, &keep_cfg), selected(&drop, &drop_cfg)];

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            WorkspaceTargetOverride {
                exclude_packages: Some(StringSetPatch::Override(HashSet::from([
                    "drop".to_string()
                ]))),
                ..WorkspaceTargetOverride::default()
            },
        );
        let ws = WorkspaceConfig {
            target_overrides: target,
            ..WorkspaceConfig::default()
        };

        let env = TestEnv::host("x86_64-unknown-linux-gnu");
        let mut eval = StubEval::default();
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let plans =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)?;

        assert_eq!(plans.plans.len(), 1);
        let names: Vec<String> = plans.plans[0]
            .packages
            .iter()
            .map(|p| p.package.name.to_string())
            .collect();
        assert_eq!(names, vec!["keep".to_string()]);
        Ok(())
    }

    #[test]
    fn conflicting_workspace_target_flags_error() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = Config::default();
        let selected = vec![selected(&pkg, &cfg)];

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            WorkspaceTargetOverride {
                subcommand_overrides: BTreeMap::from([(
                    "check".to_string(),
                    CommandCapabilities {
                        flags: FlagConfig {
                            pedantic: Some(true),
                            ..FlagConfig::default()
                        },
                        ..CommandCapabilities::default()
                    },
                )]),
                ..WorkspaceTargetOverride::default()
            },
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            WorkspaceTargetOverride {
                subcommand_overrides: BTreeMap::from([(
                    "check".to_string(),
                    CommandCapabilities {
                        flags: FlagConfig {
                            pedantic: Some(false),
                            ..FlagConfig::default()
                        },
                        ..CommandCapabilities::default()
                    },
                )]),
                ..WorkspaceTargetOverride::default()
            },
        );
        let ws = WorkspaceConfig {
            target_overrides: target,
            ..WorkspaceConfig::default()
        };
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let Err(err) =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval)
        else {
            eyre::bail!("expected conflicting workspace target flags to fail");
        };

        assert!(err.to_string().contains("subcommands.check.pedantic"));
        Ok(())
    }

    #[test]
    fn base_exclude_applies_to_all_targets() -> eyre::Result<()> {
        let keep = package("keep")?;
        let drop = package("drop")?;
        let keep_cfg = Config::default();
        let drop_cfg = Config::default();
        let selected = vec![selected(&keep, &keep_cfg), selected(&drop, &drop_cfg)];
        let ws = workspace_targets(&["linux", "windows"]);
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();
        let base_exclude = HashSet::from(["drop".to_string()]);

        let plans = build_target_plans(&selected, &ws, &base_exclude, None, true, &env, &mut eval)?;

        for plan in &plans.plans {
            let names: Vec<String> = plan
                .packages
                .iter()
                .map(|p| p.package.name.to_string())
                .collect();
            assert_eq!(names, vec!["keep".to_string()]);
        }
        Ok(())
    }

    #[test]
    fn empty_triple_in_list_is_rejected() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = config_with_targets(Some(&["linux", "  "]));
        let selected = vec![selected(&pkg, &cfg)];
        let ws = WorkspaceConfig::default();
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();

        let result =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval);
        assert!(result.is_err());
        Ok(())
    }
}
