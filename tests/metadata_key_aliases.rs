//! Integration tests for metadata key aliases.
//!
//! Verifies that all supported metadata key aliases (`cargo-fc`, `fc`,
//! `feature-combinations`, `cargo-feature-combinations`) are correctly
//! read by `cargo metadata` and produce the expected configuration.

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::Package as _;
use color_eyre::eyre::{self, OptionExt};

fn dummy_crate_with_toml(toml_body: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;

    let cargotoml = temp.child("Cargo.toml");
    cargotoml.write_str(&indoc::formatdoc!(
        r#"
            [package]
            name = "testdummy"
            version = "0.1.0"
            edition = "2024"

            [features]
            foo = []
            bar = []
            baz = []

            {toml_body}
        "#,
        toml_body = toml_body,
    ))?;

    temp.child("src/lib.rs").write_str("pub fn dummy() {}\n")?;

    Ok(temp)
}

fn config_for_toml(toml_body: &str) -> eyre::Result<cargo_feature_combinations::config::Config> {
    let temp = dummy_crate_with_toml(toml_body)?;

    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?;

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "testdummy")
        .ok_or_eyre("test package should exist")?;

    pkg.config()
}

#[test]
fn alias_cargo_feature_combinations() -> eyre::Result<()> {
    let config = config_for_toml(indoc::indoc! {r#"
        [package.metadata.cargo-feature-combinations]
        exclude_features = ["foo"]
    "#})?;

    assert!(config.exclude_features.contains("foo"));
    assert!(!config.exclude_features.contains("bar"));
    Ok(())
}

#[test]
fn alias_cargo_fc() -> eyre::Result<()> {
    let config = config_for_toml(indoc::indoc! {r#"
        [package.metadata.cargo-fc]
        exclude_features = ["bar"]
    "#})?;

    assert!(config.exclude_features.contains("bar"));
    assert!(!config.exclude_features.contains("foo"));
    Ok(())
}

#[test]
fn alias_fc() -> eyre::Result<()> {
    let config = config_for_toml(indoc::indoc! {r#"
        [package.metadata.fc]
        exclude_features = ["baz"]
        no_empty_feature_set = true
    "#})?;

    assert!(config.exclude_features.contains("baz"));
    assert!(config.no_empty_feature_set);
    Ok(())
}

#[test]
fn alias_feature_combinations() -> eyre::Result<()> {
    let config = config_for_toml(indoc::indoc! {r#"
        [package.metadata.feature-combinations]
        exclude_features = ["foo", "bar"]
    "#})?;

    assert!(config.exclude_features.contains("foo"));
    assert!(config.exclude_features.contains("bar"));
    assert!(!config.exclude_features.contains("baz"));
    Ok(())
}

#[test]
fn alias_cargo_fc_with_target_override() -> eyre::Result<()> {
    let config = config_for_toml(indoc::indoc! {r#"
        [package.metadata.cargo-fc]
        exclude_features = ["foo"]

        [package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
        exclude_features = { add = ["bar"] }
    "#})?;

    assert!(config.exclude_features.contains("foo"));
    // Target overrides are stored in the config but not applied until resolve_config
    assert!(config.target.contains_key("cfg(target_os = \"linux\")"));
    Ok(())
}

#[test]
fn alias_fc_with_target_override() -> eyre::Result<()> {
    let config = config_for_toml(indoc::indoc! {r#"
        [package.metadata.fc]
        exclude_features = ["foo"]

        [package.metadata.fc.target.'cfg(target_os = "linux")']
        exclude_features = { add = ["bar"] }
    "#})?;

    assert!(config.exclude_features.contains("foo"));
    assert!(config.target.contains_key("cfg(target_os = \"linux\")"));
    Ok(())
}

#[test]
fn no_metadata_produces_default_config() -> eyre::Result<()> {
    let config = config_for_toml("")?;

    assert!(config.exclude_features.is_empty());
    assert!(config.only_features.is_empty());
    assert!(!config.no_empty_feature_set);
    assert!(!config.skip_optional_dependencies);
    Ok(())
}

#[test]
fn alias_cargo_fc_affects_feature_matrix() -> eyre::Result<()> {
    let temp = dummy_crate_with_toml(indoc::indoc! {r#"
        [package.metadata.cargo-fc]
        exclude_features = ["foo"]
    "#})?;

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

    // "foo" is excluded, so no combination should contain it
    assert!(
        !matrix.iter().any(|s| s.contains("foo")),
        "expected no combination to contain 'foo', got: {matrix:?}"
    );
    // "bar" and "baz" should still appear
    assert!(
        matrix.iter().any(|s| s.contains("bar")),
        "expected 'bar' in at least one combination"
    );
    assert!(
        matrix.iter().any(|s| s.contains("baz")),
        "expected 'baz' in at least one combination"
    );
    Ok(())
}
