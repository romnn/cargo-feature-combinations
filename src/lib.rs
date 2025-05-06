#![allow(clippy::missing_errors_doc)]

mod config;
mod tee;

use crate::config::Config;
use color_eyre::eyre::{self, WrapErr};
use itertools::Itertools;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

static CYAN: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Cyan, true));
static RED: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Red, true));
static YELLOW: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Yellow, true));
static GREEN: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Green, true));

#[derive(Debug)]
pub struct Summary {
    package_name: String,
    features: Vec<String>,
    exit_code: Option<i32>,
    pedantic_success: bool,
    num_warnings: usize,
    num_errors: usize,
}

#[derive(Debug)]
pub enum Command {
    FeatureMatrix { pretty: bool },
    Version,
    Help,
}

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    pub manifest_path: Option<PathBuf>,
    pub packages: HashSet<String>,
    pub exclude_packages: HashSet<String>,
    pub command: Option<Command>,
    pub only_packages_with_lib_target: bool,
    pub silent: bool,
    pub verbose: bool,
    pub pedantic: bool,
    pub errors_only: bool,
    pub packages_only: bool,
    pub fail_fast: bool,
}

pub trait ArgumentParser {
    fn contains(&self, arg: &str) -> bool;
    fn get_all(&self, arg: &str, has_value: bool)
    -> Vec<(std::ops::RangeInclusive<usize>, String)>;
}

impl ArgumentParser for Vec<String> {
    fn contains(&self, arg: &str) -> bool {
        self.iter()
            .any(|a| a == arg || a.starts_with(&format!("{arg}=")))
    }

    fn get_all(
        &self,
        arg: &str,
        has_value: bool,
    ) -> Vec<(std::ops::RangeInclusive<usize>, String)> {
        let mut matched = Vec::new();
        for (idx, a) in self.iter().enumerate() {
            match (a, self.get(idx + 1)) {
                (key, Some(value)) if key == arg && has_value => {
                    matched.push((idx..=idx + 1, value.clone()));
                }
                (key, _) if key == arg && !has_value => {
                    matched.push((idx..=idx, key.clone()));
                }
                (key, _) if key.starts_with(&format!("{arg}=")) => {
                    let value = key.trim_start_matches(&format!("{arg}="));
                    matched.push((idx..=idx, value.to_string()));
                }
                _ => {}
            }
        }
        matched.reverse();
        matched
    }
}

pub trait Package {
    /// Parses the config for this package if present.
    ///
    /// If the Cargo.toml manifest contains a configuration section,
    /// the latter is parsed.
    /// Otherwise, a default configuration is used.
    ///
    /// # Errors
    ///
    /// If the configuration in the manifest can not be parsed,
    /// an Error is returned.
    ///
    fn config(&self) -> eyre::Result<Config>;
    fn feature_combinations<'a>(&'a self, config: &'a Config) -> Vec<Vec<&'a String>>;
    fn feature_matrix(&self, config: &Config) -> Vec<String>;
}

impl Package for cargo_metadata::Package {
    fn config(&self) -> eyre::Result<Config> {
        let mut config = match self.metadata.get("cargo-feature-combinations") {
            Some(config) => {
                let config: Config = serde_json::from_value(config.clone())?;
                config
            }
            None => Config::default(),
        };

        // handle deprecated config values
        config
            .exclude_feature_sets
            .append(&mut config.deprecated.skip_feature_sets);
        config
            .exclude_features
            .extend(config.deprecated.denylist.drain());
        config
            .include_feature_sets
            .append(&mut config.deprecated.exact_combinations);

        Ok(config)
    }

