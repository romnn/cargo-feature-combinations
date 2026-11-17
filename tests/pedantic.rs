//! End-to-end checks that Cargo's own warnings, such as manifest lints, count
//! like compiler warnings, through the real `cargo-fc` binary and a driver that
//! prints them instead of building.

#![cfg(unix)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::eyre;
use indoc::{formatdoc, indoc};
use similar_asserts::assert_eq as sim_assert_eq;
use std::process::Command;

const LINT: &str = "warning: `package.homepage` is redundant with `package.repository`";

/// The outcome of one `cargo fc` run against the fake driver.
#[derive(Debug)]
struct Run {
    exit_code: Option<i32>,
    /// The summary rows, one per feature combination that ran.
    summary_rows: Vec<String>,
    /// Everything cargo-fc printed to stderr.
    stderr: String,
}

/// How the fake driver behaves on every run.
#[derive(Clone, Copy, Default)]
struct Driver {
    /// Whether the manifest lint and its summary appear twice, as when a
    /// wrapper and the Cargo it spawns both load the manifest.
    repeated: bool,
    /// A shell `case` pattern over the driver's arguments for the rows that
    /// fail after warning, as Cargo does under `build.warnings = "deny"`.
    denied: Option<&'static str>,
}

/// A package with one feature, so the matrix has two rows, and a driver that
/// prints a Cargo manifest lint on every run.
struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> eyre::Result<Self> {
        Self::with_driver(Driver::default())
    }

    fn with_driver(driver: Driver) -> eyre::Result<Self> {
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

        // Cargo prints its own warnings to stderr as text in every message
        // format, and unless they are denied still exits successfully, so
        // cargo-fc alone decides whether they fail the row.
        let script = temp.child("lint-driver");
        script.write_str(&formatdoc! {r"
            #!/bin/sh
            for _ in {repeats}; do
                cat >&2 <<'EOF'
            {LINT}
             --> Cargo.toml:5:1
              |
            5 | homepage.workspace = true
              | ^^^^^^^^
            warning: `a` (manifest) generated 1 warning
            EOF
            done
            echo '    Checking a v0.1.0 (/ws)' >&2
            {deny}
            echo '    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.01s' >&2
            ",
            repeats = if driver.repeated { "1 2" } else { "1" },
            deny = driver.denied.map_or_else(String::new, |pattern| formatdoc! {r#"
                case "$*" in
                    {pattern})
                        echo 'error: warnings are denied by `build.warnings` configuration' >&2
                        exit 101
                        ;;
                esac
            "#}),
        })?;
        std::fs::set_permissions(
            script.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        Ok(Self { temp })
    }

    /// Runs `cargo fc --driver <lint-driver> <args>` in the workspace.
    fn run(&self, args: &[&str]) -> eyre::Result<Run> {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-fc"))
            .current_dir(self.temp.child("ws").path())
            .arg("fc")
            .arg("--driver")
            .arg(self.temp.child("lint-driver").path())
            .args(args)
            // An empty `$CARGO_HOME` keeps the user's global config out.
            .env("CARGO_HOME", self.temp.child("cargo-home").path())
            .output()?;
        // Color may be forced through the environment, and only the text matters.
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

/// Regression test for #31: a pedantic row fails on a Cargo manifest lint and
/// counts it, in plain and in diagnostics-only output.
#[test_util::test]
fn pedantic_fails_rows_on_cargo_warnings() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    for args in [
        &["--pedantic", "check"][..],
        &["--pedantic", "--diagnostics-only", "check"][..],
    ] {
        let run = fixture.run(args)?;

        // Each row fails on the lint, which every output mode shows.
        sim_assert_eq!(run.exit_code, Some(1), "{args:?}");
        sim_assert_eq!(
            run.summary_rows,
            [
                "FAIL a ( 0 errors, 1 warnings, features = [] )",
                "FAIL a ( 0 errors, 1 warnings, features = [x] )",
            ],
            "{args:?}"
        );
        sim_assert_eq!(run.stderr.matches(LINT).count(), 2, "{args:?}");
    }
    Ok(())
}

/// `--fail-fast` stops at the first row failed by a Cargo warning.
#[test_util::test]
fn pedantic_fail_fast_stops_on_cargo_warnings() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let run = fixture.run(&["--pedantic", "--diagnostics-only", "--fail-fast", "check"])?;

    sim_assert_eq!(run.exit_code, Some(1));
    sim_assert_eq!(
        run.summary_rows,
        ["FAIL a ( 0 errors, 1 warnings, features = [] )"]
    );
    Ok(())
}

/// Without `pedantic`, Cargo's warnings mark rows as warned and still show.
#[test_util::test]
fn cargo_warnings_warn_rows_without_pedantic() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    for args in [&["check"][..], &["--diagnostics-only", "check"][..]] {
        let run = fixture.run(args)?;

        sim_assert_eq!(run.exit_code, Some(0), "{args:?}");
        sim_assert_eq!(
            run.summary_rows,
            [
                "WARN a ( 0 errors, 1 warnings, features = [] )",
                "WARN a ( 0 errors, 1 warnings, features = [x] )",
            ],
            "{args:?}"
        );
        sim_assert_eq!(run.stderr.matches(LINT).count(), 2, "{args:?}");
    }
    Ok(())
}

