mod config;
mod tee;

use anyhow::Result;
use cargo_metadata::{Metadata, MetadataCommand};
use config::Config;
use itertools::Itertools;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::io;
use std::process::{Command, Stdio};
use tee::TeeReader;
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

lazy_static! {
    static ref CYAN: ColorSpec = color_spec(Color::Cyan, true);
    static ref RED: ColorSpec = color_spec(Color::Red, true);
    static ref YELLOW: ColorSpec = color_spec(Color::Yellow, true);
    static ref GREEN: ColorSpec = color_spec(Color::Green, true);
}

#[derive(Debug)]
struct Summary {
    package_name: String,
    features: Vec<String>,
    exit_code: Option<i32>,
    success: bool,
    num_warnings: usize,
    num_errors: usize,
}

#[derive(Debug, Default)]
struct Options {
    manifest_path: Option<String>,
    feature_matrix: bool,
    silent: bool,
    fail_fast: bool,
}

#[derive(Debug)]
struct Args(Vec<String>);

impl std::ops::Deref for Args {
    type Target = Vec<String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Args {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Args {
    pub fn contains(&self, arg: &str) -> bool {
        self.0
            .iter()
            .any(|a| a == arg || a.starts_with(&format!("{}=", arg)))
    }

    pub fn get(
        &self,
        arg: &str,
        has_value: bool,
    ) -> Option<(std::ops::RangeInclusive<usize>, String)> {
        for (idx, a) in self.0.iter().enumerate() {
            match (a, self.0.get(idx + 1)) {
                (key, Some(value)) if key == arg && has_value => {
                    return Some((idx..=idx + 1, value.clone()));
                }
                (key, _) if key == arg && !has_value => {
                    return Some((idx..=idx, key.clone()));
                }
                (key, _) if key.starts_with(&format!("{}=", arg)) => {
                    let value = key.trim_start_matches(&format!("{}=", arg));
                    return Some((idx..=idx, value.to_string()));
                }
                _ => {}
            }
        }
        None
    }
}

pub trait Package {
    fn config(&self) -> Result<Config>;
    fn feature_combinations(&self, config: &Config) -> Vec<HashSet<String>>;
    fn feature_matrix(&self, config: &Config) -> Vec<String>;
}

impl Package for cargo_metadata::Package {
    fn config(&self) -> Result<Config> {
        match self.metadata.get("cargo-feature-combinations") {
            Some(config) => {
                let config: Config = serde_json::from_value(config.clone())?;
                Ok(config)
            }
            None => Ok(Config::default()),
        }
    }

    fn feature_combinations(&self, config: &Config) -> Vec<HashSet<String>> {
        self.features
            .keys()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|ft| !config.denylist.contains(*ft))
            .powerset()
            .filter_map(|set| {
                let set: HashSet<_> = set.into_iter().cloned().collect();
                let skip = config
                    .skip_feature_sets
                    .iter()
                    .any(|skip_set| skip_set.is_subset(&set));
                if skip {
                    None
                } else {
                    Some(set)
                }
            })
            .collect()
    }

    fn feature_matrix(&self, config: &Config) -> Vec<String> {
        self.feature_combinations(config)
            .into_iter()
            .map(|features| features.iter().join(","))
            .collect()
    }
}

fn print_feature_matrix(md: Metadata) -> Result<()> {
    let root_package = md
        .root_package()
        .ok_or(anyhow::anyhow!("no root package"))?;
    let config = root_package.config()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&root_package.feature_matrix(&config))?
    );
    Ok(())
}

fn color_spec(color: Color, bold: bool) -> ColorSpec {
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color));
    spec.set_bold(bold);
    spec
}

