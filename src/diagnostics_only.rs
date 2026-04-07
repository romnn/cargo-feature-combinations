//! Diagnostics-only output mode.
//!
//! When `--diagnostics-only` (or `--dedupe`) is active, cargo is invoked with
//! `--message-format=json-diagnostic-rendered-ansi` so that its stdout carries
//! one JSON object per line. This module parses those lines, filters for
//! compiler diagnostics, and prints only their rendered text — suppressing all
//! compilation-progress noise.

use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::process;
use termcolor::StandardStream;

/// Cargo argument to request JSON diagnostics with embedded ANSI color codes.
pub(crate) const MESSAGE_FORMAT: &str = "--message-format=json-diagnostic-rendered-ansi";

/// A cargo JSON message emitted with `--message-format=json`.
///
/// Only the fields needed for diagnostics-only output are deserialized.
#[derive(serde::Deserialize)]
pub(crate) struct CargoMessage {
    pub reason: String,
    #[serde(default)]
    pub message: Option<Diagnostic>,
}

/// A rustc diagnostic embedded in a [`CargoMessage`].
#[derive(serde::Deserialize)]
pub(crate) struct Diagnostic {
    #[serde(default)]
    pub rendered: Option<String>,
    pub level: String,
}

/// Diagnostic counts returned by [`process_lines`].
struct DiagnosticCounts {
    warnings: usize,
    errors: usize,
    suppressed: usize,
}

/// Process a stream of cargo JSON lines, filtering for diagnostics.
///
/// This is the core logic shared by [`process_output`] and tests. It reads
/// lines from `reader`, writes filtered output to `writer`, and returns
/// diagnostic counts.
///
/// - `compiler-message` lines with level `"warning"` or `"error"` are counted
///   and their `rendered` text is written to `writer`.
/// - Non-JSON lines (e.g. test runner output) are passed through to `writer`.
/// - All other JSON messages (artifacts, build-finished) are silently skipped.
/// - When `dedupe` is true, diagnostics already present in `seen` are counted
///   but not written.
fn process_lines(
    reader: impl BufRead,
    writer: &mut impl Write,
    dedupe: bool,
    seen: &mut HashSet<String>,
) -> DiagnosticCounts {
    let mut warnings: usize = 0;
    let mut errors: usize = 0;
    let mut suppressed: usize = 0;

    for line in reader.lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<CargoMessage>(&line) {
            Ok(msg) if msg.reason == "compiler-message" => {
                if let Some(ref diag) = msg.message {
                    match diag.level.as_str() {
                        "warning" => warnings += 1,
                        "error" => errors += 1,
                        _ => {}
                    }
                    if let Some(ref rendered) = diag.rendered {
                        if dedupe && !seen.insert(rendered.clone()) {
                            suppressed += 1;
                        } else {
                            let _ = writer.write_all(rendered.as_bytes());
                        }
                    }
                }
            }
            Ok(_) => {
                // Skip non-diagnostic JSON (artifacts, build-finished, etc.)
            }
            Err(_) => {
                // Non-JSON line (e.g. test runner output) -- pass through
                let _ = writeln!(writer, "{line}");
            }
        }
    }

    DiagnosticCounts {
        warnings,
        errors,
        suppressed,
    }
}

