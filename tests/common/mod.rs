//! Shared deterministic stub adapters for the integration tests.
//!
//! These let the planning + matrix API run end to end over real
//! `cargo metadata` without any installed target or real `rustc`/`cargo`
//! invocation.

#![allow(
    dead_code,
    reason = "shared integration-test fixtures; not every test binary uses every item"
)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::config::{Config, FlagConfig, WorkspaceConfig};
use cargo_feature_combinations::matrix::build_matrix_rows;
use cargo_feature_combinations::plan::execution::{PlanBuildContext, build_execution_plans};
use cargo_feature_combinations::plan::targets::{
    SelectedPackage, TargetPlanRequest, build_target_plans,
};
use cargo_feature_combinations::target::{TargetEnvironment, TargetTriple};
use cargo_feature_combinations::workspace::Workspace as _;
use cargo_feature_combinations::{CfgEvaluator, Package as _};
use color_eyre::eyre;

/// A fixed host-target environment with no `CARGO_BUILD_TARGET`.
pub struct HostEnv(pub &'static str);

impl TargetEnvironment for HostEnv {
    fn cargo_build_target(&self) -> Option<String> {
        None
    }
    fn host_target(&self) -> eyre::Result<TargetTriple> {
        Ok(TargetTriple(self.0.to_string()))
    }
}

/// cfg evaluator matching `(triple, cfg)` pairs exactly, with no real `rustc`.
#[derive(Default)]
pub struct PairEval {
    rules: Vec<(String, String)>,
}

impl PairEval {
    pub fn matching(triple: &str, cfg: &str) -> Self {
        Self {
            rules: vec![(triple.to_string(), cfg.to_string())],
        }
    }

    /// Matches every one of `cfgs` on `triple`, as when several target sections apply.
    pub fn matching_all(triple: &str, cfgs: &[&str]) -> Self {
        Self {
            rules: cfgs
                .iter()
                .map(|cfg| (triple.to_string(), (*cfg).to_string()))
                .collect(),
        }
    }
}

impl CfgEvaluator for PairEval {
    fn matches(&mut self, cfg_expr: &str, target: &TargetTriple) -> eyre::Result<bool> {
        Ok(self
            .rules
            .iter()
            .any(|(t, c)| t == target.as_str() && c == cfg_expr))
    }
}

/// Create a single-crate temp workspace from the given `Cargo.toml` contents.
pub fn single_crate(cargo_toml: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(cargo_toml)?;
    temp.child("src/lib.rs").write_str("pub fn x() {}\n")?;
    Ok(temp)
}

/// Run `cargo metadata` (no deps) against a temp workspace.
pub fn metadata(temp: &TempDir) -> eyre::Result<cargo_metadata::Metadata> {
    Ok(cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?)
}

/// Build matrix rows for the given cargo subcommand using deterministic adapters.
pub fn matrix_rows_for_command(
    meta: &cargo_metadata::Metadata,
    command: Option<&str>,
    env: &impl TargetEnvironment,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<Vec<serde_json::Value>> {
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
        cli_env_set: &[],
        cli_env_remove: &[],
        default_diagnostics_allowed: false,
        host: None,
        matrix: true,
    };
    let mut plan_set =
        build_execution_plans(&target_plans, FlagConfig::default(), &context, evaluator)?;
    // Mirror production planning, which always runs the maximal-features pass
    // after building execution plans.
    cargo_feature_combinations::plan::maximal::maybe_retain_maximal_feature_sets(&mut plan_set);
    Ok(build_matrix_rows(&plan_set))
}
