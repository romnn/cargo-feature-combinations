use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::Package as _;
use color_eyre::eyre::{self, OptionExt};
use std::collections::HashSet;

fn dummy_crate_with_settings(settings: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;

    let cargotoml = temp.child("Cargo.toml");
    cargotoml.write_str(&indoc::formatdoc!(
        r#"
            [package]
            name = "testdummy"
            version = "0.1.0"
            edition = "2024"

            [features]
            cuda = []
            metal = []
            common = []

            {settings}
        "#,
        settings = settings,
    ))?;

    temp.child("src/lib.rs").write_str("pub fn dummy() {}\n")?;

    Ok(temp)
}

fn as_sets(matrix: Vec<String>) -> Vec<HashSet<String>> {
    let mut out: Vec<HashSet<String>> = matrix
        .into_iter()
        .map(|s| {
            if s.is_empty() {
                HashSet::new()
            } else {
                s.split(',').map(|v| v.to_string()).collect::<HashSet<_>>()
            }
        })
        .collect();
    out.sort_by(|a, b| a.len().cmp(&b.len()));
    out
}

fn contains_exact_set(combos: &[HashSet<String>], expected: &[&str]) -> bool {
    let expected: HashSet<&str> = expected.iter().copied().collect();
    combos.iter().any(|set| {
        let actual: HashSet<&str> = set.iter().map(std::string::String::as_str).collect();
        actual == expected
    })
}

#[derive(Default)]
struct StubEval {
    matches: HashSet<String>,
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

#[test]
fn target_override_additive_exclude_features_affects_matrix() -> eyre::Result<()> {
    let settings = indoc::indoc! {r#"
        [package.metadata.cargo-feature-combinations]
        exclude_features = ["default"]

        [package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")']
        exclude_features = { add = ["metal"] }
    "#};

    let temp = dummy_crate_with_settings(settings)?;

    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?;

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "testdummy")
        .ok_or_eyre("test package should exist")?;

    let base = pkg.config()?;

    let mut eval = StubEval::default();
    eval.matches
        .insert("cfg(target_os = \"linux\")".to_string());

    let resolved = cargo_feature_combinations::config::resolve::resolve_config(
        &base,
        &cargo_feature_combinations::target::TargetTriple("x".to_string()),
        &mut eval,
    )?;

    let matrix = pkg.feature_matrix(&resolved)?;
    let combos = as_sets(matrix);

    // Since we excluded metal, no combination should contain it.
    assert!(!contains_exact_set(&combos, &["metal"]));
    assert!(!combos.iter().any(|s| s.contains("metal")));

    // Other features should still appear.
    assert!(combos.iter().any(|s| s.contains("cuda")));
    assert!(combos.iter().any(|s| s.contains("common")));

    Ok(())
}

#[test]
fn target_override_override_array_replaces_base_value() -> eyre::Result<()> {
    let settings = indoc::indoc! {r#"
        [package.metadata.cargo-feature-combinations]
        exclude_features = ["default", "metal"]

        [package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")']
        exclude_features = ["cuda"]
    "#};

    let temp = dummy_crate_with_settings(settings)?;

    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?;

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "testdummy")
        .ok_or_eyre("test package should exist")?;

    let base = pkg.config()?;

    let mut eval = StubEval::default();
    eval.matches
        .insert("cfg(target_os = \"linux\")".to_string());

    let resolved = cargo_feature_combinations::config::resolve::resolve_config(
        &base,
        &cargo_feature_combinations::target::TargetTriple("x".to_string()),
        &mut eval,
    )?;

    // Base excluded metal, but override should replace with only cuda.
    assert!(resolved.exclude_features.contains("cuda"));
    assert!(!resolved.exclude_features.contains("metal"));

    Ok(())
}

#[test]
fn replace_true_rejects_add_remove() -> eyre::Result<()> {
    let settings = indoc::indoc! {r#"
        [package.metadata.cargo-feature-combinations]
        exclude_features = ["default"]

        [package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")']
        replace = true
        exclude_features = { add = ["metal"] }
    "#};

    let temp = dummy_crate_with_settings(settings)?;

    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?;

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "testdummy")
        .ok_or_eyre("test package should exist")?;

    let base = pkg.config()?;

    let mut eval = StubEval::default();
    eval.matches
        .insert("cfg(target_os = \"linux\")".to_string());

    let err = match cargo_feature_combinations::config::resolve::resolve_config(
        &base,
        &cargo_feature_combinations::target::TargetTriple("x".to_string()),
        &mut eval,
    ) {
        Ok(_) => eyre::bail!("expected replace=true validation to fail"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("replace=true"));

    Ok(())
}
