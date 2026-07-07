//! Integration tests for the per-subcommand feature-combination axis.
//!
//! These drive the public planning + matrix API end to end over real
//! `cargo metadata`, asserting that a `subcommands.<name>` override reshapes the
//! feature matrix only for that command (e.g. features enabled for `build` but
//! disabled for `test`). They use deterministic stub adapters so no real
//! `rustc`/`cargo` invocation or installed target is required.

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::plan::targets::{
    SelectedPackage, TargetPlanRequest, build_target_plans,
};
use cargo_feature_combinations::target::TargetEnvironment;
use cargo_feature_combinations::workspace::Workspace as _;
use cargo_feature_combinations::{
    CfgEvaluator, Config, FlagConfig, Package as _, PlanBuildContext, WorkspaceConfig,
    build_execution_plans, build_matrix_rows,
};
use color_eyre::eyre;

mod common;
use common::{HostEnv, PairEval, metadata, single_crate};

/// Build matrix rows for the given cargo subcommand using deterministic adapters.
fn matrix_rows_for_command(
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
        default_diagnostics_allowed: false,
        matrix: true,
    };
    let plan_set =
        build_execution_plans(&target_plans, FlagConfig::default(), &context, evaluator)?;
    Ok(build_matrix_rows(&plan_set))
}

/// Whether any matrix row's feature list contains `feature`.
fn any_row_has_feature(rows: &[serde_json::Value], feature: &str) -> bool {
    rows.iter().any(|r| {
        r.get("features")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|f| f.split(',').any(|x| x == feature))
    })
}

#[test]
fn subcommand_override_excludes_feature_only_for_that_command() -> eyre::Result<()> {
    // `gpu` is excluded for `test` but kept for `build` and every other command.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        gpu = []
        cpu = []
        [package.metadata.cargo-fc.subcommands.test]
        exclude_features = { add = ["gpu"] }
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");

    let mut eval = PairEval::default();
    let test_rows = matrix_rows_for_command(&meta, Some("test"), &env, &mut eval)?;
    assert!(
        !any_row_has_feature(&test_rows, "gpu"),
        "feature `gpu` should be excluded for `cargo fc test`"
    );
    assert!(
        any_row_has_feature(&test_rows, "cpu"),
        "feature `cpu` should still be present for `cargo fc test`"
    );

    let mut eval = PairEval::default();
    let build_rows = matrix_rows_for_command(&meta, Some("build"), &env, &mut eval)?;
    assert!(
        any_row_has_feature(&build_rows, "gpu"),
        "feature `gpu` should remain for `cargo fc build`"
    );

    // The command-less path (e.g. bare planning) is unaffected too.
    let mut eval = PairEval::default();
    let no_command_rows = matrix_rows_for_command(&meta, None, &env, &mut eval)?;
    assert!(any_row_has_feature(&no_command_rows, "gpu"));
    Ok(())
}

#[test]
fn subcommand_only_features_restricts_matrix_for_that_command() -> eyre::Result<()> {
    // `cargo fc test` runs a single, focused feature: only `core`.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        core = []
        extra = []
        [package.metadata.cargo-fc.subcommands.test]
        only_features = ["core"]
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");

    let mut eval = PairEval::default();
    let test_rows = matrix_rows_for_command(&meta, Some("test"), &env, &mut eval)?;
    assert!(!any_row_has_feature(&test_rows, "extra"));
    // Only `[]` and `[core]` remain.
    assert_eq!(test_rows.len(), 2);

    let mut eval = PairEval::default();
    let build_rows = matrix_rows_for_command(&meta, Some("build"), &env, &mut eval)?;
    assert!(any_row_has_feature(&build_rows, "extra"));
    Ok(())
}

