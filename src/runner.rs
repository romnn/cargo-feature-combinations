//! Cargo command execution, output parsing, summary printing, and matrix output.

use crate::DEFAULT_PKG_METADATA_SECTION;
use crate::cli::{CargoSubcommand, Options, cargo_subcommand};
use crate::package::{FeatureCombinationError, Package};
use crate::print_warning;
use crate::target::TargetTriple;

use color_eyre::eyre;
use itertools::Itertools;
use regex::Regex;
use std::collections::HashSet;
use std::io::{self, Write};
use std::process;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

static CYAN: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Cyan, true));
static RED: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Red, true));
static YELLOW: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Yellow, true));
static GREEN: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Green, true));
static DIMMED: LazyLock<ColorSpec> = LazyLock::new(|| {
    let mut spec = ColorSpec::new();
    spec.set_dimmed(true);
    spec
});

/// An optional process exit code.
///
/// `None` means success (exit 0), `Some(code)` means the process should exit
/// with the given code.
pub type ExitCode = Option<i32>;

/// Build a [`ColorSpec`] with the given foreground color and bold setting.
#[must_use]
pub fn color_spec(color: Color, bold: bool) -> ColorSpec {
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color));
    spec.set_bold(bold);
    spec
}

/// Force colored output on a subprocess.
///
/// Subprocesses see a pipe (not a TTY) on stderr because we capture their
/// output, so most tools auto-disable color. We counteract this in three ways:
///
/// - `CARGO_TERM_COLOR=always` — respected by cargo itself.
/// - `FORCE_COLOR=1` — widely adopted convention (Node.js, Python, Ruby, many
///   Rust crates via `anstream`).
/// - `--color always` is additionally injected into the cargo argument list by
///   the caller for the direct subcommand.
///
/// A more universal fix would be to allocate a pseudo-TTY (e.g. via
/// `portable-pty`) so that `isatty()` returns true in the subprocess, but the
/// env-var approach covers the vast majority of real-world cases.
fn force_color(cmd: &mut process::Command) {
    cmd.env("CARGO_TERM_COLOR", "always");
    cmd.env("FORCE_COLOR", "1");
}

/// Summary of the outcome for running (or pruning) a single feature set.
#[derive(Debug, Clone)]
pub struct Summary {
    package_name: String,
    features: Vec<String>,
    exit_code: Option<i32>,
    pedantic_success: bool,
    num_warnings: usize,
    num_errors: usize,
    num_suppressed: usize,
    /// If this combination was pruned, the features of the equivalent combo.
    equivalent_to: Option<Vec<String>>,
}

impl Summary {
    fn is_pruned(&self) -> bool {
        self.equivalent_to.is_some()
    }
}

/// Extract per-crate warning counts from cargo output.
///
/// The iterator yields the number of warnings for each compiled crate that
/// matches the summary line produced by cargo.
pub fn warning_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    static WARNING_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(
            clippy::expect_used,
            reason = "hard-coded regex pattern is expected to be valid"
        )]
        Regex::new(r"warning: .* generated (\d+) warnings?").expect("valid warning regex")
    });
    WARNING_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(0))
}

/// Extract per-crate error counts from cargo output.
///
/// The iterator yields the number of errors for each compiled crate that
/// matches the summary line produced by cargo.
pub fn error_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    static ERROR_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(
            clippy::expect_used,
            reason = "hard-coded regex pattern is expected to be valid"
        )]
        Regex::new(r"error: could not compile `[^`]*`.*due to\s*(\d*)\s*previous errors?")
            .expect("valid error regex")
    });
    ERROR_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(1))
}

/// Result of processing cargo output for a single feature combination.
pub(crate) struct ProcessResult {
    pub num_warnings: usize,
    pub num_errors: usize,
    pub num_suppressed: usize,
    pub output: Vec<u8>,
}

