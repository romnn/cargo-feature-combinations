//! Cargo command execution, output parsing, summary printing, and matrix output.

use crate::DEFAULT_PKG_METADATA_SECTION;
use crate::cli::{CargoSubcommand, cargo_subcommand};
use crate::config::ResolvedFlags;
use crate::implication::PrunedCombination;
use crate::invocation_args::{GeneratedArgPlacement, PreparedInvocationArgs};
use crate::package::FeatureCombinationError;
use crate::plan::execution::ExecutionPlanSet;
use crate::print_warning;
use crate::target::{EffectiveTarget, TargetTriple};

use color_eyre::eyre;
use itertools::Itertools;
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
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
fn color_spec(color: Color, bold: bool) -> ColorSpec {
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color));
    spec.set_bold(bold);
    spec
}

/// Force colored output on a subprocess.
///
/// Subprocesses see a pipe (not a TTY) on stderr because we capture their
/// output, so most tools auto-disable color. We counteract this with two env
/// vars:
///
/// - `CARGO_TERM_COLOR=always` — Cargo's documented env var, equivalent to
///   `[term] color = "always"`. Forces colors even when stderr is piped and
///   propagates `--color=always` to rustc. Stable since Rust 1.42.
/// - `FORCE_COLOR=1` — widely adopted convention (Node.js, Python, Ruby, many
///   Rust crates via `anstream`).
///
/// A more universal fix would be to allocate a pseudo-TTY (e.g. via
/// `portable-pty`) so that `isatty()` returns true in the subprocess, but the
/// env-var approach covers the vast majority of real-world cases.
fn force_color(cmd: &mut process::Command) {
    cmd.env("CARGO_TERM_COLOR", "always");
    cmd.env("FORCE_COLOR", "1");
}

fn driver_label(driver: Option<&str>) -> &str {
    driver.unwrap_or("cargo")
}

fn warn_missing_driver(driver: Option<&str>) {
    match driver {
        Some("cargo-zigbuild") => print_warning!(
            "build driver `cargo-zigbuild` was not found; install cargo-zigbuild and zig to cross-compile, or set --driver <bin> / [workspace.metadata.cargo-fc].driver to another driver (use `cargo` to force plain Cargo)"
        ),
        Some(driver) => print_warning!(
            "build driver `{driver}` was not found; install it, or set --driver <bin> / [workspace.metadata.cargo-fc].driver to another driver"
        ),
        None => print_warning!(
            "could not find `cargo`; install Cargo or set the CARGO environment variable"
        ),
    }
}

fn spawn_cargo_command(
    mut cmd: process::Command,
    driver: Option<&str>,
    capture_stdout: bool,
) -> eyre::Result<process::Child> {
    if capture_stdout {
        cmd.stdout(process::Stdio::piped());
    }
    cmd.stderr(process::Stdio::piped());

    match cmd.spawn() {
        Ok(child) => Ok(child),
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                warn_missing_driver(driver);
            }
            Err(eyre::eyre!(
                "failed to invoke build driver `{}`: {err}",
                driver_label(driver),
            ))
        }
    }
}

/// Target display context for a summary entry and command header.
///
/// `Hidden` preserves implicit single-host output, `Single` prints
/// `target = ...` (exact per-target attribution), and `Group` prints
/// `targets = [...]` for an aggregate multi-target invocation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SummaryTarget {
    /// Implicit single-host run: no target field is shown.
    Hidden,
    /// A single concrete target with exact attribution.
    Single(TargetTriple),
    /// An aggregate group of targets sharing one Cargo invocation.
    Group(Vec<TargetTriple>),
}

impl SummaryTarget {
    /// The `target = ...,` / `targets = [...],` field prefix shown inside the
    /// `( ... )` of headers and summary entries, including the trailing
    /// `", "`. Empty for [`SummaryTarget::Hidden`].
    fn field_prefix(&self) -> String {
        match self {
            Self::Hidden => String::new(),
            Self::Single(triple) => format!("target = {triple}, "),
            Self::Group(triples) => format!("targets = [{}], ", triples.iter().join(", ")),
        }
    }
}