    fn feature_combinations<'a>(&'a self, config: &'a Config) -> Vec<Vec<&'a String>> {
        // Generate the base powerset from
        // - all features
        // - or from isolated sets, minus excluded features
        let base_powerset = if config.isolated_feature_sets.is_empty() {
            generate_global_base_powerset(
                &self.features,
                &config.exclude_features,
                &config.include_features,
            )
        } else {
            generate_isolated_base_powerset(
                &self.features,
                &config.isolated_feature_sets,
                &config.exclude_features,
                &config.include_features,
            )
        };

        // Filter out feature sets that contain skip sets
        let mut filtered_powerset = base_powerset
            .into_iter()
            .filter(|feature_set| {
                !config.exclude_feature_sets.iter().any(|skip_set| {
                    // remove feature sets containing any of the skip sets
                    skip_set
                        .iter()
                        // skip set is contained when all its features are contained
                        .all(|skip_feature| feature_set.contains(skip_feature))
                })
            })
            .collect::<BTreeSet<_>>();

        // Add back exact combinations
        for proposed_exact_combination in &config.include_feature_sets {
            // Remove non-existent features and switch reference to that pointing to `self`
            let exact_combination = proposed_exact_combination
                .iter()
                .filter_map(|maybe_feature| {
                    self.features.get_key_value(maybe_feature).map(|(k, _v)| k)
                })
                .collect::<BTreeSet<_>>();

            // This exact combination may now be empty, but empty combination is always added anyway
            filtered_powerset.insert(exact_combination);
        }

        // Re-collect everything into a vector of vectors
        filtered_powerset
            .into_iter()
            .map(|set| set.into_iter().sorted().collect::<Vec<_>>())
            .sorted()
            .collect::<Vec<_>>()
    }

    fn feature_matrix(&self, config: &Config) -> Vec<String> {
        self.feature_combinations(config)
            .into_iter()
            .map(|features| features.iter().join(","))
            .collect()
    }
}

/// Generates the **global** base [powerset](Itertools::powerset) of features.
/// Global features are all features that are defined in the package, except the
/// features from the provided denylist.
///
/// The returned powerset is a two-level [`BTreeSet`], with the strings pointing
/// pack to the `package_features`.
fn generate_global_base_powerset<'a>(
    package_features: &'a BTreeMap<String, Vec<String>>,
    exclude_features: &'a HashSet<String>,
    include_features: &'a HashSet<String>,
) -> BTreeSet<BTreeSet<&'a String>> {
    package_features
        .keys()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|ft| !exclude_features.contains(*ft))
        .powerset()
        .map(|combination| {
            combination
                .into_iter()
                .chain(include_features)
                .collect::<BTreeSet<&'a String>>()
        })
        .collect()
}

/// Generates the **isolated** base [powerset](Itertools::powerset) of features.
/// Isolated features are features from the provided isolated feature sets,
/// except non-existent features and except the features from the provided
/// denylist.
///
/// The returned powerset is a two-level [`BTreeSet`], with the strings pointing
/// pack to the `package_features`.
fn generate_isolated_base_powerset<'a>(
    package_features: &'a BTreeMap<String, Vec<String>>,
    isolated_feature_sets: &[HashSet<String>],
    exclude_features: &'a HashSet<String>,
    include_features: &'a HashSet<String>,
) -> BTreeSet<BTreeSet<&'a String>> {
    // Collect known package features for easy querying
    let known_features = package_features.keys().collect::<HashSet<_>>();

    isolated_feature_sets
        .iter()
        .flat_map(|isolated_feature_set| {
            isolated_feature_set
                .iter()
                .filter(|ft| known_features.contains(*ft)) // remove non-existent features
                .filter(|ft| !exclude_features.contains(*ft)) // remove features from denylist
                .powerset()
                .map(|combination| {
                    combination
                        .into_iter()
                        .filter_map(|feature| known_features.get(feature).copied())
                        .chain(include_features)
                        .collect::<BTreeSet<_>>()
                })
        })
        .collect()
}

pub fn print_feature_matrix(
    packages: &[&cargo_metadata::Package],
    pretty: bool,
    packages_only: bool,
) -> eyre::Result<()> {
    let per_package_features = packages
        .iter()
        .map(|pkg| {
            let config = pkg.config()?;
            let features = if packages_only {
                vec!["default".to_string()]
            } else {
                pkg.feature_matrix(&config)
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
    Ok(())
}

#[must_use]
pub fn color_spec(color: Color, bold: bool) -> ColorSpec {
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color));
    spec.set_bold(bold);
    spec
}

pub fn warning_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    static WARNING_REGEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"warning: .* generated (\d+) warnings?").unwrap());
    WARNING_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(0))
}

pub fn error_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    static ERROR_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"error: could not compile `.*` due to\s*(\d*)\s*previous errors?").unwrap()
    });
    ERROR_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(1))
}

