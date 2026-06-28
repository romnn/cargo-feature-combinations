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
use crate::config::{Config, WorkspaceConfig, WorkspaceTargetOverride};
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
}

/// A package assigned to one concrete target.
pub struct PlannedPackage<'a> {
    /// The package.
    pub package: &'a cargo_metadata::Package,
    /// The cached base cargo-fc config for this package.
    pub config: &'a Config,
    /// The concrete target and where it came from.
    pub target: EffectiveTarget,
}

/// All package assignments for one concrete target triple.
pub struct TargetPlan<'a> {
    /// The concrete target triple this plan is for.
    pub target: TargetTriple,
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
    /// concrete source is `Host`/`CargoBuildTargetEnv`. Its only consumer is
    /// output formatting: it gates whether per-entry summaries show the
    /// `target = ...` column.
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

/// Per-package resolved effective target list.
struct PackageTargets<'a> {
    package: &'a cargo_metadata::Package,
    config: &'a Config,
    targets: Vec<EffectiveTarget>,
}

/// Resolve one selected package's effective target list using the configured
/// precedence (CLI handled by the caller as a global override).
fn package_target_list(
    selected: &SelectedPackage<'_>,
    workspace_targets: &[TargetTriple],
    env: &impl TargetEnvironment,
    fallback_cache: &mut Option<EffectiveTarget>,
) -> eyre::Result<Vec<EffectiveTarget>> {
    match &selected.config.targets {
        // Package-level list present.
        Some(list) if !list.is_empty() => {
            let triples = normalize_targets(list)?;
            Ok(triples
                .into_iter()
                .map(|triple| EffectiveTarget {
                    triple,
                    source: TargetSource::PackageConfig,
                })
                .collect())
        }
        // Package-level opt-out (`targets = []`): use the fallback single target.
        Some(_) => Ok(vec![resolve_fallback(env, fallback_cache)?]),
        // No package-level list: inherit workspace targets, else fallback.
        None => {
            if workspace_targets.is_empty() {
                Ok(vec![resolve_fallback(env, fallback_cache)?])
            } else {
                Ok(workspace_targets
                    .iter()
                    .map(|triple| EffectiveTarget {
                        triple: triple.clone(),
                        source: TargetSource::WorkspaceConfig,
                    })
                    .collect())
            }
        }
    }
}