/// Capture cargo stderr, optionally tee-ing it to the terminal.
///
/// In summary-only mode the output is buffered only; otherwise it is streamed
/// to `stdout` while also being captured for later analysis.
fn capture_stderr(
    child: &mut process::Child,
    summary_only: bool,
    stdout: &mut StandardStream,
) -> io::Result<ProcessResult> {
    let output_buffer = Vec::<u8>::new();
    let mut output_cursor = io::Cursor::new(output_buffer);

    if let Some(proc_stderr) = child.stderr.take() {
        let mut proc_reader = io::BufReader::new(proc_stderr);
        if summary_only {
            io::copy(&mut proc_reader, &mut output_cursor)?;
        } else {
            let mut tee_reader = crate::tee::Reader::new(proc_reader, stdout, true);
            io::copy(&mut tee_reader, &mut output_cursor)?;
        }
    } else {
        eprintln!("ERROR: failed to redirect stderr");
    }

    let stripped = strip_ansi_escapes::strip(output_cursor.get_ref());
    let stripped = String::from_utf8_lossy(&stripped);
    let num_warnings = warning_counts(&stripped).sum::<usize>();
    let num_errors = error_counts(&stripped).sum::<usize>();

    Ok(ProcessResult {
        num_warnings,
        num_errors,
        num_suppressed: 0,
        output: output_cursor.into_inner(),
    })
}

pub(crate) fn print_feature_combination_error(err: &FeatureCombinationError) {
    let mut stderr = StandardStream::stderr(ColorChoice::Auto);

    let _ = stderr.set_color(&RED);
    let _ = write!(&mut stderr, "error");
    let _ = stderr.reset();
    let _ = writeln!(&mut stderr, ": feature matrix generation failed");

    match err {
        FeatureCombinationError::TooManyConfigurations {
            package,
            num_features,
            num_configurations,
            limit,
        } => {
            let _ = stderr.set_color(&YELLOW);
            let _ = writeln!(&mut stderr, "  reason: too many configurations");
            let _ = stderr.reset();

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  package:");
            let _ = stderr.reset();
            let _ = writeln!(&mut stderr, " {package}");

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  features considered:");
            let _ = stderr.reset();
            let _ = writeln!(&mut stderr, " {num_features}");

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  combinations:");
            let _ = stderr.reset();
            let _ = writeln!(
                &mut stderr,
                " {}",
                num_configurations
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unbounded".to_string())
            );

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  limit:");
            let _ = stderr.reset();
            let _ = writeln!(&mut stderr, " {limit}");

            let _ = stderr.set_color(&GREEN);
            let _ = writeln!(&mut stderr, "  hint:");
            let _ = stderr.reset();
            let _ = writeln!(
                &mut stderr,
                "    Consider restricting the matrix using {DEFAULT_PKG_METADATA_SECTION}.only_features",
            );
            let _ = writeln!(
                &mut stderr,
                "    or splitting features into isolated_feature_sets, or excluding features via exclude_features."
            );
        }
    }
}

/// Print an aggregated summary for all executed feature combinations.
///
/// Returns the [`ExitCode`] of the first failing feature combination, or
/// `None` if all combinations succeeded.
///
/// This function is used by [`run_cargo_command_for_target`] after all packages and
/// feature sets have been processed.
#[must_use]
pub fn print_summary(
    summary: &[Summary],
    show_pruned: bool,
    mut stdout: termcolor::StandardStream,
    elapsed: Duration,
) -> ExitCode {
    let num_packages = summary
        .iter()
        .map(|s| &s.package_name)
        .collect::<HashSet<_>>()
        .len();
    let num_total = summary
        .iter()
        .map(|s| (&s.package_name, s.features.iter().collect::<Vec<_>>()))
        .collect::<HashSet<_>>()
        .len();
    let num_pruned = summary.iter().filter(|s| s.is_pruned()).count();
    let num_executed = num_total - num_pruned;

    println!();
    stdout.set_color(&CYAN).ok();
    print!("    Finished ");
    stdout.reset().ok();
    if num_pruned > 0 {
        print!(
            "{num_executed} of {num_total} feature combination{} for {num_packages} package{} in {:.2}s",
            if num_total > 1 { "s" } else { "" },
            if num_packages > 1 { "s" } else { "" },
            elapsed.as_secs_f64(),
        );
        stdout.set_color(&DIMMED).ok();
        print!(" ({num_pruned} pruned)");
        stdout.reset().ok();
    } else {
        print!(
            "{num_total} feature combination{} for {num_packages} package{} in {:.2}s",
            if num_total > 1 { "s" } else { "" },
            if num_packages > 1 { "s" } else { "" },
            elapsed.as_secs_f64(),
        );
    }
    println!();
    println!();

    let max_errors = summary.iter().map(|s| s.num_errors).max().unwrap_or(0);
    let max_warnings = summary.iter().map(|s| s.num_warnings).max().unwrap_or(0);
    let max_suppressed = summary.iter().map(|s| s.num_suppressed).max().unwrap_or(0);
    let show_suppressed = max_suppressed > 0;
    let errors_width = max_errors.to_string().len();
    let warnings_width = max_warnings.to_string().len();
    let suppressed_width = max_suppressed.to_string().len();

    let mut first_bad_exit_code: Option<i32> = None;

    for s in summary {
        if !show_pruned && s.is_pruned() {
            continue;
        }
        let fmt = SummaryFormat {
            show_suppressed,
            errors_width,
            warnings_width,
            suppressed_width,
        };
        print_summary_entry(s, &mut stdout, &fmt);
        if !s.pedantic_success {
            first_bad_exit_code = first_bad_exit_code.or(s.exit_code);
        }
    }
    println!();

    first_bad_exit_code
}