/// Process cargo's JSON stdout in diagnostics-only mode.
///
/// Reads JSON lines from the child's stdout, prints rendered diagnostics, and
/// drains stderr in a background thread to prevent pipe deadlocks.
///
/// When `dedupe` is `true`, diagnostics whose `rendered` text has already been
/// inserted into `seen` are counted but not printed.
pub(crate) fn process_output(
    child: &mut process::Child,
    summary_only: bool,
    dedupe: bool,
    seen: &mut HashSet<String>,
    stdout: &mut StandardStream,
) -> io::Result<crate::runner::ProcessResult> {
    let mut output_buf = Vec::<u8>::new();
    let proc_stderr = child.stderr.take();
    let proc_stdout = child.stdout.take();

    let mut counts = DiagnosticCounts {
        warnings: 0,
        errors: 0,
        suppressed: 0,
    };

    std::thread::scope(|scope| {
        // Drain stderr in background to prevent deadlock.
        // Capture it in case we need to dump on cargo-level failure.
        let stderr_handle = scope.spawn(move || -> io::Result<Vec<u8>> {
            let mut stderr_buf = Vec::new();
            if let Some(stderr) = proc_stderr {
                io::copy(
                    &mut io::BufReader::new(stderr),
                    &mut io::Cursor::new(&mut stderr_buf),
                )?;
            }
            Ok(stderr_buf)
        });

        // Process stdout JSON lines on the main thread.
        if let Some(proc_stdout) = proc_stdout {
            let reader = io::BufReader::new(proc_stdout);
            if summary_only {
                // Buffer everything for potential --fail-fast dump.
                counts = process_lines(reader, &mut output_buf, dedupe, seen);
            } else {
                // Stream diagnostics directly to the terminal.
                counts = process_lines(reader, stdout, dedupe, seen);
                let _ = stdout.flush();
            }
        }

        // Join stderr thread and check for cargo-level failures.
        // If cargo itself failed (e.g. bad Cargo.toml syntax) without emitting
        // any JSON diagnostics, forward the stderr so the user sees the error.
        if let Ok(Ok(stderr_buf)) = stderr_handle.join()
            && counts.errors == 0
            && !stderr_buf.is_empty()
        {
            output_buf.extend(&stderr_buf);
        }

        io::Result::Ok(())
    })?;

    Ok(crate::runner::ProcessResult {
        num_warnings: counts.warnings,
        num_errors: counts.errors,
        num_suppressed: counts.suppressed,
        output: output_buf,
    })
}

#[cfg(test)]
mod test {
    use super::{DiagnosticCounts, process_lines};
    use indoc::indoc;
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::collections::HashSet;