/// `--dedupe` shows a Cargo warning once and counts its repeats as suppressed,
/// as it does for compiler diagnostics.
#[test_util::test]
fn dedupe_suppresses_repeated_cargo_warnings() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let run = fixture.run(&["--dedupe", "check"])?;

    sim_assert_eq!(run.exit_code, Some(0));
    sim_assert_eq!(
        run.summary_rows,
        [
            "WARN a ( 0 errors, 1 warnings, 0 suppressed, features = [] )",
            "WARN a ( 0 errors, 1 warnings, 1 suppressed, features = [x] )",
        ]
    );
    sim_assert_eq!(run.stderr.matches(LINT).count(), 1);
    Ok(())
}

/// When Cargo fails on its own warning, the failure output explains why while
/// showing each warning once, and `--dedupe` still hides the repeats.
#[test_util::test]
fn denied_cargo_warning_is_shown_once_with_the_denial() -> eyre::Result<()> {
    let fixture = Fixture::with_driver(Driver {
        denied: Some("*"),
        ..Driver::default()
    })?;

    let run = fixture.run(&["--dedupe", "check"])?;

    sim_assert_eq!(run.exit_code, Some(101));
    sim_assert_eq!(
        run.summary_rows,
        [
            "FAIL a ( 0 errors, 1 warnings, 0 suppressed, features = [] )",
            "FAIL a ( 0 errors, 1 warnings, 1 suppressed, features = [x] )",
        ]
    );
    sim_assert_eq!(run.stderr.matches(LINT).count(), 1);
    sim_assert_eq!(run.stderr.matches("warnings are denied").count(), 2);
    Ok(())
}

/// `--dedupe` shows a Cargo warning repeated within one invocation once, as
/// it does for compiler diagnostics, without counting the repeat as
/// suppressed.
/// The count follows Cargo's summary lines, as plain output does.
#[test_util::test]
fn dedupe_folds_cargo_warnings_repeated_within_one_run() -> eyre::Result<()> {
    let fixture = Fixture::with_driver(Driver {
        repeated: true,
        ..Driver::default()
    })?;

    let run = fixture.run(&["--dedupe", "check"])?;

    sim_assert_eq!(
        run.summary_rows,
        [
            "WARN a ( 0 errors, 2 warnings, 0 suppressed, features = [] )",
            "WARN a ( 0 errors, 2 warnings, 1 suppressed, features = [x] )",
        ]
    );
    sim_assert_eq!(run.stderr.matches(LINT).count(), 1);
    Ok(())
}

/// The `--summary-only --fail-fast` dump of a row failed by a Cargo warning
/// shows the warning without Cargo's progress output.
#[test_util::test]
fn summary_only_fail_fast_dumps_cargo_warning_without_chatter() -> eyre::Result<()> {
    let fixture = Fixture::new()?;

    let run = fixture.run(&[
        "--pedantic",
        "--diagnostics-only",
        "--summary-only",
        "--fail-fast",
        "check",
    ])?;

    sim_assert_eq!(run.exit_code, Some(1));
    sim_assert_eq!(run.stderr.matches(LINT).count(), 1);
    assert!(!run.stderr.contains("Checking a"), "{}", run.stderr);
    assert!(
        !run.stderr.contains("generated 1 warning"),
        "{}",
        run.stderr
    );
    Ok(())
}

/// Under `--summary-only`, a Cargo warning hidden on a passing row is not
/// suppressed on a later failing row, whose dump must explain the failure.
#[test_util::test]
fn summary_only_dedupe_explains_a_later_denied_row() -> eyre::Result<()> {
    // Only the `[x]` row fails, after the `[]` row passed with the same lint.
    let fixture = Fixture::with_driver(Driver {
        denied: Some("*--features=a/x*"),
        ..Driver::default()
    })?;

    let run = fixture.run(&["--dedupe", "--summary-only", "--fail-fast", "check"])?;

    sim_assert_eq!(run.exit_code, Some(101));
    sim_assert_eq!(
        run.summary_rows,
        [
            "WARN a ( 0 errors, 1 warnings, features = [] )",
            "FAIL a ( 0 errors, 1 warnings, features = [x] )",
        ]
    );
    sim_assert_eq!(run.stderr.matches(LINT).count(), 1);
    sim_assert_eq!(run.stderr.matches("warnings are denied").count(), 1);
    Ok(())
}
