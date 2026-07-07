//! Integration tests for per-scope build-driver resolution.
//!
//! These parse real manifests via `cargo metadata` (so the `driver` keys flow
//! through serde into the scope structs) and drive the public planning API,
//! asserting the driver resolved onto each `PackageExecutionPlan`. The plan
//! carries the *config-resolved* driver (before cargo-fc's cross-target
//! `cargo-zigbuild` default, which is applied later against a real host).

use cargo_feature_combinations::config::{Config, FlagConfig, WorkspaceConfig};
use cargo_feature_combinations::plan::execution::{PlanBuildContext, build_execution_plans};
use cargo_feature_combinations::plan::targets::{
    SelectedPackage, TargetPlanRequest, build_target_plans,
};
use cargo_feature_combinations::target::TargetEnvironment;
use cargo_feature_combinations::workspace::Workspace as _;
use cargo_feature_combinations::{CfgEvaluator, Package as _};
use color_eyre::eyre;
use std::collections::HashMap;

mod common;
use common::{HostEnv, PairEval, metadata, single_crate};

/// Resolve the config-level driver per target triple for the given command,
/// keyed by triple, for the single package in `meta`.
fn drivers_by_target(
    meta: &cargo_metadata::Metadata,
    command: Option<&str>,
    env: &impl TargetEnvironment,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<HashMap<String, Option<String>>> {
    let ws_config: WorkspaceConfig = meta.workspace_config()?;
    let packages = meta.candidate_packages_for_fc()?;
    let configs: Vec<Config> = packages
        .iter()
        .map(|p| p.config())
        .collect::<eyre::Result<Vec<_>>>()?;
    let selected: Vec<SelectedPackage<'_>> = packages
        .iter()
        .zip(&configs)
        .map(|(package, config)| SelectedPackage {
            package,
            config,
            ignore_configured_targets: false,
            target_decision_explicit: false,
        })
        .collect();
    let base_exclude = meta.base_workspace_exclude_packages()?;

    let target_plans = build_target_plans(
        &selected,
        &ws_config,
        &base_exclude,
        TargetPlanRequest {
            raw_command: command,
            resolved_command: command,
            ..Default::default()
        },
        env,
        evaluator,
    )?;
    let context = PlanBuildContext {
        workspace_config: &ws_config,
        raw_command: command,
        resolved_command: command,
        cli_driver: None,
        default_diagnostics_allowed: false,
        matrix: false,
    };
    let plan_set =
        build_execution_plans(&target_plans, FlagConfig::default(), &context, evaluator)?;

    let mut out = HashMap::new();
    for plan in &plan_set.plans {
        for pp in &plan.package_plans {
            out.insert(plan.target.as_str().to_string(), pp.driver.clone());
        }
    }
    Ok(out)
}

#[test]
fn target_scoped_driver_resolves_only_for_its_target() -> eyre::Result<()> {
    // The wasm target cross-compiles with `cross`; the linux target inherits the
    // package driver `cargo-zigbuild`.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [package.metadata.cargo-fc]
        targets = ["t-linux", "t-wasm"]
        driver = "cargo-zigbuild"
        [package.metadata.cargo-fc.target.'cfg(target_arch = "wasm32")']
        driver = "cross"
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::matching("t-wasm", "cfg(target_arch = \"wasm32\")");

    let drivers = drivers_by_target(&meta, Some("build"), &env, &mut eval)?;
    assert_eq!(
        drivers.get("t-wasm"),
        Some(&Some("cross".to_string())),
        "wasm target uses its own driver"
    );
    assert_eq!(
        drivers.get("t-linux"),
        Some(&Some("cargo-zigbuild".to_string())),
        "linux target inherits the package driver"
    );
    Ok(())
}

#[test]
fn subcommand_driver_overrides_package_driver_for_that_command() -> eyre::Result<()> {
    // The package builds with `cargo-zigbuild`, but `cargo fc test` forces plain
    // `cargo` for the whole package.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [package.metadata.cargo-fc]
        driver = "cargo-zigbuild"
        [package.metadata.cargo-fc.subcommands.test]
        driver = "cargo"
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");

    let mut eval = PairEval::default();
    let test_drivers = drivers_by_target(&meta, Some("test"), &env, &mut eval)?;
    assert_eq!(
        test_drivers.get("host-triple"),
        Some(&Some("cargo".to_string())),
        "`test` overrides the driver to plain cargo"
    );

    let mut eval = PairEval::default();
    let build_drivers = drivers_by_target(&meta, Some("build"), &env, &mut eval)?;
    assert_eq!(
        build_drivers.get("host-triple"),
        Some(&Some("cargo-zigbuild".to_string())),
        "`build` keeps the package driver"
    );
    Ok(())
}
