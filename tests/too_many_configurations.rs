use assert_fs::prelude::*;
use cargo_feature_combinations::Package as _;
use color_eyre::eyre;
use std::fmt::Write as _;

#[test]
fn too_many_feature_configurations_errors_gracefully() -> eyre::Result<()> {
    let temp = assert_fs::TempDir::new()?;

    let mut features = String::new();
    for i in 0..25 {
        writeln!(features, "f{i} = []")?;
    }

    let crate_toml = indoc::formatdoc! {r#"
        [package]
        name = "example-many-features"
        version = "0.1.0"
        edition = "2021"

        [features]
        {features}
    "#};

    temp.child("Cargo.toml").write_str(&crate_toml)?;
    temp.child("src").create_dir_all()?;
    temp.child("src/lib.rs").write_str("pub fn dummy() {}\n")?;

    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?;

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "example-many-features")
        .expect("test package should exist");

    let config = pkg.config()?;
    let err = pkg
        .feature_matrix(&config)
        .expect_err("expected feature matrix computation to error for too many configurations");

    assert!(
        err.to_string().contains("too many configurations"),
        "unexpected error: {err}"
    );

    Ok(())
}