    /// Build a single JSON line for a `compiler-message` with the given level and rendered text.
    #[allow(
        clippy::expect_used,
        reason = "test helper — serialization of a static shape cannot fail"
    )]
    fn diag_json(level: &str, rendered: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "reason": "compiler-message",
            "message": {
                "rendered": rendered,
                "level": level,
            }
        }))
        .expect("serializing diagnostic JSON")
    }

    /// Build a single JSON line for a non-diagnostic cargo message.
    #[allow(
        clippy::expect_used,
        reason = "test helper — serialization of a static shape cannot fail"
    )]
    fn artifact_json() -> String {
        serde_json::to_string(&serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "foo",
            "target": { "kind": ["lib"], "name": "foo" }
        }))
        .expect("serializing artifact JSON")
    }

    /// Helper: run `process_lines` on a string and return the counts + written output.
    fn run_lines(
        input: &str,
        dedupe: bool,
        seen: &mut HashSet<String>,
    ) -> (DiagnosticCounts, String) {
        let reader = std::io::BufReader::new(input.as_bytes());
        let mut writer = Vec::new();
        let counts = process_lines(reader, &mut writer, dedupe, seen);
        let written = String::from_utf8(writer).unwrap_or_default();
        (counts, written)
    }

    #[test]
    fn counts_warnings_and_errors() {
        let input = include_str!("../test-data/diagnostics_only_json_output.txt");
        let mut seen = HashSet::new();
        let (counts, _) = run_lines(input, false, &mut seen);
        sim_assert_eq!(counts.warnings, 2);
        sim_assert_eq!(counts.errors, 1);
        sim_assert_eq!(counts.suppressed, 0);
    }

    #[test]
    fn rendered_diagnostics_are_written() {
        let input = include_str!("../test-data/diagnostics_only_json_output.txt");
        let mut seen = HashSet::new();
        let (_, written) = run_lines(input, false, &mut seen);
        assert!(written.contains("unused variable"));
        assert!(written.contains("unused import"));
        assert!(written.contains("cannot find value"));
    }

    #[test]
    fn non_diagnostic_json_is_skipped() {
        let input = include_str!("../test-data/diagnostics_only_json_output.txt");
        let mut seen = HashSet::new();
        let (_, written) = run_lines(input, false, &mut seen);
        assert!(!written.contains("compiler-artifact"));
        assert!(!written.contains("build-finished"));
    }

    #[test]
    fn non_json_lines_are_passed_through() {
        let input = indoc! {"
            running 5 tests
            test foo::bar ... ok
            test result: ok
        "};
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(input, false, &mut seen);
        assert!(written.contains("running 5 tests"));
        assert!(written.contains("test foo::bar ... ok"));
        assert!(written.contains("test result: ok"));
        sim_assert_eq!(counts.warnings, 0);
        sim_assert_eq!(counts.errors, 0);
    }

    #[test]
    fn mixed_json_and_non_json_lines() {
        let input = format!(
            "{}\nrunning 1 test\n{}\ntest bar ... ok\n",
            diag_json("warning", "warning: foo\n"),
            artifact_json(),
        );
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, false, &mut seen);
        sim_assert_eq!(counts.warnings, 1);
        assert!(written.contains("warning: foo"));
        assert!(written.contains("running 1 test"));
        assert!(written.contains("test bar ... ok"));
        assert!(!written.contains("compiler-artifact"));
    }

    #[test]
    fn dedupe_suppresses_duplicate_diagnostics() {
        let line = diag_json("warning", "warning: duplicate\n");
        let input = format!("{line}\n{line}\n{line}\n");
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, true, &mut seen);
        sim_assert_eq!(counts.warnings, 3);
        sim_assert_eq!(counts.suppressed, 2);
        sim_assert_eq!(written.matches("warning: duplicate").count(), 1);
    }

    #[test]
    fn dedupe_preserves_distinct_diagnostics() {
        let input = format!(
            "{}\n{}\n",
            diag_json("warning", "warning: first\n"),
            diag_json("warning", "warning: second\n"),
        );
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, true, &mut seen);
        sim_assert_eq!(counts.warnings, 2);
        sim_assert_eq!(counts.suppressed, 0);
        assert!(written.contains("warning: first"));
        assert!(written.contains("warning: second"));
    }

    #[test]
    fn dedupe_works_across_multiple_calls() {
        let input = diag_json("warning", "warning: shared\n");
        let mut seen = HashSet::new();

        // First "feature combination"
        let (c1, o1) = run_lines(&input, true, &mut seen);
        sim_assert_eq!(c1.warnings, 1);
        sim_assert_eq!(c1.suppressed, 0);
        assert!(o1.contains("warning: shared"));

        // Second "feature combination" — same diagnostic is suppressed
        let (c2, o2) = run_lines(&input, true, &mut seen);
        sim_assert_eq!(c2.warnings, 1);
        sim_assert_eq!(c2.suppressed, 1);
        assert!(!o2.contains("warning: shared"));
    }

    #[test]
    fn empty_input_produces_zero_counts() {
        let mut seen = HashSet::new();
        let (counts, written) = run_lines("", false, &mut seen);
        sim_assert_eq!(counts.warnings, 0);
        sim_assert_eq!(counts.errors, 0);
        sim_assert_eq!(counts.suppressed, 0);
        assert!(written.is_empty());
    }

    #[test]
    fn cargo_level_error_lines_are_not_silently_swallowed() {
        let input = indoc! {r"
            error: failed to parse manifest at `/tmp/foo/Cargo.toml`

            Caused by:
              duplicate key `dependencies` in table `package`
        "};
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(input, false, &mut seen);
        sim_assert_eq!(counts.warnings, 0);
        sim_assert_eq!(counts.errors, 0);
        assert!(written.contains("failed to parse manifest"));
        assert!(written.contains("duplicate key"));
    }

    #[test]
    fn rendered_text_with_special_characters_survives_roundtrip() {
        let rendered = indoc! {"
            error[E0308]: mismatched types
             --> src/lib.rs:1:1
              |
            1 | fn foo() -> &'static str { 42 }
              |              expected `&str`, found `i32`

        "};
        let input = diag_json("error", rendered);
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, false, &mut seen);
        sim_assert_eq!(counts.errors, 1);
        assert!(written.contains("mismatched types"));
        assert!(written.contains("expected `&str`, found `i32`"));
    }
}
