//! End-to-end checks that compiler warnings in a forwarded JSON
//! `--message-format` count, through the real `cargo-fc` binary and a driver
//! that prints Cargo's JSON messages instead of building.

#![cfg(unix)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::eyre;
use indoc::indoc;
use similar_asserts::assert_eq as sim_assert_eq;
use std::process::Command;

/// What the driver prints on stdout for a JSON message format: a compiler
/// warning and Cargo's `build-finished`.
const JSON_STDOUT: &str = concat!(
    r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused variable: `x`","spans":[{}],"rendered":"warning: unused variable: `x`\n"}}"#,
    "\n",
    r#"{"reason":"build-finished","success":true}"#,
    "\n",
);

/// The outcome of one `cargo fc` run against the fake driver.
#[derive(Debug)]
struct Run {
    exit_code: Option<i32>,
    stdout: String,
    /// The summary rows, one per feature combination that ran.
    summary_rows: Vec<String>,
}

/// A package with one feature, so the matrix has two rows, and a driver that
/// prints JSON messages when asked for a JSON message format.
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

            [features]
            x = []
        "#})?;
        temp.child("ws/src/lib.rs").write_str("")?;
        temp.child("cargo-home").create_dir_all()?;
        temp.child("json-stdout.txt").write_str(JSON_STDOUT)?;

        // As Cargo does, the driver keeps compiler warnings off stderr in JSON
        // formats and still exits successfully.
        let driver = temp.child("json-driver");
        driver.write_str(indoc! {r#"
            #!/bin/sh
            case "$*" in
                *--message-format=json*) cat "$(dirname "$0")/json-stdout.txt" ;;
            esac
            echo '    Checking a v0.1.0 (/ws)' >&2
        "#})?;
        std::fs::set_permissions(
            driver.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        Ok(Self { temp })
    }

    /// Runs `cargo fc --driver <json-driver> <args>` in the workspace.
    fn run(&self, args: &[&str]) -> eyre::Result<Run> {
        self.run_with_env(&[], args)
    }

    /// Runs `cargo fc --driver <json-driver> <args>` with extra environment.
    fn run_with_env(&self, env: &[(&str, &str)], args: &[&str]) -> eyre::Result<Run> {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-fc"))
            .current_dir(self.temp.child("ws").path())
            .arg("fc")
            .arg("--driver")
            .arg(self.temp.child("json-driver").path())
            .args(args)
            // An empty `$CARGO_HOME` keeps the user's global config out.
            .env("CARGO_HOME", self.temp.child("cargo-home").path())
            .envs(env.iter().copied())
            .output()?;
        let stdout = String::from_utf8(output.stdout)?;
        // Color may be forced through the environment, and only the text of
        // the summary matters.
        let summary_rows = strip_ansi_escapes::strip_str(&stdout)
            .lines()
            .filter(|line| line.contains("features = ["))
            .map(|line| line.trim().to_string())
            .collect();
        Ok(Run {
            exit_code: output.status.code(),
            stdout,
            summary_rows,
        })
    }
}

/// Regression test for #33: warnings in a forwarded JSON stream count, while
/// the stream reaches stdout unchanged and before the summary.
#[test_util::test]
fn forwarded_json_warnings_count_and_pass_through() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let run = fixture.run(&["check", "--message-format=json"])?;

    sim_assert_eq!(run.exit_code, Some(0));
    sim_assert_eq!(
        run.summary_rows,
        [
            "WARN a ( 0 errors, 1 warnings, features = [] )",
            "WARN a ( 0 errors, 1 warnings, features = [x] )",
        ]
    );

    // Each row's stream arrives byte for byte, ahead of the summary table.
    let table = run
        .stdout
        .find("WARN")
        .ok_or_else(|| eyre::eyre!("no summary table in {:?}", run.stdout))?;
    let (streams, _) = run.stdout.split_at(table);
    sim_assert_eq!(streams.matches(JSON_STDOUT).count(), 2);
    Ok(())
}

/// `--pedantic --fail-fast` stops at the first row with a forwarded JSON
/// warning, and `--summary-only` still forwards the stream itself.
#[test_util::test]
fn pedantic_fail_fast_stops_on_forwarded_json_warnings() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let run = fixture.run(&[
        "--pedantic",
        "--fail-fast",
        "--summary-only",
        "check",
        "--message-format=json",
    ])?;

    sim_assert_eq!(run.exit_code, Some(1));
    sim_assert_eq!(
        run.summary_rows,
        ["FAIL a ( 0 errors, 1 warnings, features = [] )"]
    );
    sim_assert_eq!(run.stdout.matches(JSON_STDOUT).count(), 1);
    Ok(())
}

/// Stdout stays inherited, so nothing on it counts, for
/// `json-render-diagnostics`, which leaves compiler diagnostics on stderr, and
/// for `test` and `run`, whose program output may resemble Cargo's messages.
#[test_util::test]
fn stdout_stays_inherited_unless_a_build_only_command_prints_json() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    for args in [
        &["check", "--message-format=json-render-diagnostics"][..],
        &["test", "--message-format=json"][..],
        &["run", "--message-format=json"][..],
    ] {
        let run = fixture.run(args)?;

        sim_assert_eq!(
            run.summary_rows,
            [
                "PASS a ( 0 errors, 0 warnings, features = [] )",
                "PASS a ( 0 errors, 0 warnings, features = [x] )",
            ],
            "{args:?}"
        );
        sim_assert_eq!(run.stdout.matches(JSON_STDOUT).count(), 2, "{args:?}");
    }
    Ok(())
}

/// Short forms and external subcommands such as `clippy` keep stdout
/// inherited, because an alias may shadow them with a command that runs a
/// program, as `CARGO_ALIAS_C=test` does here.
#[test_util::test]
fn shadowable_commands_keep_stdout_inherited() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    for args in [
        &["c", "--message-format=json"][..],
        &["clippy", "--message-format=json"][..],
    ] {
        let run = fixture.run_with_env(&[("CARGO_ALIAS_C", "test")], args)?;

        sim_assert_eq!(
            run.summary_rows,
            [
                "PASS a ( 0 errors, 0 warnings, features = [] )",
                "PASS a ( 0 errors, 0 warnings, features = [x] )",
            ],
            "{args:?}"
        );
        sim_assert_eq!(run.stdout.matches(JSON_STDOUT).count(), 2, "{args:?}");
    }
    Ok(())
}
