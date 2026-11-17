//! End-to-end checks that `--dedupe` suppresses only diagnostics that reached
//! the terminal, through the real `cargo-fc` binary and a driver that prints
//! Cargo's JSON diagnostics instead of building.

#![cfg(unix)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::eyre;
use indoc::indoc;
use similar_asserts::assert_eq as sim_assert_eq;
use std::process::Command;

const WARNING: &str = "warning: unused variable: `shared`";
const ERROR: &str = "error[E0308]: mismatched types";

/// The outcome of one `cargo fc` run against the fake driver.
#[derive(Debug)]
struct Run {
    exit_code: Option<i32>,
    /// The summary rows, one per feature combination that ran.
    summary_rows: Vec<String>,
    stderr: String,
}

/// A workspace whose package `a` is summary-only, with rows `[]` and `[x]`,
/// and whose package `b` prints live, all deduplicated.
///
/// Every row reports the same warning as JSON, and also as Cargo stderr text
/// when `stderr_copy` is set; the `a [x]` row fails, with a JSON error too when
/// `with_error` is set.
struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new(with_error: bool, stderr_copy: bool) -> eyre::Result<Self> {
        let temp = TempDir::new()?;
        temp.child("ws/Cargo.toml").write_str(indoc! {r#"
            [workspace]
            members = ["a", "b"]
            resolver = "2"

            [workspace.metadata.cargo-fc]
            dedupe = true
        "#})?;
        temp.child("ws/a/Cargo.toml").write_str(indoc! {r#"
            [package]
            name = "a"
            version = "0.1.0"
            edition = "2021"

            [features]
            x = []

            [package.metadata.cargo-fc]
            summary_only = true
        "#})?;
        temp.child("ws/b/Cargo.toml").write_str(indoc! {r#"
            [package]
            name = "b"
            version = "0.1.0"
            edition = "2021"
        "#})?;
        for package in ["a", "b"] {
            temp.child(format!("ws/{package}/src/lib.rs"))
                .write_str("")?;
        }
        temp.child("cargo-home").create_dir_all()?;

        let message = |level: &str, rendered: &str| {
            serde_json::json!({
                "reason": "compiler-message",
                "message": { "level": level, "rendered": format!("{rendered}\n") },
            })
            .to_string()
        };
        temp.child("warning.json")
            .write_str(&format!("{}\n", message("warning", WARNING)))?;
        if with_error {
            temp.child("error.json")
                .write_str(&format!("{}\n", message("error", ERROR)))?;
        }
        if stderr_copy {
            temp.child("warning.txt")
                .write_str(&format!("{WARNING}\n"))?;
        }

        let driver = temp.child("diagnostics-driver");
        driver.write_str(indoc! {r#"
            #!/bin/sh
            dir=$(dirname "$0")
            cat "$dir/warning.json"
            if [ -f "$dir/warning.txt" ]; then cat "$dir/warning.txt" >&2; fi
            case "$*" in
                *--features=a/x*)
                    if [ -f "$dir/error.json" ]; then cat "$dir/error.json"; fi
                    exit 101
                    ;;
            esac
        "#})?;
        std::fs::set_permissions(
            driver.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        Ok(Self { temp })
    }

    /// Runs `cargo fc --driver <diagnostics-driver> <args>` in the workspace.
    fn run(&self, args: &[&str]) -> eyre::Result<Run> {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-fc"))
            .current_dir(self.temp.child("ws").path())
            .arg("fc")
            .arg("--driver")
            .arg(self.temp.child("diagnostics-driver").path())
            .args(args)
            // An empty `$CARGO_HOME` keeps the user's global config out.
            .env("CARGO_HOME", self.temp.child("cargo-home").path())
            .output()?;
        // Color may be forced through the environment, and only text matters.
        let stdout = strip_ansi_escapes::strip_str(String::from_utf8(output.stdout)?);
        let summary_rows = stdout
            .lines()
            .filter(|line| line.contains("features = ["))
            .map(|line| line.trim().to_string())
            .collect();
        Ok(Run {
            exit_code: output.status.code(),
            summary_rows,
            stderr: strip_ansi_escapes::strip_str(String::from_utf8(output.stderr)?),
        })
    }
}

/// Regression test for #34: the hidden first row does not suppress the
/// warning in the failing row's dump, and once that dump shows it, the live
/// row suppresses its copy.
///
/// With a second copy on Cargo's stderr, the two streams share one fold, so
/// the copy is neither shown again nor counted as suppressed.
#[test_util::test]
fn hidden_rows_do_not_suppress_and_dumped_rows_do() -> eyre::Result<()> {
    for stderr_copy in [false, true] {
        let fixture = Fixture::new(false, stderr_copy)?;
        assert_hidden_rows_do_not_suppress(&fixture.run(&["check"])?);
    }
    Ok(())
}

fn assert_hidden_rows_do_not_suppress(run: &Run) {
    sim_assert_eq!(run.exit_code, Some(101));
    sim_assert_eq!(
        run.summary_rows,
        [
            "WARN a ( 0 errors, 1 warnings, 0 suppressed, features = [] )",
            "FAIL a ( 0 errors, 1 warnings, 0 suppressed, features = [x] )",
            "WARN b ( 0 errors, 1 warnings, 1 suppressed, features = [] )",
        ]
    );
    // Shown once, by the failing row's dump, then suppressed by the live row.
    sim_assert_eq!(run.stderr.matches(WARNING).count(), 1, "{}", run.stderr);
    sim_assert_eq!(
        run.stderr
            .matches("1 duplicate diagnostic suppressed")
            .count(),
        1,
        "{}",
        run.stderr
    );
}

/// A row that fails on a compiler error stops `--fail-fast` and is dumped
/// there, with the warning an earlier hidden row reported too.
#[test_util::test]
fn fail_fast_dump_keeps_the_warning_of_a_hidden_row() -> eyre::Result<()> {
    let fixture = Fixture::new(true, false)?;

    let run = fixture.run(&["--fail-fast", "check"])?;

    sim_assert_eq!(run.exit_code, Some(101));
    sim_assert_eq!(
        run.summary_rows,
        [
            "WARN a ( 0 errors, 1 warnings, features = [] )",
            "FAIL a ( 1 errors, 1 warnings, features = [x] )",
        ]
    );
    sim_assert_eq!(run.stderr.matches(WARNING).count(), 1, "{}", run.stderr);
    sim_assert_eq!(run.stderr.matches(ERROR).count(), 1, "{}", run.stderr);
    Ok(())
}
