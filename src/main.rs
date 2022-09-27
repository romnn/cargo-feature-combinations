pub mod config;

use anyhow::Result;
use cargo_metadata::{Metadata, MetadataCommand};
use config::Config;
use itertools::Itertools;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

#[derive(Debug)]
struct Summary {
    package_name: String,
    features: Vec<String>,
    exit_code: Option<i32>,
    success: bool,
    num_warnings: usize,
    num_errors: usize,
    // pedantic_success: Option<bool>,
}

#[derive(Debug, Default)]
struct Options {
    manifest_path: Option<String>,
    feature_matrix: bool,
    pedantic: bool,
    silent: bool,
    fail_fast: bool,
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

fn run_cargo_command(md: Metadata, mut cargo_args: Vec<String>, options: Options) -> Result<()> {
    let workspace_members: HashSet<_> = md.workspace_members.iter().collect();
    let all_packages: HashMap<_, _> = md
        .packages
        .iter()
        .map(|pkg| (pkg.id.clone(), pkg))
        .collect();
    let packages: Vec<_> = workspace_members
        .iter()
        .flat_map(|pkg_id| all_packages.get(pkg_id))
        .collect();

    let mut stdout = StandardStream::stdout(ColorChoice::Auto);
    lazy_static! {
        static ref CYAN: ColorSpec = color_spec(Color::Cyan, true);
        static ref RED: ColorSpec = color_spec(Color::Red, true);
        static ref YELLOW: ColorSpec = color_spec(Color::Yellow, true);
        static ref GREEN: ColorSpec = color_spec(Color::Green, true);
    }

    let mut summary: Vec<Summary> = Vec::new();
    for package in packages {
        let config = package.config()?;
        for features in package.feature_combinations(&config) {
            let mut features: Vec<_> = features.into_iter().collect();
            features.sort();

            let extra_args_idx = cargo_args
                .iter()
                .position(|arg| arg.as_str() == "--")
                .unwrap_or(cargo_args.len());
            let extra_args = cargo_args.split_off(extra_args_idx);

            let args = [
                cargo_args.clone(),
                vec![
                    "--no-default-features".to_string(),
                    format!("--features={}", &features.iter().join(",")),
                ],
                extra_args,
            ]
            .concat();

            if !options.silent {
                println!("");
            }
            let _ = stdout.set_color(&CYAN);
            if cargo_args.contains(&"build".into()) {
                print!("    Building ")
            } else if cargo_args.contains(&"check".into()) || cargo_args.contains(&"clippy".into())
            {
                print!("    Checking ")
            } else if cargo_args.contains(&"test".into()) {
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
            command.args(&args);
            command.current_dir(&working_dir);
            if !options.silent {
                command.stdout(Stdio::piped());
                command.stderr(Stdio::piped());
            }
            let output = command.output()?;

            lazy_static! {
                static ref WARNING_REGEX: Regex =
                    Regex::new(r"warning: .* generated (\d+) warnings?").unwrap();
                static ref ERROR_REGEX: Regex =
                    Regex::new(r"error: could not compile `.*` due to\s*(\d*)\s*previous errors?")
                        .unwrap();
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut num_errors = 0usize;
            for errors in ERROR_REGEX.captures(&stderr) {
                num_errors += errors
                    .get(1)
                    .and_then(|e| e.as_str().parse::<usize>().ok())
                    .unwrap_or(1);
            }
            let mut num_warnings = 0usize;
            for warnings in WARNING_REGEX.captures(&stderr) {
                num_warnings += warnings
                    .get(1)
                    .and_then(|w| w.as_str().parse::<usize>().ok())
                    .unwrap_or(0);
            }

            if options.fail_fast && !output.status.success() {
                std::process::exit(output.status.code().unwrap());
            }

            // let pedantic_success = if !output.status.success() {
            //     Some(false)
            // } else if options.pedantic {
            //     Command::new(&cargo)
            //         .args(&args)
            //         .arg("-Dwarnings")
            //         .stdout(Stdio::null())
            //         .stderr(Stdio::null())
            //         .current_dir(&working_dir)
            //         .output()
            //         .ok()
            //         .map(|output| output.status.success())
            // } else {
            //     None
            // };

            summary.push(Summary {
                package_name: package.name.clone(),
                features,
                exit_code: output.status.code(),
                success: output.status.success(),
                num_warnings,
                num_errors,
                // pedantic_success,
            });
        }
    }

    // print summary
    println!("");
    let most_errors = summary.iter().map(|s| s.num_errors).max();
    let most_warnings = summary.iter().map(|s| s.num_warnings).max();
    let errors_width = most_errors.unwrap_or(0).to_string().len();
    let warnings_width = most_warnings.unwrap_or(0).to_string().len();

    for s in summary {
        if !s.success || s.num_errors > 0 {
            let _ = stdout.set_color(&RED);
            print!("        FAIL ");
        } else if s.num_warnings > 0 {
            let _ = stdout.set_color(&YELLOW);
            print!("        WARN ")
        } else {
            let _ = stdout.set_color(&GREEN);
            print!("        PASS ")
        }
        let _ = stdout.reset();
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
    Ok(())
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().into_iter().skip(1).collect();
    let mut options = Options::default();

    let mut pos = 0;
    while pos < args.len() {
        let pair = (args[pos].as_str(), args.get(pos + 1));
        match pair {
            ("--manifest-path", Some(path)) => {
                options.manifest_path = Some(path.clone());
                pos += 1;
            }
            (path, _) if path.starts_with("--manifest-path=") => {
                options.manifest_path =
                    Some(path.trim_start_matches("--manifest-path=").to_string());
                pos += 1;
            }

            ("feature-matrix", _) => {
                options.feature_matrix = true;
                args.remove(pos);
            }
            ("--pedantic", _) => {
                options.pedantic = true;
                args.remove(pos);
            }
            ("--silent", _) => {
                options.silent = true;
                args.remove(pos);
            }
            ("--fail-fast", _) => {
                options.fail_fast = true;
                args.remove(pos);
            }
            _ => {
                pos += 1;
            }
        }
    }

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