fn run_cargo_command(md: Metadata, mut cargo_args: Args, options: Options) -> Result<()> {
    let workspace_members: HashSet<_> = md.workspace_members.iter().collect();
    let all_packages: HashMap<_, _> = md
        .packages
        .iter()
        .map(|pkg| (pkg.id.clone(), pkg))
        .collect();
    // find all packages in the workspace
    let packages: Vec<_> = workspace_members
        .iter()
        .flat_map(|pkg_id| all_packages.get(pkg_id))
        .collect();

    // split into cargo and extra arguments after --
    let extra_args_idx = cargo_args
        .iter()
        .position(|arg| arg.as_str() == "--")
        .unwrap_or(cargo_args.len());
    let extra_args = cargo_args.split_off(extra_args_idx);

    if !options.silent && !cargo_args.contains("--color") {
        // force colored output
        cargo_args.extend(["--color".to_string(), "always".to_string()]);
    }

    let mut stdout = StandardStream::stdout(ColorChoice::Auto);
    let mut summary: Vec<Summary> = Vec::new();

    for package in packages {
        let config = package.config()?;

        for features in package.feature_combinations(&config) {
            let mut features: Vec<_> = features.into_iter().collect();
            features.sort();

            if !options.silent {
                println!("");
            }
            let _ = stdout.set_color(&CYAN);
            if cargo_args.contains("build") {
                print!("    Building ")
            } else if cargo_args.contains("check") || cargo_args.contains("clippy") {
                print!("    Checking ")
            } else if cargo_args.contains("test") {
                print!("     Testing ")
            } else {
                print!("     Running ")
            }
            let _ = stdout.reset();
            println!(
                "{} ( features = [{}] )",
                package.name,
                features.iter().join(", ")
            );
            if !options.silent {
                println!("");
            }

            let manifest_path = &package.manifest_path;
            let working_dir = manifest_path.parent().ok_or(anyhow::anyhow!(
                "could not find parent dir of package {}",
                manifest_path.to_string()
            ))?;

            let cargo = std::env::var_os("CARGO").unwrap_or("cargo".into());
            let mut command = Command::new(&cargo);

            let args = [
                cargo_args.clone(),
                vec![
                    "--no-default-features".to_string(),
                    format!("--features={}", &features.iter().join(",")),
                ],
                extra_args.clone(),
            ]
            .concat();
            // dbg!(&args);

            command.args(&args);
            command.current_dir(&working_dir);

            let (output, exit_status) = if options.silent {
                let output = command.output()?;
                let exit_status = output.status;
                let output = String::from_utf8_lossy(&output.stderr);
                (output.to_string(), exit_status)
            } else {
                command.stderr(Stdio::piped());
                let mut process = command.spawn()?;

                // build an output writer buffer
                let output = Vec::<u8>::new();
                let output = io::Cursor::new(output);
                let mut output = strip_ansi_escapes::Writer::new(output);

                // tee write to buffer and stdout
                {
                    let proc_stderr = process.stderr.take().expect("open stderr");
                    let proc_reader = io::BufReader::new(proc_stderr);
                    let mut tee_reader = TeeReader::new(proc_reader, &mut output, true);
                    io::copy(&mut tee_reader, &mut stdout)?;
                }

                let exit_status = process.wait()?;
                let output: Vec<u8> = output.into_inner()?.into_inner();
                let output = String::from_utf8_lossy(&output);
                (output.to_string(), exit_status)
            };

            // let num_errors = count_errors(&output);
            // let mut num_warnings = 0usize;
            // for warnings in WARNING_REGEX.captures(&output) {
            //     num_warnings += warnings
            //         .get(1)
            //         .and_then(|w| w.as_str().parse::<usize>().ok())
            //         .unwrap_or(0);
            // }

            if options.fail_fast && !exit_status.success() {
                std::process::exit(exit_status.code().unwrap());
            }

            summary.push(Summary {
                package_name: package.name.clone(),
                features,
                exit_code: exit_status.code(),
                success: exit_status.success(),
                num_warnings: warning_counts(&output).sum(),
                num_errors: error_counts(&output).sum(),
            });
        }
    }

    // print summary
    println!("");
    let mut first_bad_exit_code: Option<i32> = None;
    let most_errors = summary.iter().map(|s| s.num_errors).max();
    let most_warnings = summary.iter().map(|s| s.num_warnings).max();
    let errors_width = most_errors.unwrap_or(0).to_string().len();
    let warnings_width = most_warnings.unwrap_or(0).to_string().len();

    for s in summary {
        if !s.success || s.num_errors > 0 {
            stdout.set_color(&RED).ok();
            print!("        FAIL ");
            first_bad_exit_code.get_or_insert(s.exit_code.unwrap());
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
            s.num_errors,
            s.num_warnings,
            s.features.iter().join(", "),
            ew = errors_width,
            ww = warnings_width,
        );
    }

    if let Some(code) = first_bad_exit_code {
        std::process::exit(code);
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args: Args = Args(std::env::args().into_iter().skip(1).collect());
    let mut options = Options::default();

    if let Some((_, manifest_path)) = args.get("--manifest-path", true) {
        options.manifest_path = Some(manifest_path);
    }
    if let Some((span, _)) = args.get("feature-matrix", false) {
        options.feature_matrix = true;
        args.drain(span);
    }
    if let Some((span, _)) = args.get("--silent", false) {
        options.silent = true;
        args.drain(span);
    }
    if let Some((span, _)) = args.get("--fail-fast", false) {
        options.fail_fast = true;
        args.drain(span);
    }
    // dbg!(&options);
    // dbg!(&args);

    let mut cmd = MetadataCommand::new();
    if let Some(ref manifest_path) = options.manifest_path {
        cmd.manifest_path(manifest_path);
    }
    let metadata = cmd.exec()?;

    if options.feature_matrix {
        print_feature_matrix(metadata)
    } else {
        run_cargo_command(metadata, args, options)
    }
}

fn warning_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    lazy_static! {
        static ref WARNING_REGEX: Regex =
            Regex::new(r"warning: .* generated (\d+) warnings?").unwrap();
    }
    WARNING_REGEX
        .captures_iter(&output)
        .flat_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(0))
}