/// Column widths and display flags for summary entry formatting.
struct SummaryFormat {
    show_suppressed: bool,
    errors_width: usize,
    warnings_width: usize,
    suppressed_width: usize,
}

fn print_summary_entry(s: &Summary, stdout: &mut termcolor::StandardStream, fmt: &SummaryFormat) {
    if s.is_pruned() {
        stdout.set_color(&DIMMED).ok();
        print!("        SKIP ");
        stdout.reset().ok();
    } else if !s.pedantic_success {
        stdout.set_color(&RED).ok();
        print!("        FAIL ");
    } else if s.num_warnings > 0 {
        stdout.set_color(&YELLOW).ok();
        print!("        WARN ");
    } else {
        stdout.set_color(&GREEN).ok();
        print!("        PASS ");
    }
    stdout.reset().ok();

    let feat = s.features.iter().join(", ");
    let ew = fmt.errors_width;
    let ww = fmt.warnings_width;
    let sw = fmt.suppressed_width;
    let ne = s.num_errors;
    let nw = s.num_warnings;
    let ns = s.num_suppressed;
    if fmt.show_suppressed {
        print!(
            "{} ( {ne:>ew$} errors, {nw:>ww$} warnings, {ns:>sw$} suppressed, features = [{feat}] )",
            s.package_name,
        );
    } else {
        print!(
            "{} ( {ne:>ew$} errors, {nw:>ww$} warnings, features = [{feat}] )",
            s.package_name,
        );
    }

    if let Some(equiv) = &s.equivalent_to {
        let equiv = equiv.iter().join(", ");
        stdout.set_color(&DIMMED).ok();
        println!(" \u{2190} equivalent to [{equiv}]");
        stdout.reset().ok();
    } else {
        println!();
    }
}

fn print_package_cmd(
    package: &cargo_metadata::Package,
    features: &[&String],
    cargo_args: &[&str],
    all_args: &[&str],
    options: &Options,
    stdout: &mut StandardStream,
) {
    let compact = options.summary_only || options.diagnostics_only;
    if !compact {
        println!();
    }
    let subcommand = cargo_subcommand(cargo_args);
    stdout.set_color(&CYAN).ok();
    match subcommand {
        CargoSubcommand::Test => {
            print!("     Testing ");
        }
        CargoSubcommand::Doc => {
            print!("     Documenting ");
        }
        CargoSubcommand::Check => {
            print!("     Checking ");
        }
        CargoSubcommand::Run => {
            print!("     Running ");
        }
        CargoSubcommand::Build => {
            print!("     Building ");
        }
        CargoSubcommand::Other => {
            print!("     ");
        }
    }
    // For known subcommands, only the verb is colored. For unknown
    // subcommands (Other) we keep cyan for the entire line so the header
    // remains visually distinct.
    if subcommand != CargoSubcommand::Other {
        stdout.reset().ok();
    }
    print!(
        "{} ( features = [{}] )",
        package.name,
        features.as_ref().iter().join(", ")
    );
    if options.verbose {
        print!(" [cargo {}]", all_args.join(" "));
    }
    stdout.reset().ok();
    println!();
    if !compact {
        println!();
    }
}

