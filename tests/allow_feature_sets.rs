//! Integration tests for allowlist-style feature-set matrices.

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
            hydrate = []
            ssr = []
            other = []

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

#[test]
fn allow_feature_sets_is_exact_allowlist() -> eyre::Result<()> {
    let settings = indoc::indoc! {r#"
        allow_feature_sets = [["hydrate"], ["ssr"]]
    "#};

    let combos = feature_sets_for_settings(settings)?;

    assert_eq!(combos.len(), 2);
    assert!(contains_exact_set(&combos, &["hydrate"]));
    assert!(contains_exact_set(&combos, &["ssr"]));
    assert!(!contains_exact_set(&combos, &[]));
    assert!(!contains_exact_set(&combos, &["hydrate", "ssr"]));

    Ok(())
}

#[test]
fn allow_feature_sets_drops_non_existent_features() -> eyre::Result<()> {
    let settings = indoc::indoc! {r#"
        allow_feature_sets = [["hydrate", "does-not-exist"], ["ssr"]]
    "#};

    let combos = feature_sets_for_settings(settings)?;

    assert_eq!(combos.len(), 2);
    assert!(contains_exact_set(&combos, &["hydrate"]));
    assert!(contains_exact_set(&combos, &["ssr"]));

    Ok(())
}

#[test]
fn allow_feature_sets_can_be_combined_with_no_empty_feature_set() -> eyre::Result<()> {
    let settings = indoc::indoc! {r#"
        allow_feature_sets = [[], ["hydrate"]]
        no_empty_feature_set = true
    "#};

    let combos = feature_sets_for_settings(settings)?;

    assert_eq!(combos.len(), 1);
    assert!(contains_exact_set(&combos, &["hydrate"]));
    assert!(!contains_exact_set(&combos, &[]));

    Ok(())
}
