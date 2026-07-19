//! Integration tests for the per-target feature-combination axis.
//!
//! These drive the public planning + matrix API end to end over real
//! `cargo metadata`, asserting on the matrix rows and target plans. They use
//! deterministic stub adapters (a fixed host environment and a cfg evaluator)
//! so no real `rustc`/`cargo` invocation or installed target is required.

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::config::{Config, FlagConfig, WorkspaceConfig};
use cargo_feature_combinations::matrix::build_matrix_rows;
use cargo_feature_combinations::plan::execution::{PlanBuildContext, build_execution_plans};
use cargo_feature_combinations::plan::targets::{
    SelectedPackage, TargetExpansion, TargetPlanRequest, build_target_plans,
};
use cargo_feature_combinations::target::TargetEnvironment;
use cargo_feature_combinations::workspace::Workspace as _;
use cargo_feature_combinations::{CfgEvaluator, Package as _};
use color_eyre::eyre::{self, OptionExt};
use std::collections::HashSet;

mod common;
use common::{HostEnv, PairEval, metadata, single_crate};

fn matrix_context(workspace_config: &WorkspaceConfig) -> PlanBuildContext<'_> {
    PlanBuildContext {
        workspace_config,
        raw_command: None,
        resolved_command: None,
        cli_driver: None,
        cli_env_set: &[],
        cli_env_remove: &[],
        default_diagnostics_allowed: false,
        matrix: true,
    }
}

