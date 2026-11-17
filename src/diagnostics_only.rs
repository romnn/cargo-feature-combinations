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
/// - When `dedupe` is true:
///   - Re-emissions of the same diagnostic within this invocation are folded
///     into the first copy: not counted, not written, and not tallied as
///     `suppressed`.
///   - Diagnostics already present in `seen` from an earlier invocation are
///     counted and tallied as `suppressed`, but not written.
///
/// `seen` is shared across feature-combination invocations. The per-run set
/// keeps duplicate cargo target emissions from inflating the per-combination
/// warning/error totals or the duplicate-diagnostic note.
fn process_lines(
    reader: impl BufRead,
    writer: &mut impl Write,
    dedupe: bool,
    seen: &mut HashSet<String>,
) -> DiagnosticCounts {
    let mut warnings: usize = 0;
    let mut errors: usize = 0;
    let mut suppressed: usize = 0;
    let mut seen_this_run: Option<HashSet<String>> = dedupe.then(HashSet::new);

    for line in reader.lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<CargoMessage>(&line) {
            Ok(msg) if msg.reason == "compiler-message" => {
                let Some(diag) = msg.message else { continue };

                if let (Some(seen_this_run), Some(rendered)) =
                    (seen_this_run.as_mut(), diag.rendered.as_ref())
                    && !seen_this_run.insert(rendered.clone())
                {
                    continue;
                }

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

/// Cargo's own output on stderr, reduced to what diagnostics-only mode shows.
#[derive(Debug, Default, PartialEq, Eq)]
struct CargoStderr {
    /// The total from Cargo's `generated N warnings` summary lines.
    warnings: usize,
    /// Each diagnostic block as Cargo rendered it, colors included, in order.
    blocks: Vec<StderrBlock>,
}

/// One diagnostic block from Cargo's stderr.
#[derive(Debug, PartialEq, Eq)]
struct StderrBlock {
    /// The block's lines, colors included.
    text: String,
    /// Whether the block is a warning, which `--dedupe` may suppress.
    warning: bool,
}

/// Splits Cargo's stderr into the diagnostics it carries.
///
/// In JSON message-format mode rustc's diagnostics arrive on stdout, while
/// Cargo prints its own warnings and errors, such as manifest lints, to stderr
/// as text.
/// Every line is kept except Cargo's progress chatter: status lines such as
/// `Checking`, the `generated N warnings` and `could not compile` summaries,
/// and the `build failed, waiting for other jobs` notice.
/// A misjudged line is therefore shown rather than hidden.
///
/// The warning count comes from the summary lines alone, the same lines plain
/// output counts.
/// Cargo's unsummarized warnings, such as a build script's `cargo::warning`,
/// are shown but not counted, as in plain output.
fn cargo_stderr(stderr: &[u8]) -> CargoStderr {
    let mut cargo = CargoStderr::default();
    let mut current: Option<StderrBlock> = None;
    for raw in String::from_utf8_lossy(stderr).split_inclusive('\n') {
        let stripped = strip_ansi_escapes::strip_str(raw);
        if let Some(count) = crate::runner::warning_counts(&stripped).next() {
            // Warning summary
            cargo.warnings += count;
            cargo.blocks.extend(current.take());
        } else if is_cargo_chatter(&stripped) {
            cargo.blocks.extend(current.take());
        } else if let Some(warning) = diagnostic_header(&stripped) {
            // Next diagnostic
            cargo.blocks.extend(current.replace(StderrBlock {
                text: raw.to_string(),
                warning,
            }));
        } else if let Some(block) = current.as_mut() {
            // Snippet, note, or help line of the current diagnostic
            block.text.push_str(raw);
        } else {
            // Text outside any diagnostic
            current = Some(StderrBlock {
                text: raw.to_string(),
                warning: false,
            });
        }
    }
    cargo.blocks.extend(current);
    cargo
}

/// Whether `line` is Cargo progress output rather than a diagnostic.
fn is_cargo_chatter(line: &str) -> bool {
    is_status_line(line)
        || crate::runner::error_counts(line).next().is_some()
        || line.starts_with("warning: build failed, waiting for other jobs")
}

/// Whether `line` starts a diagnostic, and if so whether it is a warning, such
/// as `warning: unused manifest key` or `error[E0308]: mismatched types`.
fn diagnostic_header(line: &str) -> Option<bool> {
    let (warning, rest) = if let Some(rest) = line.strip_prefix("warning") {
        (true, rest)
    } else {
        (false, line.strip_prefix("error")?)
    };
    let message = match rest.strip_prefix('[') {
        Some(code) => code.split_once(']')?.1,
        None => rest,
    };
    message.starts_with(':').then_some(warning)
}

/// Whether `line` is a Cargo status line, a known verb right-aligned to
/// column 12 such as `    Checking a v0.1.0` or `   Doc-tests a`.
///
/// Requiring a known verb keeps indented error context such as
/// `  Permission denied (os error 13)` from passing for one.
/// A status Cargo adds later is shown rather than hidden.
fn is_status_line(line: &str) -> bool {
    const STATUS_WIDTH: usize = 12;
    const VERBS: &[&str] = &[
        "Adding",
        "Blocking",
        "Building",
        "Checking",
        "Compiling",
        "Doc-tests",
        "Documenting",
        "Downloaded",
        "Downloading",
        "Executable",
        "Finished",
        "Fresh",
        "Generated",
        "Locking",
        "Running",
        "Scraping",
        "Updating",
        "Waiting",
    ];
    let unindented = line.trim_start_matches(' ');
    let indent = line.len() - unindented.len();
    unindented.split_once(' ').is_some_and(|(verb, _)| {
        indent > 0 && indent + verb.len() == STATUS_WIDTH && VERBS.contains(&verb)
    })
}

/// Process cargo's JSON stdout in diagnostics-only mode.
///
/// Reads JSON lines from the child's stdout, prints rendered diagnostics, and
/// drains stderr in a background thread to prevent pipe deadlocks.
/// Cargo's own diagnostics on stderr are printed after the compiler's, and
/// its summarized warnings are counted; see [`cargo_stderr`].
///
/// When `dedupe` is `true`, diagnostics are deduplicated by exact rendered
/// text. Duplicate target emissions within this cargo invocation are folded,
/// while diagnostics already present in `seen` from earlier invocations are
/// counted as suppressed but not printed.
pub(crate) fn process_output(
    child: &mut process::Child,
    summary_only: bool,
    dedupe: bool,
    seen: &mut HashSet<String>,
    stderr: &mut StandardStream,
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
                counts = process_lines(reader, stderr, dedupe, seen);
                let _ = stderr.flush();
            }
        }

        let Ok(Ok(stderr_buf)) = stderr_handle.join() else {
            return io::Result::Ok(());
        };

        // Cargo's own diagnostics, such as manifest lints, never reach the
        // JSON stream, so they are counted and shown from stderr.
        // Its warnings fold within a run like compiler diagnostics.
        // They are suppressed across runs only when shown live, because a
        // `--summary-only` buffer is printed only for a failing row, which an
        // earlier hidden copy must not leave unexplained.
        let cargo = cargo_stderr(&stderr_buf);
        counts.warnings += cargo.warnings;
        let mut seen_this_run = HashSet::new();
        for block in cargo.blocks {
            if dedupe && block.warning {
                if !seen_this_run.insert(block.text.clone()) {
                    continue;
                }
                if !summary_only && !seen.insert(block.text.clone()) {
                    counts.suppressed += 1;
                    continue;
                }
            }
            if summary_only {
                // Buffered for the failure and `--fail-fast` dumps.
                output_buf.extend(block.text.as_bytes());
            } else {
                stderr.write_all(block.text.as_bytes())?;
            }
        }
        if !summary_only {
            let _ = stderr.flush();
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
    use super::{DiagnosticCounts, cargo_stderr, process_lines};
    use indoc::{formatdoc, indoc};
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

    #[test_util::test]
    fn counts_warnings_and_errors() {
        let input = include_str!("../test-data/diagnostics_only_json_output.txt");
        let mut seen = HashSet::new();
        let (counts, _) = run_lines(input, false, &mut seen);
        sim_assert_eq!(counts.warnings, 2);
        sim_assert_eq!(counts.errors, 1);
        sim_assert_eq!(counts.suppressed, 0);
    }

    #[test_util::test]
    fn rendered_diagnostics_are_written() {
        let input = include_str!("../test-data/diagnostics_only_json_output.txt");
        let mut seen = HashSet::new();
        let (_, written) = run_lines(input, false, &mut seen);
        assert!(written.contains("unused variable"));
        assert!(written.contains("unused import"));
        assert!(written.contains("cannot find value"));
    }

    #[test_util::test]
    fn non_diagnostic_json_is_skipped() {
        let input = include_str!("../test-data/diagnostics_only_json_output.txt");
        let mut seen = HashSet::new();
        let (_, written) = run_lines(input, false, &mut seen);
        assert!(!written.contains("compiler-artifact"));
        assert!(!written.contains("build-finished"));
    }

    #[test_util::test]
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

    #[test_util::test]
    fn mixed_json_and_non_json_lines() {
        let diagnostic = diag_json("warning", "warning: foo\n");
        let artifact = artifact_json();
        let input = formatdoc! {"
            {diagnostic}
            running 1 test
            {artifact}
            test bar ... ok
        "};
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, false, &mut seen);
        sim_assert_eq!(counts.warnings, 1);
        assert!(written.contains("warning: foo"));
        assert!(written.contains("running 1 test"));
        assert!(written.contains("test bar ... ok"));
        assert!(!written.contains("compiler-artifact"));
    }

    #[test_util::test]
    fn duplicate_rendered_text_is_preserved_without_dedupe() {
        let line = diag_json("warning", "warning: duplicate\n");
        let input = formatdoc! {"
            {line}
            {line}
        "};
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, false, &mut seen);
        sim_assert_eq!(counts.warnings, 2);
        sim_assert_eq!(counts.suppressed, 0);
        sim_assert_eq!(written.matches("warning: duplicate").count(), 2);
    }

    #[test_util::test]
    fn dedupe_folds_duplicate_rendered_text_within_one_invocation() {
        // Cargo may emit the same lint once per compiled target in a single
        // invocation. Count and print only one copy; the rest are cargo target
        // fan-out, not additional user diagnostics.
        let line = diag_json("warning", "warning: duplicate\n");
        let input = formatdoc! {"
            {line}
            {line}
            {line}
        "};
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, true, &mut seen);
        sim_assert_eq!(counts.warnings, 1);
        sim_assert_eq!(counts.suppressed, 0);
        sim_assert_eq!(written.matches("warning: duplicate").count(), 1);
    }

    #[test_util::test]
    fn repeated_two_target_diagnostic_reports_zero_then_one_suppressed() {
        // Regression test for --all-targets-style output: a lint emitted twice
        // in each invocation reports no suppressed diagnostics on the first
        // feature combination and one on later combinations.
        let line = diag_json("error", "error: too many lines\n");
        let one_invocation = formatdoc! {"
            {line}
            {line}
        "};
        let mut seen = HashSet::new();

        let (c1, o1) = run_lines(&one_invocation, true, &mut seen);
        sim_assert_eq!(c1.errors, 1);
        sim_assert_eq!(c1.suppressed, 0);
        sim_assert_eq!(o1.matches("too many lines").count(), 1);

        let (c2, o2) = run_lines(&one_invocation, true, &mut seen);
        sim_assert_eq!(c2.errors, 1);
        sim_assert_eq!(c2.suppressed, 1);
        assert!(!o2.contains("too many lines"));
    }

    #[test_util::test]
    fn dedupe_preserves_distinct_diagnostics() {
        let first = diag_json("warning", "warning: first\n");
        let second = diag_json("warning", "warning: second\n");
        let input = formatdoc! {"
            {first}
            {second}
        "};
        let mut seen = HashSet::new();
        let (counts, written) = run_lines(&input, true, &mut seen);
        sim_assert_eq!(counts.warnings, 2);
        sim_assert_eq!(counts.suppressed, 0);
        assert!(written.contains("warning: first"));
        assert!(written.contains("warning: second"));
    }

    #[test_util::test]
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

    #[test_util::test]
    fn empty_input_produces_zero_counts() {
        let mut seen = HashSet::new();
        let (counts, written) = run_lines("", false, &mut seen);
        sim_assert_eq!(counts.warnings, 0);
        sim_assert_eq!(counts.errors, 0);
        sim_assert_eq!(counts.suppressed, 0);
        sim_assert_eq!(written, "");
    }

    #[test_util::test]
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

    #[test_util::test]
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

    /// The text of each block, and whether it is a warning.
    fn blocks(stderr: &str) -> (usize, Vec<(String, bool)>) {
        let cargo = cargo_stderr(stderr.as_bytes());
        let blocks = cargo
            .blocks
            .into_iter()
            .map(|block| (block.text, block.warning))
            .collect();
        (cargo.warnings, blocks)
    }

    /// Cargo 1.100's stderr for `check --message-format=json-…` with a
    /// redundant `homepage` and denied warnings.
    #[test_util::test]
    fn cargo_stderr_keeps_manifest_lints_and_errors_without_chatter() {
        let stderr = include_str!("../test-data/build_warnings_deny_json_stderr.txt");

        let (warnings, blocks) = blocks(stderr);

        // The lint and its snippet form one warning block, and the denial an
        // error block; the summary and status lines are dropped.
        sim_assert_eq!(warnings, 1);
        let [(lint, true), (denial, false)] = blocks.as_slice() else {
            panic!("expected a warning and an error block, got {blocks:?}");
        };
        assert!(lint.starts_with("warning: `package.homepage` is redundant"));
        assert!(lint.ends_with("help: consider removing `package.homepage`\n"));
        sim_assert_eq!(
            denial,
            "error: warnings are denied by `build.warnings` configuration\n"
        );
    }

    /// A wrapper or driver may emit CRLF line endings, which every line keeps
    /// while counting and filtering work as with LF.
    #[test_util::test]
    fn cargo_stderr_handles_crlf_line_endings() {
        let lf = include_str!("../test-data/build_warnings_deny_json_stderr.txt");
        let crlf = lf.replace('\n', "\r\n");

        let (lf_warnings, lf_blocks) = blocks(lf);
        let (crlf_warnings, crlf_blocks) = blocks(&crlf);

        sim_assert_eq!(crlf_warnings, lf_warnings);
        let expected: Vec<_> = lf_blocks
            .into_iter()
            .map(|(text, warning)| (text.replace('\n', "\r\n"), warning))
            .collect();
        sim_assert_eq!(crlf_blocks, expected);
    }

    #[test_util::test]
    fn cargo_stderr_counts_every_summary() {
        let stderr = indoc! {"
            warning: unused field `homepage` in `workspace.package`
             --> Cargo.toml:5:1
            warning: unused field `repository` in `workspace.package`
            warning: workspace (manifest) generated 2 warnings
            warning: a/Cargo.toml: unused manifest key: package.foo
            warning: `a` (manifest) generated 1 warning
                Checking a v0.1.0 (/ws/a)
                Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.02s
        "};

        let (warnings, blocks) = blocks(stderr);

        sim_assert_eq!(warnings, 3);
        sim_assert_eq!(
            blocks,
            [
                (
                    indoc! {"
                        warning: unused field `homepage` in `workspace.package`
                         --> Cargo.toml:5:1
                    "}
                    .to_string(),
                    true
                ),
                (
                    "warning: unused field `repository` in `workspace.package`\n".to_string(),
                    true
                ),
                (
                    "warning: a/Cargo.toml: unused manifest key: package.foo\n".to_string(),
                    true
                ),
            ]
        );
    }

    /// Warnings without a summary line, such as a build script's, are shown but
    /// not counted, as in plain output, wherever they land between other lines.
    #[test_util::test]
    fn cargo_stderr_shows_unsummarized_warnings_without_counting_them() {
        let stderr = indoc! {"
            warning[E0133]: call to unsafe function is unsafe
             --> src/lib.rs:1:1
                Blocking waiting for file lock on build directory
            warning: b@0.1.0: hello from build script
               Compiling b v0.1.0 (/ws/b)
            warning: `a` (lib) generated 1 warning
        "};

        let (warnings, blocks) = blocks(stderr);

        sim_assert_eq!(warnings, 1);
        sim_assert_eq!(
            blocks,
            [
                (
                    indoc! {"
                        warning[E0133]: call to unsafe function is unsafe
                         --> src/lib.rs:1:1
                    "}
                    .to_string(),
                    true
                ),
                (
                    "warning: b@0.1.0: hello from build script\n".to_string(),
                    true
                ),
            ]
        );
    }

    /// A failed compile's summaries are chatter, and a Cargo-level error keeps
    /// its `Caused by` context.
    #[test_util::test]
    fn cargo_stderr_drops_compile_summaries_and_keeps_error_context() {
        let stderr = indoc! {"
            error: `a` (lib) generated 1 warning
            error: could not compile `a` (lib) due to 1 previous error; 1 warning emitted
            warning: build failed, waiting for other jobs to finish...
            error: failed to load manifest for workspace member `/ws/b`

            Caused by:
              unknown variant `bogus`
        "};

        let (warnings, blocks) = blocks(stderr);

        sim_assert_eq!(warnings, 1);
        sim_assert_eq!(
            blocks,
            [(
                indoc! {"
                    error: failed to load manifest for workspace member `/ws/b`

                    Caused by:
                      unknown variant `bogus`
                "}
                .to_string(),
                false
            )]
        );
    }

    /// Indented error context is no status line, however it is capitalized.
    #[test_util::test]
    fn cargo_stderr_keeps_indented_error_context() {
        let stderr = indoc! {"
            error: could not execute process `/nonexistent/rustc -vV` (never executed)

            Caused by:
              No such file or directory (os error 2)
              Permission denied (os error 13)
             Documenting a v0.1.0 (/ws/a)
        "};

        let (_, blocks) = blocks(stderr);

        sim_assert_eq!(
            blocks,
            [(
                indoc! {"
                    error: could not execute process `/nonexistent/rustc -vV` (never executed)

                    Caused by:
                      No such file or directory (os error 2)
                      Permission denied (os error 13)
                "}
                .to_string(),
                false
            )]
        );
    }

    #[test_util::test]
    fn cargo_stderr_strips_colors_only_for_matching() {
        let stderr = "\x1b[1m\x1b[33mwarning\x1b[0m: unused manifest key: package.foo\n\
                      \x1b[1m\x1b[33mwarning\x1b[0m: `a` (manifest) generated 1 warning\n";

        let (warnings, blocks) = blocks(stderr);

        // The summary is recognized through the colors, and the warning is
        // shown with them.
        sim_assert_eq!(warnings, 1);
        sim_assert_eq!(
            blocks,
            [(
                "\x1b[1m\x1b[33mwarning\x1b[0m: unused manifest key: package.foo\n".to_string(),
                true
            )]
        );
    }
}