/// Options for [`print_feature_matrix_for_target`].
#[derive(Debug, Default)]
pub struct MatrixOptions {
    /// Whether to pretty-print the JSON output.
    pub pretty: bool,
    /// Whether to emit only one `"default"` entry per package instead of the
    /// full feature combination matrix.
    pub packages_only: bool,
    /// Whether to disable automatic pruning of implied feature combinations.
    pub no_prune_implied: bool,
}

/// Print a JSON feature matrix for the given packages to stdout.
///
/// The matrix is a JSON array of objects produced from each package's
/// configuration and the feature combinations returned by
/// [`Package::feature_matrix`].
///
/// # Errors
///
/// Returns an error if any configuration can not be parsed or serialization
/// of the JSON matrix fails.
pub fn print_feature_matrix_for_target(
    packages: &[&cargo_metadata::Package],
    target: &TargetTriple,
    evaluator: &mut impl crate::cfg_eval::CfgEvaluator,
    options: &MatrixOptions,
) -> eyre::Result<ExitCode> {
    let per_package_features = packages
        .iter()
        .map(|pkg| {
            let base_config = pkg.config()?;
            let config = crate::config::resolve::resolve_config(&base_config, target, evaluator)?;
            let features = if options.packages_only {
                vec!["default".to_string()]
            } else {
                let combos = pkg.feature_combinations(&config)?;
                let combos = crate::implication::maybe_prune(
                    combos,
                    &pkg.features,
                    &config,
                    options.no_prune_implied,
                );
                combos
                    .keep
                    .into_iter()
                    .map(|combo| combo.into_iter().join(","))
                    .collect()
            };
            Ok::<_, eyre::Report>((pkg.name.clone(), config, features))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let matrix: Vec<serde_json::Value> = per_package_features
        .into_iter()
        .flat_map(|(name, config, features)| {
            features.into_iter().map(move |ft| {
                use serde_json_merge::{iter::dfs::Dfs, merge::Merge};

                let mut out = serde_json::json!(config.matrix);
                out.merge::<Dfs>(&serde_json::json!({
                    "name": name,
                    "features": ft,
                }));
                out
            })
        })
        .collect();

    let matrix = if options.pretty {
        serde_json::to_string_pretty(&matrix)
    } else {
        serde_json::to_string(&matrix)
    }?;
    println!("{matrix}");
    Ok(None)
}

/// Pre-computed state shared across all feature combinations in a single
/// [`run_cargo_command_for_target`] invocation.
struct RunContext<'a> {
    cargo_args: &'a [&'a str],
    extra_args: &'a [&'a str],
    missing_arguments: bool,
    use_diagnostics_only: bool,
    options: &'a Options,
}

/// Result of [`run_single_combination`] for one feature combination.
struct CombinationResult {
    summary: Summary,
    /// Raw (colored) output buffer for potential `--fail-fast` dumping.
    colored_output: Vec<u8>,
}