fn error_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    lazy_static! {
        static ref ERROR_REGEX: Regex =
            Regex::new(r"error: could not compile `.*` due to\s*(\d*)\s*previous errors?").unwrap();
    }
    ERROR_REGEX
        .captures_iter(&output)
        .flat_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(1))
}

// let mut num_errors = 0usize;
//             for errors in ERROR_REGEX.captures(&output) {
//                 num_errors += errors
//                     .get(1)
//                     .and_then(|e| e.as_str().parse::<usize>().ok())
//                     .unwrap_or(1);
//             }

// }

#[cfg(test)]
mod test {
    use super::{error_counts, warning_counts};
    use anyhow::Result;
    use pretty_assertions::assert_eq;

    macro_rules! open {
        ( $path:expr ) => {{
            let txt = include_bytes!($path);
            let txt = std::str::from_utf8(txt)?;
            Ok::<_, anyhow::Error>(txt)
        }};
    }

    #[test]
    fn error_regex_single_mod_multiple_errors() -> Result<()> {
        let stderr = open!("../tests/single_mod_multiple_errors_stderr.txt")?;
        let errors: Vec<_> = error_counts(stderr).collect();
        assert_eq!(&errors, &vec![2]);
        Ok(())
    }

    #[test]
    fn warning_regex_two_mod_multiple_warnings() -> Result<()> {
        let stderr = open!("../tests/two_mods_warnings_stderr.txt")?;
        // let warnings = warning_counts(&stderr)
        // let all_captures: Vec<_> = WARNING_REGEX
        //     .captures_iter()
        //     .collect();
        // dbg!(&all_captures);
        // let warnings: Vec<_> = all_captures
        //     .into_iter()
        //     .flat_map(|c| c.get(1))
        //     .map(|m| m.as_str())
        //     .collect();
        let warnings: Vec<_> = warning_counts(stderr).collect();
        assert_eq!(&warnings, &vec![6, 7]);
        Ok(())
    }
}
