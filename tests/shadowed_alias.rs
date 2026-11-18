//! End-to-end checks that a user alias shadowing a built-in short form or
//! `clippy` resolves as in Cargo, through the real `cargo-fc` binary and a
//! driver that records what it was asked to run.

#![cfg(unix)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::eyre;
use indoc::indoc;
use similar_asserts::assert_eq as sim_assert_eq;
use std::process::{Command, Output};

/// A single package that deduplicates diagnostics, with distinct `check`,
/// `clippy`, and `test` policies that mark the child environment, and a driver
/// that records the marker, its command, and whether it was asked for JSON
/// diagnostics.
struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> eyre::Result<Self> {
        let temp = TempDir::new()?;
        temp.child("ws/Cargo.toml").write_str(indoc! {r#"
            [package]
            name = "a"
            version = "0.1.0"
            edition = "2021"

            [package.metadata.cargo-fc]
            dedupe = true

            [package.metadata.cargo-fc.subcommands.check]
            env = { add = { FC_POLICY = "check" } }

            [package.metadata.cargo-fc.subcommands.clippy]
            env = { add = { FC_POLICY = "clippy" } }

            [package.metadata.cargo-fc.subcommands.test]
            env = { add = { FC_POLICY = "test" } }
        "#})?;
        temp.child("ws/src/lib.rs").write_str("")?;
        temp.child("cargo-home").create_dir_all()?;

        let driver = temp.child("record-driver");
        driver.write_str(indoc! {r#"
            #!/bin/sh
            case "$*" in
                *--message-format=*) output=json ;;
                *) output=plain ;;
            esac
            printf '%s %s %s\n' "${FC_POLICY:-none}" "$1" "$output" >> "$FC_RECORD"
        "#})?;
        std::fs::set_permissions(
            driver.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        Ok(Self { temp })
    }

    /// Runs `cargo fc --driver <record-driver> <args>` in the workspace.
    fn output(&self, env: &[(&str, &str)], args: &[&str]) -> eyre::Result<Output> {
        let record = self.temp.child("record.txt");
        if record.path().exists() {
            std::fs::remove_file(record.path())?;
        }
        Ok(Command::new(env!("CARGO_BIN_EXE_cargo-fc"))
            .current_dir(self.temp.child("ws").path())
            .arg("fc")
            .arg("--driver")
            .arg(self.temp.child("record-driver").path())
            .args(args)
            // An empty `$CARGO_HOME` keeps the user's global config out.
            .env("CARGO_HOME", self.temp.child("cargo-home").path())
            .env("FC_RECORD", record.path())
            .env_remove("CARGO_ALIAS_C")
            .env_remove("CARGO_ALIAS_T")
            .envs(env.iter().copied())
            .output()?)
    }

    /// The policy marker and command of each recorded invocation.
    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> eyre::Result<Vec<String>> {
        let output = self.output(env, args)?;
        eyre::ensure!(
            output.status.success(),
            "cargo-fc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let record = std::fs::read_to_string(self.temp.child("record.txt").path())?;
        Ok(record.lines().map(str::to_string).collect())
    }
}

/// Regression test for #35: a short form an alias shadows runs the alias with
/// its command's policy and diagnostics mode, where cargo-fc used to keep `c`
/// and plan `check`.
#[test_util::test]
fn shadowed_short_form_runs_the_alias_with_its_policy() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let clippy = fixture.run(&[("CARGO_ALIAS_C", "clippy")], &["c"])?;
    let test = fixture.run(&[("CARGO_ALIAS_C", "test")], &["c"])?;
    let check = fixture.run(&[("CARGO_ALIAS_C", "check --all-targets")], &["c"])?;
    let plain = fixture.run(&[], &["c"])?;

    // `clippy` is diagnostics-safe, so `dedupe` applies; `test` is not.
    sim_assert_eq!(clippy, ["clippy clippy json"]);
    sim_assert_eq!(test, ["test test plain"]);
    sim_assert_eq!(check, ["check check json"]);
    sim_assert_eq!(plain, ["check c json"]);
    Ok(())
}

/// A shadowing alias to a command cargo-fc has no policy for, such as
/// `t = "nextest run"`, keeps the built-in meaning it had: `t` plans as `test`
/// and reaches Cargo unexpanded, which runs the alias.
#[test_util::test]
fn shadowing_alias_to_an_unknown_command_keeps_the_builtin_meaning() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let t = fixture.run(
        &[("CARGO_ALIAS_T", "nextest run -P default --all-targets")],
        &["t"],
    )?;

    sim_assert_eq!(t, ["test t plain"]);
    Ok(())
}

/// `--env` and `--unset-env` change the alias the child would resolve, so
/// cargo-fc resolves with them too.
#[test_util::test]
fn cli_env_overrides_decide_the_alias() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let set = fixture.run(&[], &["--env", "CARGO_ALIAS_C=test", "c"])?;
    let unset = fixture.run(
        &[("CARGO_ALIAS_C", "test")],
        &["--unset-env", "CARGO_ALIAS_C", "c"],
    )?;

    sim_assert_eq!(set, ["test test plain"]);
    sim_assert_eq!(unset, ["check c json"]);
    Ok(())
}

/// A recursive alias fails before planning, as Cargo fails, even in matrix
/// mode, which never spawns Cargo.
#[test_util::test]
fn recursive_alias_fails_like_cargo() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    for args in [&["c"][..], &["matrix", "c"][..]] {
        let output = fixture.output(&[("CARGO_ALIAS_C", "c --locked")], args)?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr.contains("alias c has unresolvable recursive definition: c -> c"),
            "{args:?}: {stderr}"
        );
    }
    Ok(())
}