/// Run a single cargo invocation for one feature combination and collect
/// its output into a [`Summary`].
fn run_single_combination(
    package: &cargo_metadata::Package,
    features: &[&String],
    ctx: &RunContext<'_>,
    seen_diagnostics: &mut HashSet<String>,
    stdout: &mut StandardStream,
) -> eyre::Result<CombinationResult> {
    // We set the command working dir to the package manifest parent dir.
    // This works well for now, but one could also consider `--manifest-path` or `-p`
    let Some(working_dir) = package.manifest_path.parent() else {
        eyre::bail!(
            "could not find parent dir of package {}",
            package.manifest_path.to_string()
        )
    };

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = process::Command::new(&cargo);
    force_color(&mut cmd);

    if ctx.options.errors_only {
        cmd.env(
            "RUSTFLAGS",
            format!(
                "-Awarnings {}", // allows all warnings
                std::env::var("RUSTFLAGS").unwrap_or_default()
            ),
        );
    }

    let mut args = ctx.cargo_args.to_vec();
    if ctx.use_diagnostics_only {
        args.push(crate::diagnostics_only::MESSAGE_FORMAT);
    }
    let features_flag = format!("--features={}", &features.iter().join(","));
    if !ctx.missing_arguments {
        args.push("--no-default-features");
        args.push(&features_flag);
    }
    args.extend_from_slice(ctx.extra_args);
    print_package_cmd(
        package,
        features,
        ctx.cargo_args,
        &args,
        ctx.options,
        stdout,
    );

    cmd.args(&args).current_dir(working_dir);
    if ctx.use_diagnostics_only {
        cmd.stdout(process::Stdio::piped());
    }
    cmd.stderr(process::Stdio::piped());
    let mut child = cmd.spawn()?;

    let mut result = if ctx.use_diagnostics_only {
        crate::diagnostics_only::process_output(
            &mut child,
            ctx.options.summary_only,
            ctx.options.dedupe,
            seen_diagnostics,
            stdout,
        )?
    } else {
        capture_stderr(&mut child, ctx.options.summary_only, stdout)?
    };

    let exit_status = child.wait()?;

    // Print per-combination dedup note after diagnostics
    if result.num_suppressed > 0 && !ctx.options.summary_only {
        stdout.set_color(&CYAN).ok();
        print!("       Note ");
        stdout.reset().ok();
        println!(
            "{} duplicate diagnostic{} suppressed",
            result.num_suppressed,
            if result.num_suppressed > 1 { "s" } else { "" },
        );
    }

    let fail = !exit_status.success();

    // In diagnostics-only mode, cargo-level failures (bad CLI arguments,
    // dependency resolution errors, …) produce no JSON diagnostics — so the
    // user would only see "FAIL … 0 errors, 0 warnings" with no explanation.
    // When that happens the output buffer holds the captured stderr which is
    // the only clue about what went wrong. Print it unconditionally (even in
    // --summary-only mode) so the failure is never silent.
    if ctx.use_diagnostics_only && fail && result.num_errors == 0 && !result.output.is_empty() {
        stdout.write_all(&result.output)?;
        stdout.flush().ok();
        // Clear the buffer so the --fail-fast dump does not print it a
        // second time.
        result.output.clear();
    }

    let pedantic_fail = ctx.options.pedantic && (result.num_errors > 0 || result.num_warnings > 0);

    let summary = Summary {
        features: features.iter().map(|s| (*s).clone()).collect(),
        num_errors: result.num_errors,
        num_warnings: result.num_warnings,
        num_suppressed: result.num_suppressed,
        package_name: package.name.to_string(),
        exit_code: exit_status.code(),
        pedantic_success: !(fail || pedantic_fail),
        equivalent_to: None,
    };

    Ok(CombinationResult {
        summary,
        colored_output: result.output,
    })
}

/// Run a cargo command for all requested packages and feature combinations.
///
/// This is useful for library consumers that want to control target
/// resolution themselves, e.g. when cross-compiling.
///
/// # Errors
///
/// Returns an error if a cargo process can not be spawned or if IO operations
/// fail while reading cargo's output.
pub fn run_cargo_command_for_target(
    packages: &[&cargo_metadata::Package],
    mut cargo_args: Vec<&str>,
    options: &Options,
    target: &TargetTriple,
    evaluator: &mut impl crate::cfg_eval::CfgEvaluator,
) -> eyre::Result<ExitCode> {
    let start = Instant::now();

    // split into cargo and extra arguments after --
    let extra_args_idx = cargo_args
        .iter()
        .position(|arg| *arg == "--")
        .unwrap_or(cargo_args.len());
    let extra_args = cargo_args.split_off(extra_args_idx);

    let missing_arguments = cargo_args.is_empty() && extra_args.is_empty();

    if !cargo_args.contains(&"--color") {
        // force colored output
        cargo_args.extend(["--color", "always"]);
    }

    // Determine whether we can use diagnostics-only (JSON) mode.
    let user_has_message_format = cargo_args.iter().any(|a| a.starts_with("--message-format"));
    let use_diagnostics_only = options.diagnostics_only && !user_has_message_format;

    if options.diagnostics_only && user_has_message_format {
        print_warning!("--diagnostics-only is ignored when --message-format is already specified");
    }

    let ctx = RunContext {
        cargo_args: &cargo_args,
        extra_args: &extra_args,
        missing_arguments,
        use_diagnostics_only,
        options,
    };

    let mut stdout = StandardStream::stdout(ColorChoice::Auto);
    let mut summary: Vec<Summary> = Vec::new();
    let mut seen_diagnostics: HashSet<String> = HashSet::new();

    // show_pruned is a display concern for the global summary, so if any
    // package enables it via config we show pruned entries for all packages.
    let mut show_pruned = options.show_pruned;

    for package in packages {
        let base_config = package.config()?;
        let config = crate::config::resolve::resolve_config(&base_config, target, evaluator)?;
        show_pruned = show_pruned || config.show_pruned;

        let all_combos = package.feature_combinations(&config)?;
        let prune_result = crate::implication::maybe_prune(
            all_combos,
            &package.features,
            &config,
            options.no_prune_implied,
        );

        let pkg_start = summary.len();
        for features in &prune_result.keep {
            let result = run_single_combination(
                package,
                features,
                &ctx,
                &mut seen_diagnostics,
                &mut stdout,
            )?;
            let should_stop = options.fail_fast && !result.summary.pedantic_success;
            let exit_code = result.summary.exit_code;
            summary.push(result.summary);
            if should_stop {
                if options.summary_only {
                    io::copy(&mut io::Cursor::new(result.colored_output), &mut stdout)?;
                    stdout.flush().ok();
                }
                let code = print_summary(&summary, show_pruned, stdout, start.elapsed())
                    .or(exit_code)
                    .unwrap_or(1);
                return Ok(Some(code));
            }
        }

        append_pruned_summaries(
            &mut summary,
            pkg_start,
            package.name.as_ref(),
            prune_result.pruned,
        );
    }

    Ok(print_summary(
        &summary,
        show_pruned,
        stdout,
        start.elapsed(),
    ))
}

