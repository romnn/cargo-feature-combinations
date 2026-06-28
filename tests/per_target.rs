//! Integration tests for the per-target feature-combination axis.
//!
//! These drive the public planning + matrix API end to end over real
//! `cargo metadata`, asserting on the matrix rows and target plans. They use
//! deterministic stub adapters (a fixed host environment and a cfg evaluator)
//! so no real `rustc`/`cargo` invocation or installed target is required.

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::Package as _;
use cargo_feature_combinations::cfg_eval::CfgEvaluator;
use cargo_feature_combinations::cli::Options;
use cargo_feature_combinations::config::Config;
use cargo_feature_combinations::runner::{build_execution_plans, build_matrix_rows};
use cargo_feature_combinations::target::{TargetEnvironment, TargetTriple};
use cargo_feature_combinations::target_plan::{SelectedPackage, build_target_plans};
use cargo_feature_combinations::workspace::Workspace as _;
use color_eyre::eyre::{self, OptionExt};
use std::collections::HashSet;

struct HostEnv(&'static str);

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
struct PairEval {
    rules: Vec<(String, String)>,
}

impl PairEval {
    fn matching(triple: &str, cfg: &str) -> Self {
        Self {
            rules: vec![(triple.to_string(), cfg.to_string())],
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

fn single_crate(cargo_toml: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(cargo_toml)?;
    temp.child("src/lib.rs").write_str("pub fn x() {}\n")?;
    Ok(temp)
}

fn metadata(temp: &TempDir) -> eyre::Result<cargo_metadata::Metadata> {
    Ok(cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?)
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
        .map(|(package, config)| SelectedPackage { package, config })
        .collect();
    let base_exclude = meta.base_workspace_exclude_packages()?;

    let target_plans = build_target_plans(
        &selected,
        &ws_config,
        &base_exclude,
        cli_target,
        capability_allowed,
        env,
        evaluator,
    )?;
    let plan_set = build_execution_plans(&target_plans, &Options::default(), false, evaluator)?;
    Ok(build_matrix_rows(&plan_set, false))
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
fn matrix_reserved_target_key_is_overwritten_by_builtin() -> eyre::Result<()> {
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
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");
    let mut eval = PairEval::default();
    let rows = matrix_rows(&meta, None, true, &env, &mut eval)?;

    // The built-in target wins over the user-supplied matrix `target`.
    assert_eq!(targets_in(&rows), HashSet::from(["t-linux".to_string()]));
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
        .map(|(package, config)| SelectedPackage { package, config })
        .collect();
    let base_exclude = meta.base_workspace_exclude_packages()?;
    let env = HostEnv("host-triple");
    let mut eval = cargo_feature_combinations::cfg_eval::RustcCfgEvaluator::default();

    let target_plans = build_target_plans(
        &selected,
        &ws_config,
        &base_exclude,
        None,
        true,
        &env,
        &mut eval,
    )?;
    let err = build_execution_plans(&target_plans, &Options::default(), false, &mut eval)
        .err()
        .ok_or_eyre("expected unavailable target to fail")?;
    assert!(
        err.to_string().contains("definitely-not-a-real-triple")
            || format!("{err:?}").contains("definitely-not-a-real-triple"),
        "error should name the offending triple: {err:?}"
    );
    Ok(())
}