/// Resolve the effective workspace `exclude_packages` set for one target.
///
/// Starts from the base (target-independent) exclude set and applies matching
/// workspace `target.'cfg(...)'` `exclude_packages` patches deterministically
/// (cfg key order). Uses the same patch semantics as package target overrides.
fn resolve_target_excludes(
    base: &HashSet<String>,
    overrides: &BTreeMap<String, WorkspaceTargetOverride>,
    triple: &TargetTriple,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<HashSet<String>> {
    let mut any = false;
    let mut override_value: Option<HashSet<String>> = None;
    let mut add: HashSet<String> = HashSet::new();
    let mut remove: HashSet<String> = HashSet::new();

    for (expr, ov) in overrides {
        let is_match = evaluator
            .matches(expr, triple)
            .wrap_err_with(|| format!("failed to evaluate cfg expression `{expr}`"))?;
        if !is_match {
            continue;
        }
        let Some(patch) = &ov.exclude_packages else {
            continue;
        };
        any = true;

        if let Some(ovv) = patch.override_value() {
            match &override_value {
                None => override_value = Some(ovv.clone()),
                Some(existing) => {
                    if existing != ovv {
                        eyre::bail!(
                            "conflicting overrides for `exclude_packages` from workspace target override `{expr}`"
                        );
                    }
                }
            }
        }
        add.extend(patch.add_values().iter().cloned());
        remove.extend(patch.remove_values().iter().cloned());
    }

    if !any {
        return Ok(base.clone());
    }

    // Order: start from override (or base), then remove, then add (add wins).
    let mut out = override_value.unwrap_or_else(|| base.clone());
    for r in &remove {
        out.remove(r);
    }
    out.extend(add);
    Ok(out)
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
#[allow(
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
    let mut fallback_cache: Option<EffectiveTarget> = None;

    let contains_configured_assignments = if cli_target.is_some() {
        true
    } else if capability_allowed {
        !workspace_config.targets.is_empty() || selected.iter().any(|s| s.config.targets.is_some())
    } else {
        false
    };

    // Resolve each selected package's effective target list.
    let package_targets: Vec<PackageTargets<'a>> = if let Some(cli) = cli_target {
        // Explicit `--target` wins globally: every package runs the single CLI
        // target and all configured lists are ignored. Cargo already received
        // the flag from the user, so the source is `Cli` (no injection).
        let triple = cli.trim();
        if triple.is_empty() {
            eyre::bail!("empty `--target` value");
        }
        let cli_target = EffectiveTarget {
            triple: TargetTriple(triple.to_string()),
            source: TargetSource::Cli,
        };
        selected
            .iter()
            .map(|s| PackageTargets {
                package: s.package,
                config: s.config,
                targets: vec![cli_target.clone()],
            })
            .collect()
    } else if capability_allowed {
        let workspace_targets = normalize_targets(&workspace_config.targets)?;
        let mut out = Vec::with_capacity(selected.len());
        for s in selected {
            let targets = package_target_list(s, &workspace_targets, env, &mut fallback_cache)?;
            out.push(PackageTargets {
                package: s.package,
                config: s.config,
                targets,
            });
        }
        out
    } else {
        // Capability denied: ignore configured lists, use the fallback single
        // target for every package.
        let fallback = resolve_fallback(env, &mut fallback_cache)?;
        selected
            .iter()
            .map(|s| PackageTargets {
                package: s.package,
                config: s.config,
                targets: vec![fallback.clone()],
            })
            .collect()
    };

    // Build the global target order: workspace target order first, then
    // package-only targets in selected-package order, then the fallback target
    // (which appears via the packages that use it).
    let mut order: Vec<TargetTriple> = Vec::new();
    let mut seen: HashSet<TargetTriple> = HashSet::new();

    if cli_target.is_none() && capability_allowed {
        for triple in normalize_targets(&workspace_config.targets)? {
            if seen.insert(triple.clone()) {
                order.push(triple);
            }
        }
    }
    for pt in &package_targets {
        for et in &pt.targets {
            if seen.insert(et.triple.clone()) {
                order.push(et.triple.clone());
            }
        }
    }

    // For each target in order, attach packages whose effective list contains
    // it (preserving each package's source), apply the effective workspace
    // exclude set for that target, and drop empty plans.
    let mut plans = Vec::new();
    for triple in &order {
        let exclude =
            resolve_target_excludes(base_exclude, &workspace_config.target, triple, evaluator)?;
        let mut packages = Vec::new();
        for pt in &package_targets {
            if exclude.contains(pt.package.name.as_str()) {
                continue;
            }
            if let Some(et) = pt.targets.iter().find(|et| &et.triple == triple) {
                packages.push(PlannedPackage {
                    package: pt.package,
                    config: pt.config,
                    target: et.clone(),
                });
            }
        }
        if !packages.is_empty() {
            plans.push(TargetPlan {
                target: triple.clone(),
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
    use crate::config::{Config, WorkspaceConfig, WorkspaceTargetOverride};
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

    #[derive(Default)]
    struct StubEval {
        matches: HashSet<String>,
    }

    impl CfgEvaluator for StubEval {
        fn matches(&mut self, cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            Ok(self.matches.contains(cfg_expr))
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

    fn package(name: &str) -> eyre::Result<cargo_metadata::Package> {
        use cargo_metadata::{PackageBuilder, PackageId, PackageName};
        use semver::Version;
        use std::str::FromStr as _;
        Ok(PackageBuilder::new(
            PackageName::from_str(name)?,
            Version::parse("0.1.0")?,
            PackageId {
                repr: name.to_string(),
            },
            "",
        )
        .build()?)
    }

    fn config_with_targets(targets: Option<&[&str]>) -> Config {
        Config {
            targets: targets.map(|t| t.iter().map(|s| (*s).to_string()).collect()),
            ..Config::default()
        }
    }

    fn workspace_targets(targets: &[&str]) -> WorkspaceConfig {
        WorkspaceConfig {
            targets: targets.iter().map(|s| (*s).to_string()).collect(),
            ..WorkspaceConfig::default()
        }
    }

    fn triples(plan_targets: &[&super::TargetPlan<'_>]) -> Vec<String> {
        plan_targets.iter().map(|p| p.target.0.clone()).collect()
    }

    #[test]
    fn no_config_falls_back_to_host_single_target() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = Config::default();
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
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
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
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
        let selected = vec![
            SelectedPackage {
                package: &web,
                config: &web_cfg,
            },
            SelectedPackage {
                package: &core,
                config: &core_cfg,
            },
        ];
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
    fn package_opt_out_uses_fallback() -> eyre::Result<()> {
        let pkg = package("native")?;
        let cfg = config_with_targets(Some(&[]));
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
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
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
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
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
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
        let selected = vec![
            SelectedPackage {
                package: &a,
                config: &a_cfg,
            },
            SelectedPackage {
                package: &b,
                config: &b_cfg,
            },
        ];
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
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
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
    fn cargo_build_target_used_as_fallback_source() -> eyre::Result<()> {
        let pkg = package("a")?;
        let cfg = Config::default();
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
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
            SelectedPackage {
                package: &native,
                config: &native_cfg,
            },
            SelectedPackage {
                package: &wasm_app,
                config: &wasm_cfg,
            },
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
            },
        );
        let ws = WorkspaceConfig {
            targets: vec!["linux".to_string(), "wasm".to_string()],
            target,
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
        let selected = vec![
            SelectedPackage {
                package: &keep,
                config: &keep_cfg,
            },
            SelectedPackage {
                package: &drop,
                config: &drop_cfg,
            },
        ];

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            WorkspaceTargetOverride {
                exclude_packages: Some(StringSetPatch::Override(HashSet::from([
                    "drop".to_string()
                ]))),
            },
        );
        let ws = WorkspaceConfig {
            target,
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
    fn base_exclude_applies_to_all_targets() -> eyre::Result<()> {
        let keep = package("keep")?;
        let drop = package("drop")?;
        let keep_cfg = Config::default();
        let drop_cfg = Config::default();
        let selected = vec![
            SelectedPackage {
                package: &keep,
                config: &keep_cfg,
            },
            SelectedPackage {
                package: &drop,
                config: &drop_cfg,
            },
        ];
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
        let selected = vec![SelectedPackage {
            package: &pkg,
            config: &cfg,
        }];
        let ws = WorkspaceConfig::default();
        let env = TestEnv::host("host");
        let mut eval = StubEval::default();

        let result =
            build_target_plans(&selected, &ws, &HashSet::new(), None, true, &env, &mut eval);
        assert!(result.is_err());
        Ok(())
    }
}
