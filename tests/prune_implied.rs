//! Integration tests for implied feature pruning.

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::Package as _;
use color_eyre::eyre::{self, OptionExt};
use similar_asserts::assert_eq as sim_assert_eq;

fn dummy_crate(features_toml: &str, settings: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;

    let cargotoml = temp.child("Cargo.toml");
    cargotoml.write_str(&indoc::formatdoc!(
        r#"
            [package]
            name = "testpruning"
            version = "0.1.0"
            edition = "2024"

            {features_toml}

            [package.metadata.cargo-feature-combinations]
            {settings}
        "#,
    ))?;

    temp.child("src/lib.rs").write_str("pub fn main() {}\n")?;

    Ok(temp)
}

fn dummy_crate_with_dep(features_toml: &str, settings: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;

    let dep_dir = temp.child("optDep");
    dep_dir
        .child("Cargo.toml")
        .write_str("[package]\nname = \"optDep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n")?;
    dep_dir
        .child("src/lib.rs")
        .write_str("pub fn dummy() {}\n")?;

    let cargotoml = temp.child("Cargo.toml");
    cargotoml.write_str(&indoc::formatdoc!(
        r#"
            [package]
            name = "testpruning"
            version = "0.1.0"
            edition = "2024"

            {features_toml}

            [dependencies]
            optDep = {{ path = "optDep", optional = true }}

            [package.metadata.cargo-feature-combinations]
            {settings}
        "#,
    ))?;

    temp.child("src/lib.rs").write_str("pub fn main() {}\n")?;

    Ok(temp)
}

struct PruneTestResult {
    kept: Vec<Vec<String>>,
    pruned: Vec<(Vec<String>, Vec<String>)>,
}

fn run_prune_test(features_toml: &str, settings: &str) -> eyre::Result<PruneTestResult> {
    let temp = dummy_crate(features_toml, settings)?;
    run_prune_in_dir(&temp)
}

fn run_prune_test_with_dep(features_toml: &str, settings: &str) -> eyre::Result<PruneTestResult> {
    let temp = dummy_crate_with_dep(features_toml, settings)?;
    run_prune_in_dir(&temp)
}

fn run_prune_in_dir(temp: &TempDir) -> eyre::Result<PruneTestResult> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?;

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "testpruning")
        .ok_or_eyre("test package should exist")?;

    let config = pkg.config()?;
    let combos = pkg.feature_combinations(&config)?;
    let result =
        cargo_feature_combinations::implication::maybe_prune(combos, &pkg.features, &config, false);

    let mut kept: Vec<Vec<String>> = result
        .keep
        .into_iter()
        .map(|c| c.into_iter().cloned().collect())
        .collect();
    kept.sort();

    let mut pruned: Vec<(Vec<String>, Vec<String>)> = result
        .pruned
        .into_iter()
        .map(|p| (p.features, p.equivalent_to))
        .collect();
    pruned.sort();

    Ok(PruneTestResult { kept, pruned })
}

fn vs(strs: &[&str]) -> Vec<String> {
    strs.iter().copied().map(String::from).collect()
}

#[test]
fn simple_implication() -> eyre::Result<()> {
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
            C = []
        "#},
        r#"exclude_features = ["default"]"#,
    )?;

    // B implies A, so [A, B] and [A, B, C] are redundant
    sim_assert_eq!(
        result.kept,
        vec![
            vs(&[]),
            vs(&["A"]),
            vs(&["A", "C"]),
            vs(&["B"]),
            vs(&["B", "C"]),
            vs(&["C"]),
        ]
    );
    sim_assert_eq!(result.pruned.len(), 2);
    assert!(
        result
            .pruned
            .iter()
            .any(|(f, e)| f == &vs(&["A", "B"]) && e == &vs(&["B"]))
    );
    assert!(
        result
            .pruned
            .iter()
            .any(|(f, e)| f == &vs(&["A", "B", "C"]) && e == &vs(&["B", "C"]))
    );

    Ok(())
}

