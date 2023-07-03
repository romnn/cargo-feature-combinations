#![allow(clippy::missing_errors_doc)]

mod config;
mod tee;

use crate::config::Config;
use color_eyre::eyre::{self, WrapErr};
use itertools::Itertools;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

lazy_static! {
    static ref CYAN: ColorSpec = color_spec(Color::Cyan, true);
    static ref RED: ColorSpec = color_spec(Color::Red, true);
    static ref YELLOW: ColorSpec = color_spec(Color::Yellow, true);
    static ref GREEN: ColorSpec = color_spec(Color::Green, true);
}

#[derive(Debug)]
pub struct Summary {
    package_name: String,
    features: Vec<String>,
    exit_code: Option<i32>,
    pedantic_success: bool,
    num_warnings: Option<usize>,
    num_errors: Option<usize>,
}

#[derive(Debug)]
pub enum Subcommand {
    FeatureMatrix { pretty: bool },
    Help,
}

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    pub manifest_path: Option<PathBuf>,
    pub packages: HashSet<String>,
    pub command: Option<Subcommand>,
    pub silent: bool,
    pub verbose: bool,
    pub pedantic: bool,
    pub fail_fast: bool,
}

#[derive(Debug)]
pub struct Args(pub Vec<String>);

impl std::ops::Deref for Args {
    type Target = Vec<String>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Args {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug)]
pub struct ArgOptions {
    pub has_value: bool,
    pub remove: bool,
}

impl Args {
    #[inline]
    #[must_use]
    pub fn contains(&self, arg: &str) -> bool {
        self.0
            .iter()
            .any(|a| a == arg || a.starts_with(&format!("{arg}=")))
    }

    #[inline]
    pub fn get_all(
        &mut self,
        arg: &str,
        has_value: bool,
    ) -> impl Iterator<Item = (std::ops::RangeInclusive<usize>, String)> {
        let mut matched = Vec::new();
        for (idx, a) in self.0.iter().enumerate() {
            match (a, self.0.get(idx + 1)) {
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
        matched.into_iter()
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
    fn feature_combinations(&self, config: &Config) -> Vec<Vec<&String>>;
    fn feature_matrix(&self, config: &Config) -> Vec<String>;
}

impl Package for cargo_metadata::Package {
    #[inline]
    fn config(&self) -> eyre::Result<Config> {
        match self.metadata.get("cargo-feature-combinations") {
            Some(config) => {
                let config: Config = serde_json::from_value(config.clone())?;
                Ok(config)
            }
            None => Ok(Config::default()),
        }
    }

    #[inline]
    fn feature_combinations(&self, config: &Config) -> Vec<Vec<&String>> {
        self.features
            .keys()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|ft| !config.denylist.contains(*ft))
            .powerset()
            .filter_map(|mut set: Vec<&String>| {
                set.sort();
                let hset: HashSet<_> = set.iter().copied().cloned().collect();
                let skip = config
                    .skip_feature_sets
                    .iter()
                    .any(|skip_set| skip_set.is_subset(&hset));
                if skip {
                    None
                } else {
                    Some(set)
                }
            })
            .sorted_by(|a, b| Ord::cmp(a, b))
            // .sorted_by(|a, b| match Ord::cmp(&a.len(), &b.len()) {
            //     Ordering::Equal => Ord::cmp(a, b),
            //     ordering => ordering,
            // })
            .collect()
    }

    #[inline]
    fn feature_matrix(&self, config: &Config) -> Vec<String> {
        self.feature_combinations(config)
            .into_iter()
            .map(|features| features.iter().join(","))
            .collect()
    }
}

#[inline]
pub fn print_feature_matrix(
    packages: &[&cargo_metadata::Package],
    pretty: bool,
) -> eyre::Result<()> {
    let matrix: Vec<serde_json::Value> = packages
        .iter()
        .flat_map(|pkg| {
            let features = pkg
                .config()
                .as_ref()
                .map(|cfg| pkg.feature_matrix(cfg))
                .unwrap_or_default();
            features.into_iter().map(|ft| {
                serde_json::json!({
                    "name": pkg.name.clone(),
                    "features": ft,
                })
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

#[inline]
#[must_use]
pub fn color_spec(color: Color, bold: bool) -> ColorSpec {
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color));
    spec.set_bold(bold);
    spec
}

#[inline]
pub fn warning_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    lazy_static! {
        static ref WARNING_REGEX: Regex =
            Regex::new(r"warning: .* generated (\d+) warnings?").unwrap();
    }
    WARNING_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(0))
}

#[inline]
pub fn error_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    lazy_static! {
        static ref ERROR_REGEX: Regex =
            Regex::new(r"error: could not compile `.*` due to\s*(\d*)\s*previous errors?").unwrap();
    }
    ERROR_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(1))
}

#[inline]
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
    let most_errors = summary.iter().filter_map(|s| s.num_errors).max();
    let most_warnings = summary.iter().filter_map(|s| s.num_warnings).max();
    let errors_width = most_errors.unwrap_or(0).to_string().len();
    let warnings_width = most_warnings.unwrap_or(0).to_string().len();