#[test]
fn target_and_subcommand_feature_overrides_compose() -> eyre::Result<()> {
    // `a` is excluded for the linux target; `b` is excluded for `test`. Running
    // `cargo fc test` on linux drops both.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        a = []
        b = []
        c = []
        [package.metadata.cargo-fc]
        targets = ["t-linux"]
        [package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
        exclude_features = { add = ["a"] }
        [package.metadata.cargo-fc.subcommands.test]
        exclude_features = { add = ["b"] }
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");

    let mut eval = PairEval::matching("t-linux", "cfg(target_os = \"linux\")");
    let test_rows = matrix_rows_for_command(&meta, Some("test"), &env, &mut eval)?;
    assert!(!any_row_has_feature(&test_rows, "a"), "target excludes `a`");
    assert!(
        !any_row_has_feature(&test_rows, "b"),
        "subcommand excludes `b`"
    );
    assert!(any_row_has_feature(&test_rows, "c"), "`c` is untouched");

    // For `build` on linux, only the target exclusion applies.
    let mut eval = PairEval::matching("t-linux", "cfg(target_os = \"linux\")");
    let build_rows = matrix_rows_for_command(&meta, Some("build"), &env, &mut eval)?;
    assert!(!any_row_has_feature(&build_rows, "a"));
    assert!(any_row_has_feature(&build_rows, "b"));
    Ok(())
}

#[test]
fn target_subcommand_override_narrows_to_target_and_command() -> eyre::Result<()> {
    // A `target.'cfg(...)'.subcommands.test` section applies only when both the
    // target matches and the command is `test`.
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
        targets = ["t-linux"]
        [package.metadata.cargo-fc.target.'cfg(target_os = "linux")'.subcommands.test]
        exclude_features = { add = ["a"] }
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");

    let mut eval = PairEval::matching("t-linux", "cfg(target_os = \"linux\")");
    let test_rows = matrix_rows_for_command(&meta, Some("test"), &env, &mut eval)?;
    assert!(!any_row_has_feature(&test_rows, "a"));

    // Same target, different command: the target×subcommand section is inert.
    let mut eval = PairEval::matching("t-linux", "cfg(target_os = \"linux\")");
    let build_rows = matrix_rows_for_command(&meta, Some("build"), &env, &mut eval)?;
    assert!(any_row_has_feature(&build_rows, "a"));
    Ok(())
}

#[test]
fn subcommand_alias_resolves_to_builtin_override() -> eyre::Result<()> {
    // `t` is cargo's built-in alias for `test`; the `subcommands.test` override
    // must apply when the raw token is the alias.
    let temp = single_crate(
        r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"
        [features]
        gpu = []
        [package.metadata.cargo-fc.subcommands.test]
        exclude_features = { add = ["gpu"] }
        "#,
    )?;
    let meta = metadata(&temp)?;
    let env = HostEnv("host-triple");

    let mut eval = PairEval::default();
    let rows = matrix_rows_for_command(&meta, Some("t"), &env, &mut eval)?;
    assert!(
        !any_row_has_feature(&rows, "gpu"),
        "the `t` alias should resolve to the `test` subcommand override"
    );
    Ok(())
}

/// The package names present in a matrix-row set.
fn row_package_names(rows: &[serde_json::Value]) -> std::collections::HashSet<String> {
    rows.iter()
        .filter_map(|r| r.get("name").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect()
}

#[test]
fn workspace_subcommand_excludes_package_for_that_command() -> eyre::Result<()> {
    // `exclude a package when testing`: the workspace subcommand override drops
    // `skip` for `cargo fc test`, but it participates for `cargo fc build`.
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(
        r#"
        [workspace]
        members = ["keep", "skip"]
        resolver = "2"

        [workspace.metadata.cargo-fc.subcommands.test]
        exclude_packages = { add = ["skip"] }
        "#,
    )?;
    for name in ["keep", "skip"] {
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

    let mut eval = PairEval::default();
    let test_names = row_package_names(&matrix_rows_for_command(
        &meta,
        Some("test"),
        &env,
        &mut eval,
    )?);
    assert!(test_names.contains("keep"));
    assert!(!test_names.contains("skip"), "skip is excluded for test");

    let mut eval = PairEval::default();
    let build_names = row_package_names(&matrix_rows_for_command(
        &meta,
        Some("build"),
        &env,
        &mut eval,
    )?);
    assert!(build_names.contains("keep"));
    assert!(build_names.contains("skip"), "skip participates for build");
    Ok(())
}