/// Build matrix rows for a metadata workspace using the deterministic adapters.
fn matrix_rows(
    meta: &cargo_metadata::Metadata,
    cli_target: Option<&str>,
    capability_allowed: bool,
    env: &impl TargetEnvironment,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<Vec<serde_json::Value>> {
    let ws_config = meta.workspace_config()?;
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

    let expansion = match cli_target {
        Some(cli) => TargetExpansion::Explicit(cli),
        None if capability_allowed => TargetExpansion::Configured,
        None => TargetExpansion::Denied,
    };
    let target_plans = build_target_plans(
        &selected,
        &ws_config,
        &base_exclude,
        TargetPlanRequest {
            expansion,
            ..Default::default()
        },
        env,
        evaluator,
    )?;
    let context = matrix_context(&ws_config);
    let plan_set =
        build_execution_plans(&target_plans, FlagConfig::default(), &context, evaluator)?;
    Ok(build_matrix_rows(&plan_set))
}

/// Reduce matrix rows to a comparable `(name, target, features)` set.
fn row_set(rows: &[serde_json::Value]) -> HashSet<(String, String, String)> {
    rows.iter()
        .map(|r| {
            let s = |k: &str| {
                r.get(k)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            (s("name"), s("target"), s("features"))
        })
        .collect()
}

fn targets_in(rows: &[serde_json::Value]) -> HashSet<String> {
    rows.iter()
        .filter_map(|r| r.get("target").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect()
}

#[test]
fn no_configured_targets_matrix_includes_host_on_every_row() -> eyre::Result<()> {
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        b = []
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    assert!(!rows.is_empty());
    assert_eq!(
        targets_in(&rows),
        HashSet::from(["host-triple".to_string()])
    );
    assert!(rows.iter().all(|row| {
        row.get("metadata")
            .and_then(serde_json::Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
    }));
    Ok(())
}

#[test]
fn package_targets_multiply_matrix_rows() -> eyre::Result<()> {
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        [package.metadata.cargo-fc]
        targets = ["t-linux", "t-wasm"]
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    // Two combos ([] and [a]) across two targets = 4 rows.
    assert_eq!(rows.len(), 4);
    assert_eq!(
        targets_in(&rows),
        HashSet::from(["t-linux".to_string(), "t-wasm".to_string()])
    );
    Ok(())
}

#[test]
fn explicit_cli_target_ignores_configured_lists() -> eyre::Result<()> {
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        [package.metadata.cargo-fc]
        targets = ["t-linux", "t-wasm"]
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, Some("t-cli"), true, &env, &mut eval)?;

    assert_eq!(targets_in(&rows), HashSet::from(["t-cli".to_string()]));
    Ok(())
}

#[test]
fn package_opt_out_uses_host_target() -> eyre::Result<()> {
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        [package.metadata.cargo-fc]
        targets = []
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    assert_eq!(
        targets_in(&rows),
        HashSet::from(["host-triple".to_string()])
    );
    Ok(())
}

#[test]
fn capability_denied_ignores_configured_targets() -> eyre::Result<()> {
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        [package.metadata.cargo-fc]
        targets = ["t-linux", "t-wasm"]
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    // Capability denied → configured lists ignored, single host target.
    let rows = matrix_rows(&meta, None, false, &env, &mut eval)?;

    assert_eq!(
        targets_in(&rows),
        HashSet::from(["host-triple".to_string()])
    );
    Ok(())
}

#[test]
fn target_override_changes_feature_rows_per_target() -> eyre::Result<()> {
    // `a` is excluded only for `t-linux` via a matching cfg override, so the
    // linux rows must not contain `a` while the wasm rows do.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        b = []
        [package.metadata.cargo-fc]
        targets = ["t-linux", "t-wasm"]
        [package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
        exclude_features = { add = ["a"] }
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::matching("t-linux", "cfg(target_os = \"linux\")");
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    let linux_has_a = rows.iter().any(|r| {
        r.get("target").and_then(serde_json::Value::as_str) == Some("t-linux")
            && r.get("features")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|f| f.split(',').any(|x| x == "a"))
    });
    let wasm_has_a = rows.iter().any(|r| {
        r.get("target").and_then(serde_json::Value::as_str) == Some("t-wasm")
            && r.get("features")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|f| f.split(',').any(|x| x == "a"))
    });
    assert!(!linux_has_a, "feature `a` should be excluded for t-linux");
    assert!(wasm_has_a, "feature `a` should remain for t-wasm");
    Ok(())
}

#[test]
fn matrix_user_fields_are_nested_under_metadata() -> eyre::Result<()> {
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [package.metadata.cargo-fc]
        targets = ["t-linux"]
        [package.metadata.cargo-fc.matrix]
        target = "user-supplied"
        features = "user-features"
        kind = "ci"
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    // cargo-fc owns top-level fields; user fields live under `metadata`.
    assert_eq!(targets_in(&rows), HashSet::from(["t-linux".to_string()]));
    let row = rows.first().ok_or_eyre("expected one matrix row")?;
    assert_eq!(
        row.get("metadata")
            .and_then(|m| m.get("target"))
            .and_then(serde_json::Value::as_str),
        Some("user-supplied")
    );
    assert_eq!(
        row.get("metadata")
            .and_then(|m| m.get("features"))
            .and_then(serde_json::Value::as_str),
        Some("user-features")
    );
    assert_eq!(
        row.get("metadata")
            .and_then(|m| m.get("kind"))
            .and_then(serde_json::Value::as_str),
        Some("ci")
    );
    assert_eq!(
        serde_json::to_string(row)?,
        r#"{"features":"","metadata":{"features":"user-features","kind":"ci","target":"user-supplied"},"name":"solo","target":"t-linux"}"#,
        "matrix rows are serialized with sorted keys"
    );
    Ok(())
}

fn workspace(root_toml: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(root_toml)?;
    temp.child("member/Cargo.toml").write_str(
        r#"
        [package]
        name = "member"
        version = "0.1.0"
        edition = "2021"
        [features]
        f = []
        "#,
    )?;
    temp.child("member/src/lib.rs")
        .write_str("pub fn x() {}\n")?;
    Ok(temp)
}

#[test]
fn workspace_targets_multiply_matrix_rows() -> eyre::Result<()> {
    let temp = workspace(
        r#"
        [workspace]
        members = ["member"]
        resolver = "2"

        [workspace.metadata.cargo-fc]
        targets = ["ws-a", "ws-b"]
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    assert_eq!(
        targets_in(&rows),
        HashSet::from(["ws-a".to_string(), "ws-b".to_string()])
    );
    assert!(
        rows.iter()
            .all(|r| r.get("name").and_then(serde_json::Value::as_str) == Some("member"))
    );
    Ok(())
}

#[test]
fn package_targets_override_workspace_targets() -> eyre::Result<()> {
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(
        r#"
        [workspace]
        members = ["member"]
        resolver = "2"

        [workspace.metadata.cargo-fc]
        targets = ["ws-a", "ws-b"]
        "#,
    )?;
    temp.child("member/Cargo.toml").write_str(
        r#"
        [package]
        name = "member"
        version = "0.1.0"
        edition = "2021"
        [package.metadata.cargo-fc]
        targets = ["pkg-only"]
        "#,
    )?;
    temp.child("member/src/lib.rs")
        .write_str("pub fn x() {}\n")?;

    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    assert_eq!(targets_in(&rows), HashSet::from(["pkg-only".to_string()]));
    Ok(())
}

#[test]
fn workspace_target_override_excludes_package_for_matching_target() -> eyre::Result<()> {
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(
        r#"
        [workspace]
        members = ["keep", "drop"]
        resolver = "2"

        [workspace.metadata.cargo-fc]
        targets = ["t-linux", "t-wasm"]

        [workspace.metadata.cargo-fc.target.'cfg(target_arch = "wasm32")']
        exclude_packages = { add = ["drop"] }
        "#,
    )?;
    for name in ["keep", "drop"] {
        temp.child(format!("{name}/Cargo.toml"))
            .write_str(&format!(
                r#"
            [package]
            name = "{name}"
            version = "0.1.0"
            edition = "2021"
            "#
            ))?;
        temp.child(format!("{name}/src/lib.rs"))
            .write_str("pub fn x() {}\n")?;
    }

    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::matching("t-wasm", "cfg(target_arch = \"wasm32\")");
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    let set = row_set(&rows);
    // `drop` participates for linux but is excluded for wasm.
    assert!(set.contains(&("drop".to_string(), "t-linux".to_string(), String::new())));
    assert!(!set.contains(&("drop".to_string(), "t-wasm".to_string(), String::new())));
    // `keep` participates for both.
    assert!(set.contains(&("keep".to_string(), "t-linux".to_string(), String::new())));
    assert!(set.contains(&("keep".to_string(), "t-wasm".to_string(), String::new())));
    Ok(())
}

#[test]
fn unavailable_target_with_override_fails_clearly() -> eyre::Result<()> {
    // An invalid triple combined with a target override forces real cfg
    // evaluation (rustc), which fails with a clear, triple-named error.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        [package.metadata.cargo-fc]
        targets = ["definitely-not-a-real-triple"]
        [package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
        exclude_features = { add = ["a"] }
        "#,
    )?;
    let meta = metadata(&temp)?;
    let ws_config = meta.workspace_config()?;
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
    let env = HostEnv("host-triple");
    let mut eval = cargo_feature_combinations::cfg_eval::RustcCfgEvaluator::default();

    let target_plans = build_target_plans(
        &selected,
        &ws_config,
        &base_exclude,
        TargetPlanRequest::default(),
        &env,
        &mut eval,
    )?;
    let context = matrix_context(&ws_config);
    let err = build_execution_plans(&target_plans, FlagConfig::default(), &context, &mut eval)
        .err()
        .ok_or_eyre("expected unavailable target to fail")?;
    assert!(
        err.to_string().contains("definitely-not-a-real-triple")
            || format!("{err:?}").contains("definitely-not-a-real-triple"),
        "error should name the offending triple: {err:?}"
    );
    Ok(())
}
