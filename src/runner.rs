//! Cargo command execution, output parsing, summary printing, and matrix output.

use crate::PKG_METADATA_SECTION;
use crate::cfg_eval::RustcCfgEvaluator;
use crate::cli::{CargoSubcommand, Options, cargo_subcommand};
use crate::package::{FeatureCombinationError, Package};
use crate::target::{RustcTargetDetector, TargetDetector, TargetTriple};

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

/// Summary of the outcome for running a cargo command on a single feature set.
#[derive(Debug)]
pub struct Summary {
    package_name: String,
    features: Vec<String>,
    exit_code: Option<i32>,
    pedantic_success: bool,
    num_warnings: usize,
    num_errors: usize,
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
        Regex::new(r"error: could not compile `.*` due to\s*(\d*)\s*previous errors?")
            .expect("valid error regex")
    });
    ERROR_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(1))
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
                "    Consider restricting the matrix using {PKG_METADATA_SECTION}.only_features",
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
/// This function is used by [`run_cargo_command`] after all packages and
/// feature sets have been processed.
#[must_use]
pub fn print_summary(
    summary: Vec<Summary>,
    mut stdout: termcolor::StandardStream,
    elapsed: Duration,
) -> ExitCode {
    let num_packages = summary
        .iter()
        .map(|s| &s.package_name)
        .collect::<HashSet<_>>()
        .len();
    let num_feature_sets = summary
        .iter()
        .map(|s| (&s.package_name, s.features.iter().collect::<Vec<_>>()))
        .collect::<HashSet<_>>()
        .len();

    println!();
    stdout.set_color(&CYAN).ok();
    print!("    Finished ");
    stdout.reset().ok();
    println!(
        "{num_feature_sets} total feature combination{} for {num_packages} package{} in {elapsed:?}",
        if num_feature_sets > 1 { "s" } else { "" },
        if num_packages > 1 { "s" } else { "" },
    );
    println!();

    let mut first_bad_exit_code: Option<i32> = None;
    let most_errors = summary.iter().map(|s| s.num_errors).max().unwrap_or(0);
    let most_warnings = summary.iter().map(|s| s.num_warnings).max().unwrap_or(0);
    let errors_width = most_errors.to_string().len();
    let warnings_width = most_warnings.to_string().len();

    for s in summary {
        if !s.pedantic_success {
            stdout.set_color(&RED).ok();
            print!("        FAIL ");
            if first_bad_exit_code.is_none() {
                first_bad_exit_code = s.exit_code;
            }
        } else if s.num_warnings > 0 {
            stdout.set_color(&YELLOW).ok();
            print!("        WARN ");
        } else {
            stdout.set_color(&GREEN).ok();
            print!("        PASS ");
        }
        stdout.reset().ok();
        println!(
            "{} ( {:ew$} errors, {:ww$} warnings, features = [{}] )",
            s.package_name,
            s.num_errors.to_string(),
            s.num_warnings.to_string(),
            s.features.iter().join(", "),
            ew = errors_width,
            ww = warnings_width,
        );
    }
    println!();

    first_bad_exit_code
}