#[test]
fn transitive_chain() -> eyre::Result<()> {
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
            C = ["B"]
        "#},
        r#"exclude_features = ["default"]"#,
    )?;

    sim_assert_eq!(
        result.kept,
        vec![vs(&[]), vs(&["A"]), vs(&["B"]), vs(&["C"])]
    );
    sim_assert_eq!(result.pruned.len(), 4);

    Ok(())
}

#[test]
fn include_features_no_false_pruning() -> eyre::Result<()> {
    // When include_features adds A to every combo, [A, B] is NOT redundant
    // with [A] because their resolved sets differ: {A} vs {A, B}.
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
        "#},
        indoc::indoc! {r#"
            exclude_features = ["default"]
            include_features = ["A"]
        "#},
    )?;

    // With include_features = ["A"], combos are [A] and [A, B].
    // resolved([A]) = {A}, resolved([A, B]) = {A, B}. Different! No pruning.
    sim_assert_eq!(result.kept, vec![vs(&["A"]), vs(&["A", "B"])]);
    sim_assert_eq!(result.pruned.len(), 0);

    Ok(())
}

#[test]
fn disabled_via_config() -> eyre::Result<()> {
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
        "#},
        indoc::indoc! {r#"
            exclude_features = ["default"]
            prune_implied = false
        "#},
    )?;

    // All 4 combos preserved when pruning is disabled
    sim_assert_eq!(result.kept.len(), 4);
    sim_assert_eq!(result.pruned.len(), 0);

    Ok(())
}

#[test]
fn allow_feature_sets_bypasses_pruning() -> eyre::Result<()> {
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
        "#},
        indoc::indoc! {r#"
            exclude_features = ["default"]
            allow_feature_sets = [["A"], ["A", "B"], ["B"]]
        "#},
    )?;

    // allow_feature_sets bypasses pruning entirely
    sim_assert_eq!(result.kept, vec![vs(&["A"]), vs(&["A", "B"]), vs(&["B"])]);
    sim_assert_eq!(result.pruned.len(), 0);

    Ok(())
}

#[test]
fn dep_syntax_not_treated_as_implication() -> eyre::Result<()> {
    let result = run_prune_test_with_dep(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["dep:optDep", "A"]
        "#},
        r#"exclude_features = ["default"]"#,
    )?;

    // B implies A (plain name), dep:optDep is ignored for the graph.
    // So [A, B] is redundant with [B].
    sim_assert_eq!(result.kept, vec![vs(&[]), vs(&["A"]), vs(&["B"])]);
    sim_assert_eq!(result.pruned, vec![(vs(&["A", "B"]), vs(&["B"]))]);

    Ok(())
}

#[test]
fn diamond_graph() -> eyre::Result<()> {
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
            C = ["A"]
            D = ["B", "C"]
        "#},
        r#"exclude_features = ["default"]"#,
    )?;

    sim_assert_eq!(
        result.kept,
        vec![
            vs(&[]),
            vs(&["A"]),
            vs(&["B"]),
            vs(&["B", "C"]),
            vs(&["C"]),
            vs(&["D"]),
        ]
    );
    sim_assert_eq!(result.pruned.len(), 10);

    Ok(())
}

