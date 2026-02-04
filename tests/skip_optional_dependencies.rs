//! Integration tests for skipping implicit optional dependency features.

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::Package as _;
use color_eyre::eyre::{self, OptionExt};
use std::collections::HashSet;

fn dummy_crate_with_settings(settings: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;

    // Create dummy dependency crates referenced via `path` so that `cargo metadata` succeeds.
    for dep in ["fixDepA", "optDepB", "optDepC"] {
        let dep_dir = temp.child(dep);
        dep_dir.child("Cargo.toml").write_str(&format!(
            "[package]\nname = \"{dep}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
        ))?;
        dep_dir
            .child("src/lib.rs")
            .write_str("pub fn dummy() {}\n")?;
    }

    // Root crate that uses optional dependencies and a small feature graph.
    let cargotoml = temp.child("Cargo.toml");
    cargotoml.write_str(&indoc::formatdoc!(
        r#"
            [package]
            name = "testdummy"
            version = "0.1.0"
            edition = "2024"

            [features]
            A = []
            B = ["A"]
            C = ["dep:optDepC"]

            [dependencies]
            fixDepA = {{ path = "fixDepA" }}
            oDepB = {{ path = "optDepB", package = "optDepB", optional = true }}
            optDepC = {{ path = "optDepC", optional = true }}

            [package.metadata.cargo-feature-combinations]
            {settings}
        "#,
        settings = settings,
    ))?;

    temp.child("src/lib.rs").write_str("pub fn main() {}\n")?;

    Ok(temp)
}

fn feature_sets_for_settings(settings: &str) -> eyre::Result<Vec<Vec<String>>> {
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

    let config = pkg.config()?;
    let matrix = pkg.feature_matrix(&config)?;

    // Normalize into sorted vectors of feature names per combination.
    let mut combos: Vec<Vec<String>> = matrix
        .into_iter()
        .map(|s| {
            if s.is_empty() {
                Vec::new()
            } else {
                let mut v: Vec<String> =
                    s.split(',').map(std::string::ToString::to_string).collect();
                v.sort();
                v
            }
        })
        .collect();

    combos.sort();
    Ok(combos)
}

fn contains_exact_set(combos: &[Vec<String>], expected: &[&str]) -> bool {
    let expected: HashSet<&str> = expected.iter().copied().collect();
    combos.iter().any(|set| {
        let actual: HashSet<&str> = set.iter().map(std::string::String::as_str).collect();
        actual == expected
    })
}

fn as_vec_string(sets: &[Vec<&str>]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = sets
        .iter()
        .map(|set| set.iter().map(std::string::ToString::to_string).collect())
        .collect();
    out.sort();
    out
}

#[test]
fn parity_simple_like_cargo_all_features() -> eyre::Result<()> {
    // Mirror the `simple` test from cargo-all-features/tests/settings.rs, but
    // explicitly exclude the implicit `default` feature using
    // `exclude_features` so that the remaining behaviour matches.
    let settings = indoc::indoc! {r#"
        exclude_features = ["default"]
    "#};

    let combos = feature_sets_for_settings(settings)?;

    let expected: Vec<Vec<&str>> = vec![
        vec![],
        vec!["A"],
        vec!["B"],
        vec!["C"],
        vec!["oDepB"],
        vec!["A", "B"],
        vec!["A", "C"],
        vec!["A", "oDepB"],
        vec!["B", "C"],
        vec!["B", "oDepB"],
        vec!["C", "oDepB"],
        vec!["A", "B", "C"],
        vec!["A", "B", "oDepB"],
        vec!["A", "C", "oDepB"],
        vec!["B", "C", "oDepB"],
        vec!["A", "B", "C", "oDepB"],
    ];

    assert_eq!(combos, as_vec_string(&expected));

    Ok(())
}

#[test]
fn parity_skip_optional_dependencies_like_cargo_all_features() -> eyre::Result<()> {
    // Mirror the `skip_opt_deps` test from cargo-all-features/tests/settings.rs.
    let settings = indoc::indoc! {r#"
        exclude_features = ["default"]
        skip_optional_dependencies = true
    "#};

    let combos = feature_sets_for_settings(settings)?;

    let expected: Vec<Vec<&str>> = vec![
        vec![],
        vec!["A"],
        vec!["B"],
        vec!["C"],
        vec!["A", "B"],
        vec!["A", "C"],
        vec!["B", "C"],
        vec!["A", "B", "C"],
    ];

    assert_eq!(combos, as_vec_string(&expected));

    Ok(())
}

#[test]
fn optional_dependency_features_can_be_added_back_via_include_sets() -> eyre::Result<()> {
    let settings = indoc::indoc! {r#"
        exclude_features = ["default"]
        skip_optional_dependencies = true
        include_feature_sets = [["oDepB"], ["C", "A"]]
    "#};

    let combos = feature_sets_for_settings(settings)?;

    // Even though implicit optional dependency features are skipped from the
    // base matrix, they can still be added back explicitly via
    // include_feature_sets.
    assert!(contains_exact_set(&combos, &["oDepB"]));
    assert!(contains_exact_set(&combos, &["A", "C"]));

    Ok(())
}