/// Append pruned summaries for a single package, looking up the equivalent
/// combo's error/warning counts from already-executed summaries, then sort
/// all entries for this package by features for interleaved display.
fn append_pruned_summaries(
    summary: &mut Vec<Summary>,
    pkg_start: usize,
    package_name: &str,
    pruned: Vec<crate::implication::PrunedCombination>,
) {
    let executed: std::collections::HashMap<Vec<String>, Summary> = summary
        .get(pkg_start..)
        .unwrap_or_default()
        .iter()
        .filter(|s| !s.is_pruned())
        .map(|s| (s.features.clone(), s.clone()))
        .collect();

    for p in pruned {
        let Some(equiv) = executed.get(&p.equivalent_to) else {
            continue;
        };
        summary.push(Summary {
            package_name: package_name.to_string(),
            features: p.features,
            equivalent_to: Some(p.equivalent_to),
            num_errors: equiv.num_errors,
            num_warnings: equiv.num_warnings,
            num_suppressed: equiv.num_suppressed,
            exit_code: None,
            pedantic_success: true,
        });
    }

    if let Some(slice) = summary.get_mut(pkg_start..) {
        slice.sort_by(|a, b| a.features.cmp(&b.features));
    }
}

#[cfg(test)]
mod test {
    use super::{error_counts, warning_counts};
    use similar_asserts::assert_eq as sim_assert_eq;

    #[test]
    fn error_regex_single_mod_multiple_errors() {
        let stderr = include_str!("../test-data/single_mod_multiple_errors_stderr.txt");
        let errors: Vec<_> = error_counts(stderr).collect();
        sim_assert_eq!(&errors, &vec![2]);
    }

    #[test]
    fn error_regex_with_target_kind() {
        let stderr =
            "error: could not compile `docparser-paddleocr-vl` (lib) due to 24 previous errors";
        let errors: Vec<_> = error_counts(stderr).collect();
        sim_assert_eq!(&errors, &vec![24]);
    }

    #[test]
    fn error_regex_with_target_kind_bin() {
        let stderr =
            "error: could not compile `my-crate` (bin \"my-crate\") due to 3 previous errors";
        let errors: Vec<_> = error_counts(stderr).collect();
        sim_assert_eq!(&errors, &vec![3]);
    }

    #[test]
    fn warning_regex_two_mod_multiple_warnings() {
        let stderr = include_str!("../test-data/two_mods_warnings_stderr.txt");
        let warnings: Vec<_> = warning_counts(stderr).collect();
        sim_assert_eq!(&warnings, &vec![6, 7]);
    }
}