fn print_package_cmd(
    package: &cargo_metadata::Package,
    features: &[&String],
    cargo_args: &[&str],
    all_args: &[&str],
    options: &Options,
    stdout: &mut StandardStream,
) {
    if !options.silent {
        println!();
    }
    stdout.set_color(&CYAN).ok();
    match cargo_subcommand(cargo_args) {
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
    stdout.reset().ok();
    print!(
        "{} ( features = [{}] )",
        package.name,
        features.as_ref().iter().join(", ")
    );
    if options.verbose {
        print!(" [cargo {}]", all_args.join(" "));
    }
    println!();
    if !options.silent {
        println!();
    }
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
pub fn print_feature_matrix(
    packages: &[&cargo_metadata::Package],
    pretty: bool,
    packages_only: bool,
) -> eyre::Result<ExitCode> {
    let detector = RustcTargetDetector::default();
    let target = detector.detect_target(&Vec::new())?;
    let mut evaluator = RustcCfgEvaluator::default();
    print_feature_matrix_for_target(packages, pretty, packages_only, &target, &mut evaluator)
}

/// Like [`print_feature_matrix`], but for a specific target and evaluator.
///
/// This is useful for library consumers that want to control target
/// resolution themselves, e.g. when cross-compiling.
///
/// # Errors
///
/// Returns an error if any configuration can not be parsed or serialization
/// of the JSON matrix fails.
pub fn print_feature_matrix_for_target(
    packages: &[&cargo_metadata::Package],
    pretty: bool,
    packages_only: bool,
    target: &TargetTriple,
    evaluator: &mut impl crate::cfg_eval::CfgEvaluator,
) -> eyre::Result<ExitCode> {
    let per_package_features = packages
        .iter()
        .map(|pkg| {
            let base_config = pkg.config()?;
            let config = crate::config::resolve::resolve_config(&base_config, target, evaluator)?;
            let features = if packages_only {
                vec!["default".to_string()]
            } else {
                pkg.feature_matrix(&config)?
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

    let matrix = if pretty {
        serde_json::to_string_pretty(&matrix)
    } else {
        serde_json::to_string(&matrix)
    }?;
    println!("{matrix}");
    Ok(None)
}

/// Run a cargo command for all requested packages and feature combinations.
///
/// This function drives the main execution loop by spawning cargo for each
/// feature set and collecting a [`Summary`] for every run.
///
/// Returns the [`ExitCode`] of the first failing feature combination, or
/// `None` if all combinations succeeded.
///
/// # Errors
///
/// Returns an error if a cargo process can not be spawned or if IO operations
/// fail while reading cargo's output.
pub fn run_cargo_command(
    packages: &[&cargo_metadata::Package],
    cargo_args: Vec<&str>,
    options: &Options,
) -> eyre::Result<ExitCode> {
    // Public API: if called directly, resolve config for the host target.
    let detector = RustcTargetDetector::default();
    let cargo_args_owned: Vec<String> = cargo_args.iter().map(|s| (*s).to_string()).collect();
    let target = detector.detect_target(&cargo_args_owned)?;
    let mut evaluator = RustcCfgEvaluator::default();
    run_cargo_command_for_target(packages, cargo_args, options, &target, &mut evaluator)
}

/// Like [`run_cargo_command`], but for a specific target and evaluator.
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

    let mut stdout = StandardStream::stdout(ColorChoice::Auto);
    let mut summary: Vec<Summary> = Vec::new();

    for package in packages {
        let base_config = package.config()?;
        let config = crate::config::resolve::resolve_config(&base_config, target, evaluator)?;

        for features in package.feature_combinations(&config)? {
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

            if options.errors_only {
                cmd.env(
                    "RUSTFLAGS",
                    format!(
                        "-Awarnings {}", // allows all warnings
                        std::env::var("RUSTFLAGS").unwrap_or_default()
                    ),
                );
            }

            let mut args = cargo_args.clone();
            let features_flag = format!("--features={}", &features.iter().join(","));
            if !missing_arguments {
                args.push("--no-default-features");
                args.push(&features_flag);
            }
            args.extend(extra_args.clone());
            print_package_cmd(package, &features, &cargo_args, &args, options, &mut stdout);

            cmd.args(args)
                .current_dir(working_dir)
                .stderr(process::Stdio::piped());
            let mut process = cmd.spawn()?;

            // build an output writer buffer
            let output_buffer = Vec::<u8>::new();
            let mut colored_output = io::Cursor::new(output_buffer);

            {
                // tee write to buffer and stdout
                if let Some(proc_stderr) = process.stderr.take() {
                    let mut proc_reader = io::BufReader::new(proc_stderr);
                    if options.silent {
                        io::copy(&mut proc_reader, &mut colored_output)?;
                    } else {
                        let mut tee_reader =
                            crate::tee::Reader::new(proc_reader, &mut stdout, true);
                        io::copy(&mut tee_reader, &mut colored_output)?;
                    }
                } else {
                    eprintln!("ERROR: failed to redirect stderr");
                }
            }

            let exit_status = process.wait()?;
            let output = strip_ansi_escapes::strip(colored_output.get_ref());
            let output = String::from_utf8_lossy(&output);

            let num_warnings = warning_counts(&output).sum::<usize>();
            let num_errors = error_counts(&output).sum::<usize>();
            let has_errors = num_errors > 0;
            let has_warnings = num_warnings > 0;

            let fail = !exit_status.success();

            let pedantic_fail = options.pedantic && (has_errors || has_warnings);
            let pedantic_success = !(fail || pedantic_fail);

            summary.push(Summary {
                features: features.into_iter().cloned().collect(),
                num_errors,
                num_warnings,
                package_name: package.name.to_string(),
                exit_code: exit_status.code(),
                pedantic_success,
            });

            if options.fail_fast && !pedantic_success {
                if options.silent {
                    io::copy(
                        &mut io::Cursor::new(colored_output.into_inner()),
                        &mut stdout,
                    )?;
                    stdout.flush().ok();
                }
                let code = print_summary(summary, stdout, start.elapsed())
                    .or(exit_status.code())
                    .unwrap_or(1);
                return Ok(Some(code));
            }
        }
    }

    Ok(print_summary(summary, stdout, start.elapsed()))
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
    fn warning_regex_two_mod_multiple_warnings() {
        let stderr = include_str!("../test-data/two_mods_warnings_stderr.txt");
        let warnings: Vec<_> = warning_counts(stderr).collect();
        sim_assert_eq!(&warnings, &vec![6, 7]);
    }
}