pub fn print_summary(
    summary: Vec<Summary>,
    mut stdout: termcolor::StandardStream,
    elapsed: Duration,
) {
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

    if let Some(exit_code) = first_bad_exit_code {
        std::process::exit(exit_code);
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

pub fn run_cargo_command(
    packages: &[&cargo_metadata::Package],
    mut cargo_args: Vec<&str>,
    options: &Options,
) -> eyre::Result<()> {
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
        let config = package.config()?;

        for features in package.feature_combinations(&config) {
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
                package_name: package.name.clone(),
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
                print_summary(summary, stdout, start.elapsed());
                std::process::exit(exit_status.code().unwrap_or(1));
            }
        }
    }

    print_summary(summary, stdout, start.elapsed());
    Ok(())
}

fn print_help() {
    let help = r#"Run cargo commands for all feature combinations

USAGE:
    cargo [+toolchain] [SUBCOMMAND] [SUBCOMMAND_OPTIONS]
    cargo [+toolchain] [OPTIONS] [CARGO_OPTIONS] [CARGO_SUBCOMMAND]

SUBCOMMAND:
    matrix                  Print JSON feature combination matrix to stdout
        --pretty            Print pretty JSON

OPTIONS:
    --help                  Print help information
    --silent                Hide cargo output and only show summary
    --fail-fast             Fail fast on the first bad feature combination
    --errors-only           Allow all warnings, show errors only (-Awarnings)
    --exclude-package       Exclude a package from feature combinations 
    --only-packages-with-lib-target
                            Only consider packages with a library target
    --pedantic              Treat warnings like errors in summary and
                            when using --fail-fast

Feature sets can be configured in your Cargo.toml configuration.
For example:

```toml
[package.metadata.cargo-feature-combinations]
# When at least one isolated feature set is configured, stop taking all project
# features as a whole, and instead take them in these isolated sets. Build a
# sub-matrix for each isolated set, then merge sub-matrices into the overall
# feature matrix. If any two isolated sets produce an identical feature
# combination, such combination will be included in the overall matrix only once.
#
# This feature is intended for projects with large number of features, sub-sets
# of which are completely independent, and thus don’t need cross-play.
#
# Other configuration options are still respected.
isolated_feature_sets = [
    ["foo-a", "foo-b", "foo-c"],
    ["bar-a", "bar-b"],
    ["other-a", "other-b", "other-c"],
]

# Exclude groupings of features that are incompatible or do not make sense
exclude_feature_sets = [ ["foo", "bar"], ] # formerly "skip_feature_sets"

# Exclude features from the feature combination matrix
exclude_features = ["default", "full"] # formerly "denylist"

# When using a cargo workspace, you can exclude packages in the *root* `Cargo.toml`
exclude_packages = ["package-a", "package-b"]

# In the end, always add these exact combinations to the overall feature matrix, 
# unless one is already present there.
#
# Non-existent features are ignored. Other configuration options are ignored.
include_feature_sets = [
    ["foo-a", "bar-a", "other-a"],
] # formerly "exact_combinations"
```

For more information, see 'https://github.com/romnn/cargo-feature-combinations'.

See 'cargo help <command>' for more information on a specific command.
    "#;
    println!("{help}");
}