    for s in summary {
        if !s.pedantic_success {
            stdout.set_color(&RED).ok();
            print!("        FAIL ");
            if first_bad_exit_code.is_none() {
                first_bad_exit_code = s.exit_code;
            }
        } else if s.num_warnings.unwrap_or(0) > 0 {
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
            s.num_errors.map_or("?".into(), |n| n.to_string()),
            s.num_warnings.map_or("?".into(), |n| n.to_string()),
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

#[inline]
fn print_package_cmd(
    package: &cargo_metadata::Package,
    features: &[&String],
    cargo_args: &Args,
    all_args: &[String],
    options: &Options,
    stdout: &mut StandardStream,
) {
    if !options.silent {
        println!();
    }
    stdout.set_color(&CYAN).ok();
    if cargo_args.contains("build") {
        print!("    Building ");
    } else if cargo_args.contains("check") || cargo_args.contains("clippy") {
        print!("    Checking ");
    } else if cargo_args.contains("test") {
        print!("     Testing ");
    } else {
        print!("     Running ");
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

#[inline]
pub fn run_cargo_command(
    packages: &[&cargo_metadata::Package],
    mut cargo_args: Args,
    options: &Options,
) -> eyre::Result<()> {
    let start = Instant::now();
    // let packages = md.workspace_packages();

    // split into cargo and extra arguments after --
    let extra_args_idx = cargo_args
        .iter()
        .position(|arg| arg.as_str() == "--")
        .unwrap_or(cargo_args.len());
    let extra_args = cargo_args.split_off(extra_args_idx);

    let missing_arguments = cargo_args.is_empty() && extra_args.is_empty();

    if !cargo_args.contains("--color") {
        // force colored output
        cargo_args.extend(["--color".to_string(), "always".to_string()]);
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
            let mut cmd = Command::new(&cargo);
            let mut args = cargo_args.clone();
            if !missing_arguments {
                args.push("--no-default-features".to_string());
                args.push(format!("--features={}", &features.iter().join(",")));
            }
            args.extend(extra_args.clone());
            print_package_cmd(
                package,
                &features,
                &cargo_args,
                args.as_slice(),
                options,
                &mut stdout,
            );

            cmd.args(args)
                .current_dir(working_dir)
                .stderr(Stdio::piped());
            let mut process = cmd.spawn()?;

            // build an output writer buffer
            let output_buffer = Vec::<u8>::new();
            let mut colored_output = io::Cursor::new(output_buffer);

            {
                // tee write to buffer and stdout
                let proc_stderr = process.stderr.take().expect("open stderr");
                let mut proc_reader = io::BufReader::new(proc_stderr);
                if options.silent {
                    io::copy(&mut proc_reader, &mut colored_output)?;
                } else {
                    let mut tee_reader = crate::tee::Reader::new(proc_reader, &mut stdout, true);
                    io::copy(&mut tee_reader, &mut colored_output)?;
                }
            }

            let exit_status = process.wait()?;
            let output = strip_ansi_escapes::strip(colored_output.get_ref())
                .map(|out| String::from_utf8_lossy(&out).into_owned());

            if let Err(ref err) = output {
                eprintln!("failed to read stderr: {err:?}");
            }
            let num_warnings = output.as_ref().ok().map(|out| warning_counts(out).sum());
            let num_errors = output.as_ref().ok().map(|out| error_counts(out).sum());

            let fail = !exit_status.success();
            let has_errors = num_errors.unwrap_or(0) > 0;
            let has_warnings = num_warnings.unwrap_or(0) > 0;
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

#[inline]
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
    --pedantic              Treat warnings like errors in summary and 
                            when using --fail-fast

Feature sets can be configured in your Cargo.toml configuration.
For example:

```toml
[package.metadata.cargo-feature-combinations]
# Exclude groupings of features that are incompatible or do not make sense
skip_feature_sets = [ ["foo", "bar"], ]

# Exclude features from the feature combination matrix
denylist = ["default", "full"]
```

For more information, see 'https://github.com/romnn/cargo-feature-combinations'.

See 'cargo help <command>' for more information on a specific command.
    "#;
    println!("{help}");
}

static VALID_BOOLS: [&str; 4] = ["yes", "true", "y", "t"];

#[inline]
pub fn run(bin_name: impl AsRef<str>) -> eyre::Result<()> {
    color_eyre::install()?;

    let mut args: Args = Args(
        std::env::args()
            // skip executable name
            .skip(1)
            // skip our own cargo-* command name
            .skip_while(|arg| arg.as_str() == bin_name.as_ref())
            .collect(),
    );
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

    // check for matrix command
    for (span, _) in args.get_all("matrix", false) {
        options.command = Some(Subcommand::FeatureMatrix { pretty: false });
        args.drain(span);
    }
    // check for pretty matrix option
    for (span, _) in args.get_all("--pretty", false) {
        if let Some(Subcommand::FeatureMatrix { ref mut pretty }) = options.command {
            *pretty = true;
        }
        args.drain(span);
    }

    // check for help command
    for (span, _) in args.get_all("--pretty", false) {
        options.command = Some(Subcommand::Help);
        args.drain(span);
    }

    // check for pedantic flag
    for (span, _) in args.get_all("--pedantic", false) {
        options.pedantic = true;
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

    // get metadata for cargo package
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(ref manifest_path) = options.manifest_path {
        cmd.manifest_path(manifest_path);
    }
    let metadata = cmd.exec()?;

    // filter packages
    let mut packages = metadata.workspace_packages();
    if !options.packages.is_empty() {
        packages.retain(|p| options.packages.contains(&p.name));
    }

    match options.command {
        Some(Subcommand::Help) => {
            print_help();
            Ok(())
        }
        Some(Subcommand::FeatureMatrix { pretty }) => {
            print_feature_matrix(packages.as_slice(), pretty)
        }
        None => run_cargo_command(packages.as_slice(), args, &options),
    }
}

#[cfg(test)]
mod test {
    use super::{error_counts, warning_counts};
    use color_eyre::eyre;
    use pretty_assertions::assert_eq;

    macro_rules! open {
        ( $path:expr ) => {{
            let txt = include_bytes!($path);
            let txt = std::str::from_utf8(txt)?;
            Ok::<_, eyre::Report>(txt)
        }};
    }

    #[test]
    fn error_regex_single_mod_multiple_errors() -> eyre::Result<()> {
        let stderr = open!("../tests/single_mod_multiple_errors_stderr.txt")?;
        let errors: Vec<_> = error_counts(stderr).collect();
        assert_eq!(&errors, &vec![2]);
        Ok(())
    }

    #[test]
    fn warning_regex_two_mod_multiple_warnings() -> eyre::Result<()> {
        let stderr = open!("../tests/two_mods_warnings_stderr.txt")?;
        let warnings: Vec<_> = warning_counts(stderr).collect();
        assert_eq!(&warnings, &vec![6, 7]);
        Ok(())
    }
}