/// Summary of the outcome for running (or pruning) a single feature set.
#[derive(Debug, Clone)]
struct Summary {
    package_name: String,
    target: SummaryTarget,
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
fn warning_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
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
fn error_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
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
                num_configurations.map_or_else(|| "unbounded".to_string(), |v| v.to_string())
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
                "    Consider restricting the matrix using [{DEFAULT_PKG_METADATA_SECTION}].only_features",
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
#[must_use]
fn print_summary(
    summary: &[Summary],
    show_pruned: bool,
    stdout: &mut termcolor::StandardStream,
    elapsed: Duration,
) -> ExitCode {
    let num_packages = summary
        .iter()
        .map(|s| &s.package_name)
        .collect::<HashSet<_>>()
        .len();
    // Key executed/pruned combinations by (package, target, features) so that
    // identical feature sets across targets do not collapse.
    let num_total = summary
        .iter()
        .map(|s| {
            (
                &s.package_name,
                &s.target,
                s.features.iter().collect::<Vec<_>>(),
            )
        })
        .collect::<HashSet<_>>()
        .len();
    let num_pruned = summary.iter().filter(|s| s.is_pruned()).count();
    let num_executed = num_total - num_pruned;

    let mut target_set: HashSet<&TargetTriple> = HashSet::new();
    for s in summary {
        match &s.target {
            SummaryTarget::Hidden => {}
            SummaryTarget::Single(triple) => {
                target_set.insert(triple);
            }
            SummaryTarget::Group(triples) => {
                target_set.extend(triples.iter());
            }
        }
    }
    let num_targets = target_set.len();
    let targets_clause = if num_targets > 1 {
        format!(" across {num_targets} targets")
    } else {
        String::new()
    };

    println!();
    stdout.set_color(&CYAN).ok();
    print!("    Finished ");
    stdout.reset().ok();
    if num_pruned > 0 {
        print!(
            "{num_executed} of {num_total} feature combination{} for {num_packages} package{}{targets_clause} in {:.2}s",
            if num_total > 1 { "s" } else { "" },
            if num_packages > 1 { "s" } else { "" },
            elapsed.as_secs_f64(),
        );
        stdout.set_color(&DIMMED).ok();
        print!(" ({num_pruned} pruned)");
        stdout.reset().ok();
    } else {
        print!(
            "{num_total} feature combination{} for {num_packages} package{}{targets_clause} in {:.2}s",
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
        print_summary_entry(s, stdout, &fmt);
        if !s.pedantic_success {
            let exit_code = match s.exit_code {
                Some(code) if code != 0 => code,
                _ => 1,
            };
            first_bad_exit_code = first_bad_exit_code.or(Some(exit_code));
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
    let target = s.target.field_prefix();
    let ew = fmt.errors_width;
    let ww = fmt.warnings_width;
    let sw = fmt.suppressed_width;
    let ne = s.num_errors;
    let nw = s.num_warnings;
    let ns = s.num_suppressed;
    if fmt.show_suppressed {
        print!(
            "{} ( {target}{ne:>ew$} errors, {nw:>ww$} warnings, {ns:>sw$} suppressed, features = [{feat}] )",
            s.package_name,
        );
    } else {
        print!(
            "{} ( {target}{ne:>ew$} errors, {nw:>ww$} warnings, features = [{feat}] )",
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

/// Position of a feature combination within the overall run.
#[derive(Clone, Copy)]
struct Progress {
    index: usize,
    total: usize,
    width: usize,
}

/// The per-combination inputs for one Cargo invocation.
struct Invocation<'a> {
    package: &'a cargo_metadata::Package,
    features: &'a [String],
    /// Fully resolved cargo-fc flags for this package-target invocation.
    flags: ResolvedFlags,
    /// Target triples cargo-fc must inject as `--target` (configured sources).
    inject_targets: &'a [String],
    /// Display/attribution context for the header and summary entry.
    summary_target: &'a SummaryTarget,
}

/// One aggregate-mode Cargo invocation after transposing target plans by package
/// and feature combination.
struct AggregateInvocationPlan<'a> {
    package: &'a cargo_metadata::Package,
    combo: Vec<String>,
    flags: ResolvedFlags,
    targets: Vec<EffectiveTarget>,
}

fn print_package_cmd(
    inv: &Invocation<'_>,
    all_args: &[&str],
    diagnostics_only: bool,
    driver: Option<&str>,
    progress: Progress,
    stdout: &mut StandardStream,
) {
    let compact = inv.flags.summary_only || diagnostics_only;
    if !compact {
        println!();
    }
    let subcommand = cargo_subcommand(all_args);
    stdout.set_color(&CYAN).ok();
    match subcommand {
        CargoSubcommand::Test => {
            print!("     Testing ");
        }
        CargoSubcommand::Doc => {
            print!("     Documenting ");
        }
        CargoSubcommand::Lint => {
            print!("     Linting ");
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
    // The progress counter sits immediately to the left of the package name.
    // It is always dimmed; for known subcommands only the verb is cyan, while
    // for unknown subcommands (Other) the rest of the line stays cyan so the
    // header remains visually distinct.
    stdout.set_color(&DIMMED).ok();
    print!(
        "[{idx:>width$}/{total}]",
        idx = progress.index,
        total = progress.total,
        width = progress.width,
    );
    if subcommand == CargoSubcommand::Other {
        stdout.set_color(&CYAN).ok();
    } else {
        stdout.reset().ok();
    }
    print!(
        " {} ( {}features = [{}] )",
        inv.package.name,
        inv.summary_target.field_prefix(),
        inv.features.iter().join(", ")
    );
    if inv.flags.verbose {
        print!(" [{} {}]", driver_label(driver), all_args.join(" "));
    }
    stdout.reset().ok();
    println!();
    if !compact {
        println!();
    }
}

/// Pre-computed state shared across all feature combinations in one execution.
struct RunContext<'a> {
    invocation_args: &'a PreparedInvocationArgs<'a>,
    /// Build driver to invoke instead of `$CARGO`/`cargo` (e.g. `cargo-zigbuild`).
    driver: Option<&'a str>,
}

/// Result of [`run_single_combination`] for one feature combination.
struct CombinationResult {
    summary: Summary,
    /// Raw (colored) output buffer for potential `--fail-fast` dumping.
    colored_output: Vec<u8>,
    flags: ResolvedFlags,
}

/// Run a single cargo invocation for one feature combination and collect
/// its output into a [`Summary`].
fn run_single_combination(
    inv: &Invocation<'_>,
    ctx: &RunContext<'_>,
    progress: Progress,
    seen_diagnostics: &mut HashSet<String>,
    stdout: &mut StandardStream,
) -> eyre::Result<CombinationResult> {
    let package = inv.package;
    let features = inv.features;
    let mut diagnostics_only = inv.flags.diagnostics_only;
    let mut dedupe = inv.flags.dedupe;
    if ctx
        .invocation_args
        .has_message_format_arg_for_generated_args()
    {
        // `--message-format` is a forwarded Cargo argument, so it wins at
        // execution time instead of becoming part of cargo-fc config
        // resolution.
        diagnostics_only = false;
        dedupe = false;
    }
    // We set the command working dir to the package manifest parent dir.
    // This works well for now, but one could also consider `--manifest-path` or `-p`
    let Some(working_dir) = package.manifest_path.parent() else {
        eyre::bail!(
            "could not find parent dir of package {}",
            package.manifest_path.to_string()
        )
    };

    let cargo: std::ffi::OsString = match ctx.driver {
        Some(driver) => std::ffi::OsString::from(driver),
        None => std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
    };
    let mut cmd = process::Command::new(&cargo);
    force_color(&mut cmd);

    if inv.flags.errors_only {
        cmd.env(
            "RUSTFLAGS",
            format!(
                "-Awarnings {}", // allows all warnings
                std::env::var("RUSTFLAGS").unwrap_or_default()
            ),
        );
    }

    let features_flag = format!("--features={}", features.iter().join(","));
    let mut generated_args = Vec::new();
    if diagnostics_only {
        generated_args.push(crate::diagnostics_only::MESSAGE_FORMAT);
    }
    for triple in inv.inject_targets {
        generated_args.push("--target");
        generated_args.push(triple.as_str());
    }
    if !ctx.invocation_args.is_missing_command() {
        generated_args.push("--no-default-features");
        generated_args.push(&features_flag);
    }
    let args = ctx.invocation_args.with_generated_args(generated_args);
    print_package_cmd(inv, &args, diagnostics_only, ctx.driver, progress, stdout);

    cmd.args(&args).current_dir(working_dir);
    let mut child = spawn_cargo_command(cmd, ctx.driver, diagnostics_only)?;

    let mut result = if diagnostics_only {
        crate::diagnostics_only::process_output(
            &mut child,
            inv.flags.summary_only,
            dedupe,
            seen_diagnostics,
            stdout,
        )?
    } else {
        capture_stderr(&mut child, inv.flags.summary_only, stdout)?
    };

    let exit_status = child.wait()?;

    // Print per-combination dedup note after diagnostics
    if result.num_suppressed > 0 && !inv.flags.summary_only {
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
    if diagnostics_only && fail && result.num_errors == 0 && !result.output.is_empty() {
        stdout.write_all(&result.output)?;
        stdout.flush().ok();
        // Clear the buffer so the --fail-fast dump does not print it a
        // second time.
        result.output.clear();
    }

    let pedantic_fail = inv.flags.pedantic && (result.num_errors > 0 || result.num_warnings > 0);

    let summary = Summary {
        features: features.to_vec(),
        target: inv.summary_target.clone(),
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
        flags: inv.flags,
    })
}

/// Execution mode over the same execution plans.
///
/// Both modes are single-threaded and stream live output; they differ only in
/// how targets map onto Cargo invocations and how summary entries are keyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetExecutionMode {
    /// Default: one invocation per `(package, target, combo)`, exact per-target
    /// attribution.
    SerialPerTarget,
    /// `--aggregate-targets`: one invocation per `(package, combo)` carrying all
    /// that combo's targets as repeated `--target` flags, group-level
    /// attribution.
    Aggregate,
}

/// Set up shared Cargo invocation context and run the execution plans in the
/// given mode.
///
/// # Errors
///
/// Returns an error if a cargo process can not be spawned or if IO operations
/// fail while reading cargo's output.
pub fn run_execution_plans(
    plan_set: &ExecutionPlanSet,
    cargo_args: Vec<&str>,
    mode: TargetExecutionMode,
    driver: Option<&str>,
    generated_arg_placement: GeneratedArgPlacement,
) -> eyre::Result<ExitCode> {
    let start = Instant::now();

    let invocation_args = PreparedInvocationArgs::new(cargo_args, generated_arg_placement);

    let removed_feature_args = invocation_args.removed_feature_args();
    if !removed_feature_args.is_empty() {
        let flag_label = if removed_feature_args.len() == 1 {
            "flag"
        } else {
            "flags"
        };
        print_warning!(
            "ignoring cargo feature-selection {flag_label} incompatible with feature matrix: {}",
            removed_feature_args.iter().join(" ")
        );
    } else if invocation_args.preserved_feature_selection_for_unknown_command() {
        print_warning!(
            "leaving cargo feature-selection flags unchanged for unresolved cargo alias/custom subcommand"
        );
    }

    let wants_diagnostics = plan_set.plans.iter().any(|plan| {
        plan.package_plans
            .iter()
            .any(|package_plan| package_plan.flags.diagnostics_only)
    });
    if wants_diagnostics && invocation_args.has_message_format_arg_for_generated_args() {
        print_warning!("--diagnostics-only is ignored when --message-format is already specified");
    }

    let ctx = RunContext {
        invocation_args: &invocation_args,
        driver,
    };

    let mut stdout = StandardStream::stdout(ColorChoice::Auto);
    let mut seen_diagnostics: HashSet<String> = HashSet::new();

    match mode {
        TargetExecutionMode::SerialPerTarget => {
            execute_serial(plan_set, &ctx, &mut seen_diagnostics, &mut stdout, start)
        }
        TargetExecutionMode::Aggregate => {
            execute_aggregate(plan_set, &ctx, &mut seen_diagnostics, &mut stdout, start)
        }
    }
}

/// Serial per-target execution: one Cargo invocation per
/// `(package, target, combo)`.
fn execute_serial(
    plan_set: &ExecutionPlanSet,
    ctx: &RunContext<'_>,
    seen_diagnostics: &mut HashSet<String>,
    stdout: &mut StandardStream,
    start: Instant,
) -> eyre::Result<ExitCode> {
    let mut summary: Vec<Summary> = Vec::new();
    let total: usize = plan_set
        .plans
        .iter()
        .flat_map(|plan| plan.package_plans.iter())
        .map(|pp| pp.combinations.len())
        .sum();
    let width = total.to_string().len();
    let mut index = 0;

    for plan in &plan_set.plans {
        for pp in &plan.package_plans {
            let summary_target = if plan_set.show_target {
                SummaryTarget::Single(plan.target.clone())
            } else {
                SummaryTarget::Hidden
            };
            let inject: Vec<String> = if pp.target.source.should_inject_target_arg() {
                vec![pp.target.triple.0.clone()]
            } else {
                Vec::new()
            };

            let pkg_start = summary.len();
            for combo in &pp.combinations {
                index += 1;
                let result = run_single_combination(
                    &Invocation {
                        package: pp.package,
                        features: combo,
                        flags: pp.flags,
                        inject_targets: &inject,
                        summary_target: &summary_target,
                    },
                    ctx,
                    Progress {
                        index,
                        total,
                        width,
                    },
                    seen_diagnostics,
                    stdout,
                )?;
                if let Some(code) = record_result_and_maybe_stop(
                    &mut summary,
                    result,
                    plan_set.show_pruned,
                    ctx,
                    stdout,
                    start,
                )? {
                    return Ok(Some(code));
                }
            }

            append_pruned_summaries(
                &mut summary,
                pkg_start,
                pp.package.name.as_ref(),
                &summary_target,
                pp.pruned.clone(),
            );
        }
    }

    Ok(print_summary(
        &summary,
        plan_set.show_pruned,
        stdout,
        start.elapsed(),
    ))
}

/// Aggregate execution: one Cargo invocation per `(package, combo)` carrying
/// every target that shares the combo as repeated `--target` flags.
fn execute_aggregate(
    plan_set: &ExecutionPlanSet,
    ctx: &RunContext<'_>,
    seen_diagnostics: &mut HashSet<String>,
    stdout: &mut StandardStream,
    start: Instant,
) -> eyre::Result<ExitCode> {
    let invocations = aggregate_invocation_plans(plan_set);
    let total = invocations.len();
    let width = total.to_string().len();
    let mut summary: Vec<Summary> = Vec::new();

    for (zero_index, inv_plan) in invocations.iter().enumerate() {
        let index = zero_index + 1;
        let triples: Vec<TargetTriple> =
            inv_plan.targets.iter().map(|t| t.triple.clone()).collect();
        let summary_target = match triples.as_slice() {
            [single] => SummaryTarget::Single(single.clone()),
            _ => SummaryTarget::Group(triples.clone()),
        };
        let inject: Vec<String> = inv_plan
            .targets
            .iter()
            .filter(|t| t.source.should_inject_target_arg())
            .map(|t| t.triple.0.clone())
            .collect();

        let result = run_single_combination(
            &Invocation {
                package: inv_plan.package,
                features: &inv_plan.combo,
                flags: inv_plan.flags,
                inject_targets: &inject,
                summary_target: &summary_target,
            },
            ctx,
            Progress {
                index,
                total,
                width,
            },
            seen_diagnostics,
            stdout,
        )?;
        if let Some(code) = record_result_and_maybe_stop(
            &mut summary,
            result,
            plan_set.show_pruned,
            ctx,
            stdout,
            start,
        )? {
            return Ok(Some(code));
        }
    }

    Ok(print_summary(
        &summary,
        plan_set.show_pruned,
        stdout,
        start.elapsed(),
    ))
}

fn record_result_and_maybe_stop(
    summary: &mut Vec<Summary>,
    result: CombinationResult,
    show_pruned: bool,
    _ctx: &RunContext<'_>,
    stdout: &mut StandardStream,
    start: Instant,
) -> eyre::Result<ExitCode> {
    let CombinationResult {
        summary: result_summary,
        colored_output,
        flags,
    } = result;
    let should_stop = flags.fail_fast && !result_summary.pedantic_success;
    let exit_code = result_summary.exit_code;
    summary.push(result_summary);

    if !should_stop {
        return Ok(None);
    }

    if flags.summary_only {
        io::copy(&mut io::Cursor::new(colored_output), stdout)?;
        stdout.flush().ok();
    }
    Ok(Some(
        print_summary(summary, show_pruned, stdout, start.elapsed())
            .or(exit_code)
            .unwrap_or(1),
    ))
}

/// Transpose per-target execution plans into aggregate-mode invocations.
///
/// The resulting order is package first-appearance order, sorted canonical combo
/// order, then target-plan order within each combo's target group.
fn aggregate_invocation_plans<'a>(
    plan_set: &'a ExecutionPlanSet<'a>,
) -> Vec<AggregateInvocationPlan<'a>> {
    let mut package_order: Vec<&cargo_metadata::Package> = Vec::new();
    let mut seen_packages: HashSet<String> = HashSet::new();
    let mut grouped: HashMap<String, BTreeMap<AggregateKey, Vec<EffectiveTarget>>> = HashMap::new();

    for plan in &plan_set.plans {
        for pp in &plan.package_plans {
            let id = pp.package.id.repr.clone();
            if seen_packages.insert(id.clone()) {
                package_order.push(pp.package);
            }
            let entry = grouped.entry(id).or_default();
            for combo in &pp.combinations {
                entry
                    .entry(AggregateKey {
                        combo: combo.clone(),
                        flags: pp.flags,
                    })
                    .or_default()
                    .push(pp.target.clone());
            }
        }
    }

    let mut invocations = Vec::new();
    for package in package_order {
        let Some(combos) = grouped.remove(&package.id.repr) else {
            continue;
        };
        for (key, targets) in combos {
            invocations.push(AggregateInvocationPlan {
                package,
                combo: key.combo,
                flags: key.flags,
                targets,
            });
        }
    }

    invocations
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AggregateKey {
    combo: Vec<String>,
    flags: ResolvedFlags,
}

/// Append pruned summaries for a single `(package, target)` block, looking up
/// the equivalent combo's error/warning counts from already-executed summaries
/// scoped to that block, then sort the block by features for interleaved
/// display.
fn append_pruned_summaries(
    summary: &mut Vec<Summary>,
    pkg_start: usize,
    package_name: &str,
    summary_target: &SummaryTarget,
    pruned: Vec<PrunedCombination>,
) {
    let executed: HashMap<Vec<String>, Summary> = summary
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
            target: summary_target.clone(),
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
    use super::{
        Summary, SummaryTarget, aggregate_invocation_plans, error_counts, print_summary,
        warning_counts,
    };
    use crate::config::ResolvedFlags;
    use crate::package::test::{effective_target, package};
    use crate::plan::execution::{ExecutionPlan, ExecutionPlanSet, PackageExecutionPlan};
    use crate::target::TargetTriple;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn string_vec(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(String::from).collect()
    }

    fn summary_with_failure(exit_code: Option<i32>, pedantic_success: bool) -> Summary {
        Summary {
            package_name: "pkg".to_string(),
            target: SummaryTarget::Hidden,
            features: Vec::new(),
            exit_code,
            pedantic_success,
            num_warnings: usize::from(!pedantic_success),
            num_errors: 0,
            num_suppressed: 0,
            equivalent_to: None,
        }
    }

    fn package_plan<'a>(
        package: &'a cargo_metadata::Package,
        target: &str,
        combinations: Vec<Vec<String>>,
        flags: ResolvedFlags,
    ) -> PackageExecutionPlan<'a> {
        PackageExecutionPlan {
            package,
            target: effective_target(target),
            combinations,
            pruned: Vec::new(),
            matrix: serde_json::Map::new(),
            flags,
            ignored_diagnostics_config: false,
        }
    }

    #[test]
    fn summary_target_field_prefix() {
        sim_assert_eq!(SummaryTarget::Hidden.field_prefix(), "");
        sim_assert_eq!(
            SummaryTarget::Single(TargetTriple("t-a".to_string())).field_prefix(),
            "target = t-a, "
        );
        sim_assert_eq!(
            SummaryTarget::Group(vec![
                TargetTriple("t-a".to_string()),
                TargetTriple("t-b".to_string()),
            ])
            .field_prefix(),
            "targets = [t-a, t-b], "
        );
    }

    #[test]
    fn print_summary_returns_one_for_pedantic_warning_exit_zero() {
        let summary = vec![summary_with_failure(Some(0), false)];
        let mut stdout = termcolor::StandardStream::stdout(termcolor::ColorChoice::Never);

        let exit = print_summary(&summary, false, &mut stdout, std::time::Duration::ZERO);

        sim_assert_eq!(exit, Some(1));
    }

    #[test]
    fn print_summary_returns_one_for_failure_without_exit_code() {
        let summary = vec![summary_with_failure(None, false)];
        let mut stdout = termcolor::StandardStream::stdout(termcolor::ColorChoice::Never);

        let exit = print_summary(&summary, false, &mut stdout, std::time::Duration::ZERO);

        sim_assert_eq!(exit, Some(1));
    }

    #[test]
    fn print_summary_preserves_nonzero_failure_exit_code() {
        let summary = vec![summary_with_failure(Some(101), false)];
        let mut stdout = termcolor::StandardStream::stdout(termcolor::ColorChoice::Never);

        let exit = print_summary(&summary, false, &mut stdout, std::time::Duration::ZERO);

        sim_assert_eq!(exit, Some(101));
    }

    #[test]
    fn aggregate_invocation_plans_group_by_package_combo_and_target_order() -> eyre::Result<()> {
        let package_a = package("a")?;
        let package_b = package("b")?;
        let plan_set = ExecutionPlanSet {
            plans: vec![
                ExecutionPlan {
                    target: TargetTriple("t1".to_string()),
                    package_plans: vec![
                        package_plan(
                            &package_a,
                            "t1",
                            vec![string_vec(&["b"]), string_vec(&[])],
                            ResolvedFlags::default(),
                        ),
                        package_plan(
                            &package_b,
                            "t1",
                            vec![string_vec(&["z"])],
                            ResolvedFlags::default(),
                        ),
                    ],
                },
                ExecutionPlan {
                    target: TargetTriple("t2".to_string()),
                    package_plans: vec![
                        package_plan(
                            &package_a,
                            "t2",
                            vec![string_vec(&[]), string_vec(&["a"])],
                            ResolvedFlags::default(),
                        ),
                        package_plan(
                            &package_b,
                            "t2",
                            vec![string_vec(&["z"])],
                            ResolvedFlags::default(),
                        ),
                    ],
                },
            ],
            show_pruned: false,
            show_target: true,
        };

        let simplified: Vec<_> = aggregate_invocation_plans(&plan_set)
            .into_iter()
            .map(|inv| {
                (
                    inv.package.name.to_string(),
                    inv.combo,
                    inv.targets
                        .into_iter()
                        .map(|target| target.triple.0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();

        sim_assert_eq!(
            simplified,
            vec![
                (
                    "a".to_string(),
                    string_vec(&[]),
                    vec!["t1".to_string(), "t2".to_string()]
                ),
                ("a".to_string(), string_vec(&["a"]), vec!["t2".to_string()]),
                ("a".to_string(), string_vec(&["b"]), vec!["t1".to_string()]),
                (
                    "b".to_string(),
                    string_vec(&["z"]),
                    vec!["t1".to_string(), "t2".to_string()]
                ),
            ],
        );
        Ok(())
    }

    #[test]
    fn aggregate_invocation_plans_split_by_resolved_flags() -> eyre::Result<()> {
        let package = package("a")?;
        let dedupe_flags = ResolvedFlags {
            diagnostics_only: true,
            dedupe: true,
            ..ResolvedFlags::default()
        };
        let plan_set = ExecutionPlanSet {
            plans: vec![
                ExecutionPlan {
                    target: TargetTriple("t1".to_string()),
                    package_plans: vec![package_plan(
                        &package,
                        "t1",
                        vec![string_vec(&[])],
                        ResolvedFlags::default(),
                    )],
                },
                ExecutionPlan {
                    target: TargetTriple("t2".to_string()),
                    package_plans: vec![package_plan(
                        &package,
                        "t2",
                        vec![string_vec(&[])],
                        dedupe_flags,
                    )],
                },
            ],
            show_pruned: false,
            show_target: true,
        };

        let simplified: Vec<_> = aggregate_invocation_plans(&plan_set)
            .into_iter()
            .map(|inv| {
                (
                    inv.combo,
                    inv.flags,
                    inv.targets
                        .into_iter()
                        .map(|target| target.triple.0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();

        sim_assert_eq!(
            simplified,
            vec![
                (
                    string_vec(&[]),
                    ResolvedFlags::default(),
                    vec!["t1".to_string()]
                ),
                (string_vec(&[]), dedupe_flags, vec!["t2".to_string()]),
            ],
        );
        Ok(())
    }

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