static VALID_BOOLS: [&str; 4] = ["yes", "true", "y", "t"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CargoSubcommand {
    Build,
    Check,
    Test,
    Doc,
    Run,
    Other,
}

fn cargo_subcommand(args: &[impl AsRef<str>]) -> CargoSubcommand {
    let args: HashSet<&str> = args.iter().map(AsRef::as_ref).collect();
    if args.contains("build") || args.contains("b") {
        CargoSubcommand::Build
    } else if args.contains("check") || args.contains("c") || args.contains("clippy") {
        CargoSubcommand::Check
    } else if args.contains("test") || args.contains("t") {
        CargoSubcommand::Test
    } else if args.contains("doc") || args.contains("d") {
        CargoSubcommand::Doc
    } else if args.contains("run") || args.contains("r") {
        CargoSubcommand::Run
    } else {
        CargoSubcommand::Other
    }
}

pub fn parse_arguments(bin_name: &str) -> eyre::Result<(Options, Vec<String>)> {
    let mut args: Vec<String> = std::env::args_os()
        // skip executable name
        .skip(1)
        // skip our own cargo-* command name
        .skip_while(|arg| {
            let arg = arg.as_os_str();
            arg == bin_name || arg == "cargo"
        })
        .map(|s| s.to_string_lossy().to_string())
        .collect();

    let mut options = Options {
        verbose: VALID_BOOLS.contains(
            &std::env::var("VERBOSE")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
        ),
        ..Options::default()
    };

    // extract path to manifest to operate on
    for (span, manifest_path) in args.get_all("--manifest-path", true) {
        let manifest_path = PathBuf::from(manifest_path);
        let manifest_path = manifest_path
            .canonicalize()
            .wrap_err_with(|| format!("manifest {} does not exist", manifest_path.display()))?;
        options.manifest_path = Some(manifest_path);
        args.drain(span);
    }

    // extract packages to operate on
    for flag in ["--package", "-p"] {
        for (span, package) in args.get_all(flag, true) {
            options.packages.insert(package);
            args.drain(span);
        }
    }

    for (span, package) in args.get_all("--exclude-package", true) {
        options.exclude_packages.insert(package.trim().to_string());
        args.drain(span);
    }

    for (span, _) in args.get_all("--only-packages-with-lib-target", false) {
        options.only_packages_with_lib_target = true;
        args.drain(span);
    }

    // check for matrix command
    for (span, _) in args.get_all("matrix", false) {
        options.command = Some(Command::FeatureMatrix { pretty: false });
        args.drain(span);
    }
    // check for pretty matrix option
    for (span, _) in args.get_all("--pretty", false) {
        if let Some(Command::FeatureMatrix { ref mut pretty }) = options.command {
            *pretty = true;
        }
        args.drain(span);
    }

    // check for help command
    for (span, _) in args.get_all("--help", false) {
        options.command = Some(Command::Help);
        args.drain(span);
    }

    // check for version flag
    for (span, _) in args.get_all("--version", false) {
        options.command = Some(Command::Version);
        args.drain(span);
    }

    // check for version command
    for (span, _) in args.get_all("version", false) {
        options.command = Some(Command::Version);
        args.drain(span);
    }

    // check for pedantic flag
    for (span, _) in args.get_all("--pedantic", false) {
        options.pedantic = true;
        args.drain(span);
    }

    // check for errors only
    for (span, _) in args.get_all("--errors-only", false) {
        options.errors_only = true;
        args.drain(span);
    }

    // packages only
    for (span, _) in args.get_all("--packages-only", false) {
        options.packages_only = true;
        args.drain(span);
    }

    // check for silent flag
    for (span, _) in args.get_all("--silent", false) {
        options.silent = true;
        args.drain(span);
    }

    // check for fail fast flag
    for (span, _) in args.get_all("--fail-fast", false) {
        options.fail_fast = true;
        args.drain(span);
    }

    Ok((options, args))
}

pub fn run(bin_name: &str) -> eyre::Result<()> {
    color_eyre::install()?;

    let (options, cargo_args) = parse_arguments(bin_name)?;

    if let Some(Command::Help) = options.command {
        print_help();
        return Ok(());
    }

    if let Some(Command::Version) = options.command {
        println!("cargo-{bin_name} v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // get metadata for cargo package
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(ref manifest_path) = options.manifest_path {
        cmd.manifest_path(manifest_path);
    }
    let metadata = cmd.exec()?;
    let mut packages = metadata.workspace_packages();

    // filter excluded packages via CLI arguments
    packages.retain(|p| !options.exclude_packages.contains(&p.name));

    if options.only_packages_with_lib_target {
        // filter only packages with a library target
        packages.retain(|p| {
            p.targets
                .iter()
                .any(|t| t.kind.contains(&cargo_metadata::TargetKind::Lib))
        });
    }

    if let Some(root_package) = metadata.root_package() {
        let config = root_package.config()?;
        // filter packages based on root package Cargo.toml configuration
        packages.retain(|p| !config.exclude_packages.contains(&p.name));
    }

    // filter packages based on CLI options
    if !options.packages.is_empty() {
        packages.retain(|p| options.packages.contains(&p.name));
    }

    let cargo_args: Vec<&str> = cargo_args.iter().map(String::as_str).collect();
    match options.command {
        Some(Command::Version | Command::Help) => unreachable!(),
        Some(Command::FeatureMatrix { pretty }) => {
            print_feature_matrix(&packages, pretty, options.packages_only)
        }
        None => {
            if cargo_subcommand(cargo_args.as_slice()) == CargoSubcommand::Other {
                eyre::bail!(
                    "`cargo {bin_name}` only works for cargo's `build`, `test`, `run`, `check`, `doc`, and `clippy` subcommands"
                )
            }
            run_cargo_command(&packages, cargo_args, &options)
        }
    }
}

#[cfg(test)]
mod test {
    use super::{Package, error_counts, warning_counts};
    use crate::config::Config;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::collections::HashSet;

    static INIT: std::sync::Once = std::sync::Once::new();

    /// Initialize test
    ///
    /// This ensures `color_eyre` is setup once.
    pub(crate) fn init() {
        INIT.call_once(|| {
            color_eyre::install().ok();
        });
    }

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

    #[test]
    fn combinations() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo-c", "foo-a", "foo-b"])?;
        let config = Config::default();
        let expected_combinations = vec![
            vec![],
            vec!["foo-a"],
            vec!["foo-a", "foo-b"],
            vec!["foo-a", "foo-b", "foo-c"],
            vec!["foo-a", "foo-c"],
            vec!["foo-b"],
            vec!["foo-b", "foo-c"],
            vec!["foo-c"],
        ];

        let actual_combinations = package.feature_combinations(&config);

        sim_assert_eq!(expected_combinations, actual_combinations);
        Ok(())
    }

    #[test]
    fn combinations_isolated() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-b", "bar-a", "car-b", "car-a"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "foo-b".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            ..Default::default()
        };
        let expected_combinations = vec![
            vec![],
            vec!["bar-a"],
            vec!["bar-a", "bar-b"],
            vec!["bar-b"],
            vec!["foo-a"],
            vec!["foo-a", "foo-b"],
            vec!["foo-b"],
        ];

        let actual_combinations = package.feature_combinations(&config);

        sim_assert_eq!(expected_combinations, actual_combinations);
        Ok(())
    }

    #[test]
    fn combinations_isolated_non_existent() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-a", "bar-b", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "non-existent".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            ..Default::default()
        };
        let expected_combinations = vec![
            vec![],
            vec!["bar-a"],
            vec!["bar-a", "bar-b"],
            vec!["bar-b"],
            vec!["foo-a"],
        ];

        let actual_combinations = package.feature_combinations(&config);

        sim_assert_eq!(expected_combinations, actual_combinations);
        Ok(())
    }

    #[test]
    fn combinations_isolated_denylist() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-b", "bar-a", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "foo-b".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            exclude_features: HashSet::from(["bar-a".to_string()]),
            ..Default::default()
        };
        let expected_combinations = vec![
            vec![],
            vec!["bar-b"],
            vec!["foo-a"],
            vec!["foo-a", "foo-b"],
            vec!["foo-b"],
        ];

        let actual_combinations = package.feature_combinations(&config);

        sim_assert_eq!(expected_combinations, actual_combinations);
        Ok(())
    }

    #[test]
    fn combinations_isolated_non_existent_denylist() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-b", "foo-a", "bar-a", "bar-b", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "non-existent".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            exclude_features: HashSet::from(["bar-a".to_string()]),
            ..Default::default()
        };
        let expected_combinations = vec![vec![], vec!["bar-b"], vec!["foo-a"]];

        let actual_combinations = package.feature_combinations(&config);

        sim_assert_eq!(expected_combinations, actual_combinations);
        Ok(())
    }

    #[test]
    fn combinations_isolated_non_existent_denylist_exact() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-a", "bar-b", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "non-existent".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            exclude_features: HashSet::from(["bar-a".to_string()]),
            include_feature_sets: vec![HashSet::from([
                "car-a".to_string(),
                "bar-a".to_string(),
                "non-existent".to_string(),
            ])],
            ..Default::default()
        };
        let expected_combinations =
            vec![vec![], vec!["bar-a", "car-a"], vec!["bar-b"], vec!["foo-a"]];

        let actual_combinations = package.feature_combinations(&config);

        sim_assert_eq!(expected_combinations, actual_combinations);
        Ok(())
    }

    fn package_with_features(features: &[&str]) -> eyre::Result<cargo_metadata::Package> {
        use cargo_metadata::{PackageBuilder, PackageId};
        use semver::Version;

        let mut package = PackageBuilder::new(
            "test",
            Version::parse("0.1.0")?,
            PackageId {
                repr: "test".to_string(),
            },
            "",
        )
        .build()?;
        package.features = features
            .iter()
            .map(|feature| ((*feature).to_string(), vec![]))
            .collect();
        Ok(package)
    }
}
