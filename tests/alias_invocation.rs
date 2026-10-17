//! End-to-end checks of where each invocation runs and how it selects its
//! package, through the real `cargo-fc` binary and a driver that records its
//! argv instead of building.

#![cfg(unix)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::eyre;
use similar_asserts::assert_eq as sim_assert_eq;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One recorded driver invocation.
#[derive(Debug)]
struct Recorded {
    cwd: PathBuf,
    args: Vec<String>,
}

/// A workspace whose members sit at different depths, a standalone tool
/// package next to it, and a recording driver.
struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> eyre::Result<Self> {
        let temp = TempDir::new()?;
        temp.child("ws/Cargo.toml").write_str(
            "[workspace]\nmembers = [\"crates/a\", \"crates/deep/nested/b\"]\nresolver = \"2\"\n",
        )?;
        for (name, dir) in [("a", "crates/a"), ("b", "crates/deep/nested/b")] {
            temp.child(format!("ws/{dir}/Cargo.toml"))
                .write_str(&format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
                ))?;
            temp.child(format!("ws/{dir}/src/lib.rs")).write_str("")?;
        }
        temp.child("outside").create_dir_all()?;
        temp.child("cargo-home").create_dir_all()?;

        let driver = temp.child("record-driver");
        driver.write_str(
            "#!/bin/sh\n\
             {\n\
             printf 'cwd=%s\\n' \"$(pwd -P)\"\n\
             for arg in \"$@\"; do printf 'arg=%s\\n' \"$arg\"; done\n\
             printf 'end\\n'\n\
             } >> \"$FC_RECORD\"\n",
        )?;
        std::fs::set_permissions(
            driver.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        Ok(Self { temp })
    }

    fn path(&self, relative: &str) -> eyre::Result<PathBuf> {
        Ok(self.temp.path().canonicalize()?.join(relative))
    }

    fn write_alias_config(&self, body: &str) -> eyre::Result<()> {
        self.temp.child("ws/.cargo/config.toml").write_str(body)?;
        Ok(())
    }

    /// Runs `cargo fc --driver <recorder> <args>` from `cwd` and returns the
    /// recorded invocations.
    fn run(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> eyre::Result<Vec<Recorded>> {
        let record = self.temp.child("record.txt");
        if record.path().exists() {
            std::fs::remove_file(record.path())?;
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-fc"));
        command
            .current_dir(cwd)
            .arg("fc")
            .arg("--driver")
            .arg(self.temp.child("record-driver").path())
            .args(args)
            // An empty `$CARGO_HOME` keeps the user's global aliases out.
            .env("CARGO_HOME", self.temp.child("cargo-home").path())
            .env("FC_RECORD", record.path())
            .env_remove("CARGO_ALIAS_TOOL")
            .env_remove("CARGO_ALIAS_LINT")
            .envs(env.iter().copied());
        let output = command.output()?;
        eyre::ensure!(
            output.status.success(),
            "cargo-fc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse_records(&std::fs::read_to_string(record.path())?)
    }
}

fn parse_records(contents: &str) -> eyre::Result<Vec<Recorded>> {
    let mut records = Vec::new();
    let mut cwd = None;
    let mut args = Vec::new();
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("cwd=") {
            cwd = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("arg=") {
            args.push(value.to_string());
        } else if line == "end" {
            let cwd = cwd
                .take()
                .ok_or_else(|| eyre::eyre!("record without cwd"))?;
            records.push(Recorded {
                cwd,
                args: std::mem::take(&mut args),
            });
        }
    }
    Ok(records)
}

/// Regression test for #23: a cwd-relative path in an alias body resolves
/// against the invocation directory for members at every depth.
#[test]
fn relative_alias_path_resolves_from_invocation_directory() -> eyre::Result<()> {
    let fixture = Fixture::new()?;
    fixture
        .write_alias_config("[alias]\ntool = \"run --manifest-path ../tool/Cargo.toml --\"\n")?;
    let ws = fixture.path("ws")?;

    let records = fixture.run(&ws, &[], &["tool"])?;

    // Every row runs from `ws/`, keeps the alias path as written, and hands the
    // wrapped tool the planned member after the alias `--`.
    let expected: Vec<(PathBuf, Vec<String>)> = ["crates/a", "crates/deep/nested/b"]
        .iter()
        .map(|dir| {
            let manifest = ws.join(dir).join("Cargo.toml");
            (
                ws.clone(),
                [
                    "run",
                    "--manifest-path",
                    "../tool/Cargo.toml",
                    "--",
                    "--manifest-path",
                    &manifest.to_string_lossy(),
                    "--no-default-features",
                    "--features=",
                ]
                .map(String::from)
                .to_vec(),
            )
        })
        .collect();
    let actual: Vec<_> = records.into_iter().map(|r| (r.cwd, r.args)).collect();
    sim_assert_eq!(actual, expected);
    Ok(())
}

/// A `CARGO_ALIAS_<NAME>` variable overrides the config-file alias, as in Cargo.
#[test]
fn env_alias_overrides_config_alias() -> eyre::Result<()> {
    let fixture = Fixture::new()?;
    fixture
        .write_alias_config("[alias]\ntool = \"run --manifest-path ../tool/Cargo.toml --\"\n")?;
    let ws = fixture.path("ws")?;

    let records = fixture.run(
        &ws,
        &[(
            "CARGO_ALIAS_TOOL",
            "run --manifest-path /abs/tool/Cargo.toml -- from-env",
        )],
        &["-p", "a", "tool"],
    )?;

    let [record] = records.as_slice() else {
        eyre::bail!("expected one invocation, got {records:?}");
    };
    sim_assert_eq!(
        record.args.get(..5),
        Some(
            [
                "run",
                "--manifest-path",
                "/abs/tool/Cargo.toml",
                "--",
                "from-env"
            ]
            .map(String::from)
            .as_slice()
        )
    );
    Ok(())
}

/// A manifest in a normal alias body locates the workspace, so the planned
/// package is selected by its ID instead of a second, conflicting manifest.
#[test]
fn alias_manifest_selects_package_by_id() -> eyre::Result<()> {
    let fixture = Fixture::new()?;
    fixture.write_alias_config("[alias]\nlocalcheck = \"check --manifest-path Cargo.toml\"\n")?;
    let ws = fixture.path("ws")?;
    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(&ws)
        .no_deps()
        .exec()?;
    let b_id = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "b")
        .map(|package| package.id.repr.clone())
        .ok_or_else(|| eyre::eyre!("package b missing from metadata"))?;

    let records = fixture.run(&ws, &[], &["-p", "b", "localcheck"])?;

    let [record] = records.as_slice() else {
        eyre::bail!("expected one invocation, got {records:?}");
    };
    sim_assert_eq!(
        record.args,
        [
            "check",
            "--manifest-path",
            "Cargo.toml",
            "-p",
            &b_id,
            "--no-default-features",
            "--features=",
        ]
        .map(String::from)
    );
    Ok(())
}

/// From outside the workspace, a wrapper alias's outer `cargo run` gets the
/// manifest cargo-fc was given, and the wrapped command gets the package.
#[test]
fn wrapper_alias_outside_workspace_uses_cargo_fc_manifest() -> eyre::Result<()> {
    let fixture = Fixture::new()?;
    let outside = fixture.path("outside")?;
    let manifest = fixture.path("ws/crates/a/Cargo.toml")?;
    let manifest = manifest.to_string_lossy();

    let records = fixture.run(
        &outside,
        &[("CARGO_ALIAS_LINT", "run --package wrapper -- lint")],
        &["--manifest-path", &manifest, "lint"],
    )?;

    let [record] = records.as_slice() else {
        eyre::bail!("expected one invocation, got {records:?}");
    };
    sim_assert_eq!(record.cwd, outside);
    sim_assert_eq!(
        record.args,
        [
            "run",
            "--package",
            "wrapper",
            "--manifest-path",
            &manifest,
            "--",
            "lint",
            "--manifest-path",
            &manifest,
            "--no-default-features",
            "--features=",
        ]
        .map(String::from)
    );
    Ok(())
}