#[test]
fn no_implications_no_pruning() -> eyre::Result<()> {
    let result = run_prune_test(
        indoc::indoc! {"
            [features]
            A = []
            B = []
            C = []
        "},
        r#"exclude_features = ["default"]"#,
    )?;

    sim_assert_eq!(result.kept.len(), 8); // 2^3 = 8
    sim_assert_eq!(result.pruned.len(), 0);

    Ok(())
}

#[test]
fn isolated_feature_sets_with_pruning() -> eyre::Result<()> {
    // Pruning applies to the final combined output of isolated sets.
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
            C = []
            D = []
        "#},
        indoc::indoc! {r#"
            exclude_features = ["default"]
            isolated_feature_sets = [["A", "B"], ["C", "D"]]
        "#},
    )?;

    // Isolated set 1 generates: [], [A], [B], [A, B]
    // Isolated set 2 generates: [], [C], [D], [C, D]
    // Merged (deduped): [], [A], [B], [A, B], [C], [D], [C, D]
    // Pruning: [A, B] is redundant with [B] (B implies A)
    assert!(
        !result.kept.contains(&vs(&["A", "B"])),
        "[A, B] should be pruned"
    );
    assert!(
        result.kept.contains(&vs(&["B"])),
        "[B] should be kept as the representative"
    );
    // [C, D] has no implications, should be kept
    assert!(result.kept.contains(&vs(&["C", "D"])));
    sim_assert_eq!(result.pruned.len(), 1);

    Ok(())
}

#[test]
fn exclude_feature_sets_with_pruning() -> eyre::Result<()> {
    // exclude_feature_sets is applied first, then pruning on the remainder.
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
            C = []
        "#},
        indoc::indoc! {r#"
            exclude_features = ["default"]
            exclude_feature_sets = [["B", "C"]]
        "#},
    )?;

    // Without exclusion: [], [A], [B], [C], [A,B], [A,C], [B,C], [A,B,C]
    // After exclude [B,C]: [], [A], [B], [C], [A,B], [A,C], [A,B,C] removed too
    //   (exclude_feature_sets removes any combo containing ALL features of the
    //    skip set, so [B,C] and [A,B,C] are removed)
    // After pruning: [A,B] redundant with [B] (B implies A)
    assert!(!result.kept.contains(&vs(&["A", "B"])));
    assert!(!result.kept.contains(&vs(&["B", "C"])));
    assert!(!result.kept.contains(&vs(&["A", "B", "C"])));
    assert!(result.kept.contains(&vs(&["B"])));

    Ok(())
}

#[test]
fn no_empty_feature_set_with_pruning() -> eyre::Result<()> {
    let result = run_prune_test(
        indoc::indoc! {r#"
            [features]
            A = []
            B = ["A"]
        "#},
        indoc::indoc! {r#"
            exclude_features = ["default"]
            no_empty_feature_set = true
        "#},
    )?;

    // Without no_empty_feature_set: [], [A], [B], [A, B]
    // After no_empty_feature_set: [A], [B], [A, B]
    // After pruning: [A, B] redundant with [B]
    sim_assert_eq!(result.kept, vec![vs(&["A"]), vs(&["B"])]);
    sim_assert_eq!(result.pruned.len(), 1);

    Ok(())
}

#[derive(Default)]
struct StubEval {
    matches: std::collections::HashSet<String>,
}

impl cargo_feature_combinations::cfg_eval::CfgEvaluator for StubEval {
    fn matches(
        &mut self,
        cfg_expr: &str,
        _target: &cargo_feature_combinations::target::TargetTriple,
    ) -> eyre::Result<bool> {
        Ok(self.matches.contains(cfg_expr))
    }
}

fn resolve_and_prune(temp: &TempDir, matching_cfgs: &[&str]) -> eyre::Result<PruneTestResult> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?;

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "testpruning")
        .ok_or_eyre("test package should exist")?;

    let base = pkg.config()?;
    let mut eval = StubEval::default();
    for &cfg in matching_cfgs {
        eval.matches.insert(cfg.to_string());
    }

    let config = cargo_feature_combinations::config::resolve::resolve_config(
        &base,
        &cargo_feature_combinations::target::TargetTriple("x".to_string()),
        &mut eval,
    )?;

    let combos = pkg.feature_combinations(&config)?;
    let result =
        cargo_feature_combinations::implication::maybe_prune(combos, &pkg.features, &config, false);

    let mut kept: Vec<Vec<String>> = result
        .keep
        .into_iter()
        .map(|c| c.into_iter().cloned().collect())
        .collect();
    kept.sort();

    let mut pruned: Vec<(Vec<String>, Vec<String>)> = result
        .pruned
        .into_iter()
        .map(|p| (p.features, p.equivalent_to))
        .collect();
    pruned.sort();

    Ok(PruneTestResult { kept, pruned })
}

#[test]
fn target_override_changes_matrix_then_pruning_applies() -> eyre::Result<()> {
    // A target override excludes feature C, changing the matrix.
    // Pruning should apply to the resulting smaller matrix.
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(indoc::indoc! {r#"
        [package]
        name = "testpruning"
        version = "0.1.0"
        edition = "2024"

        [features]
        A = []
        B = ["A"]
        C = []

        [package.metadata.cargo-feature-combinations]
        exclude_features = ["default"]

        [package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")']
        exclude_features = { add = ["C"] }
    "#})?;
    temp.child("src/lib.rs").write_str("")?;

    // On "linux": C is excluded, matrix is [], [A], [B], [A, B]
    // Pruning: [A, B] redundant with [B] (B implies A)
    let linux = resolve_and_prune(&temp, &["cfg(target_os = \"linux\")"])?;
    sim_assert_eq!(linux.kept, vec![vs(&[]), vs(&["A"]), vs(&["B"])]);
    sim_assert_eq!(linux.pruned, vec![(vs(&["A", "B"]), vs(&["B"]))]);

    // On other targets: no override, full matrix with C included
    // Pruning: [A, B] → [B], [A, B, C] → [B, C]
    let other = resolve_and_prune(&temp, &[])?;
    sim_assert_eq!(
        other.kept,
        vec![
            vs(&[]),
            vs(&["A"]),
            vs(&["A", "C"]),
            vs(&["B"]),
            vs(&["B", "C"]),
            vs(&["C"]),
        ]
    );
    sim_assert_eq!(other.pruned.len(), 2);

    Ok(())
}

#[test]
fn target_override_disables_pruning_for_specific_target() -> eyre::Result<()> {
    // Base config has pruning enabled (default). A target override disables it.
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(indoc::indoc! {r#"
        [package]
        name = "testpruning"
        version = "0.1.0"
        edition = "2024"

        [features]
        A = []
        B = ["A"]

        [package.metadata.cargo-feature-combinations]
        exclude_features = ["default"]

        [package.metadata.cargo-feature-combinations.target.'cfg(target_os = "macos")']
        prune_implied = false
    "#})?;
    temp.child("src/lib.rs").write_str("")?;

    // On macOS: pruning disabled by target override
    let macos = resolve_and_prune(&temp, &["cfg(target_os = \"macos\")"])?;
    sim_assert_eq!(macos.kept.len(), 4); // all 2^2 combos
    sim_assert_eq!(macos.pruned.len(), 0);

    // On other targets: pruning active
    let other = resolve_and_prune(&temp, &[])?;
    sim_assert_eq!(other.kept, vec![vs(&[]), vs(&["A"]), vs(&["B"])]);
    sim_assert_eq!(other.pruned.len(), 1);

    Ok(())
}

#[test]
fn target_override_include_features_interacts_with_pruning() -> eyre::Result<()> {
    // A target override adds A to include_features, making it appear in every combo.
    // This changes whether pruning can fire.
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(indoc::indoc! {r#"
        [package]
        name = "testpruning"
        version = "0.1.0"
        edition = "2024"

        [features]
        A = []
        B = ["A"]

        [package.metadata.cargo-feature-combinations]
        exclude_features = ["default"]

        [package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")']
        include_features = { add = ["A"] }
    "#})?;
    temp.child("src/lib.rs").write_str("")?;

    // On Linux: include_features = ["A"], so combos are [A], [A, B]
    // resolved([A]) = {A}, resolved([A, B]) = {A, B} → DIFFERENT, no pruning
    let linux = resolve_and_prune(&temp, &["cfg(target_os = \"linux\")"])?;
    sim_assert_eq!(linux.kept, vec![vs(&["A"]), vs(&["A", "B"])]);
    sim_assert_eq!(linux.pruned.len(), 0);

    // On other targets: no include_features, combos are [], [A], [B], [A, B]
    // resolved([A, B]) = resolved([B]) = {A, B} → [A, B] pruned
    let other = resolve_and_prune(&temp, &[])?;
    sim_assert_eq!(other.kept, vec![vs(&[]), vs(&["A"]), vs(&["B"])]);
    sim_assert_eq!(other.pruned.len(), 1);

    Ok(())
}
