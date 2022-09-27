pub mod config;
// pub mod features;

use anyhow::Result;
use cargo_metadata::{Metadata, MetadataCommand};
use config::Config;
// use features::feature_combinations;
use itertools::Itertools;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Debug, Default)]
struct Args {
    manifest_path: Option<String>,
    feature_matrix: bool,
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args()
        .into_iter()
        .skip(1)
        // .map(|s| s.as_str())
        .collect();
    let mut config = Args::default();

    let mut args_iter = args.iter();
    while let Some(arg) = args_iter.next() {
        if arg == &"--manifest-path" {
            config.manifest_path = args_iter.next().map(ToString::to_string);
        } else if arg.starts_with("--manifest-path=") {
            let path = arg.trim_start_matches("--manifest-path=").into();
            config.manifest_path = Some(path);
        } else if *arg == "feature-matrix" {
            config.feature_matrix = true;
        }
    }

    // remove our extra arguments
    // args.retain(|arg| arg != "feature-matrix");

    // dbg!(&config);
    // dbg!(&args);

    let mut cmd = MetadataCommand::new();
    if let Some(manifest_path) = config.manifest_path {
        cmd.manifest_path(manifest_path);
    }
    let metadata = cmd.exec()?;

    if config.feature_matrix {
        print_feature_matrix(metadata)
    } else {
        run_cargo_command(metadata, args)
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

fn print_feature_matrix(metadata: Metadata) -> Result<()> {
    let root_package = metadata
        .root_package()
        .ok_or(anyhow::anyhow!("no root package"))?;
    // dbg!(&root_package.features);
    let config = root_package.config()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&root_package.feature_matrix(&config))?
    );
    Ok(())
}

fn run_cargo_command(metadata: Metadata, mut args: Vec<String>) -> Result<()> {
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().collect();
    let all_packages: HashMap<_, _> = metadata
        .packages
        .iter()
        .map(|pkg| (pkg.id.clone(), pkg))
        .collect();
    let packages: Vec<_> = workspace_members
        .iter()
        .flat_map(|pkg_id| all_packages.get(pkg_id))
        .collect();
    // dbg!(&packages.len());

    for package in packages {
        let config = package.config()?;
        for features in package.feature_combinations(&config) {
            // let mut command = Command::new("cargo")
            // || ffi::OsString::from("cargo"));
            // dbg!(&features);
            // dbg!(&args);
            let cargo = std::env::var_os("CARGO").unwrap_or("cargo".into());
            use std::process::{Command, Stdio};
            let mut command = Command::new(cargo);
            // .args(&["build", "--message-format=json-render-diagnostics"])
            let cargo_args_idx = args.iter().position(|arg| arg.as_str() == "--");
            let extra_args = args.split_off(cargo_args_idx.unwrap_or(args.len()));

            command
                .args(&args)
                .arg("--no-default-features")
                .arg(format!("--features={}", &features.iter().join(",")))
                .args(&extra_args);

            // .stdout(Stdio::piped())
            // .stdout(Stdio::null())
            // .output()?;

            use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};
            println!("");
            let mut stdout = StandardStream::stdout(ColorChoice::Auto);
            let mut color_spec = ColorSpec::new();
            color_spec.set_fg(Some(Color::Cyan));
            color_spec.set_bold(true);
            let _ = stdout.set_color(&color_spec);

            if args.contains(&"build".into()) {
                print!("    Building ")
            } else if args.contains(&"check".into()) || args.contains(&"clippy".into()) {
                print!("    Checking ")
            } else if args.contains(&"test".into()) {
                print!("     Testing ")
            } else {
                print!("     Running ")
            }
            stdout.reset().unwrap();
            println!(
                "{} ( features = [{}] )",
                package.name,
                features.iter().join(", ")
            );
            println!("");

            let manifest_path = &package.manifest_path;
            let working_dir = manifest_path.parent().ok_or(anyhow::anyhow!(
                "could not find parent dir of package {}",
                manifest_path.to_string()
            ))?;
            // dbg!(&command);
            let output = command
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .current_dir(&working_dir)
                .output()?;
            if !output.status.success() {
                std::process::exit(output.status.code().unwrap());
            }
            // break;
        }
        // let config = match package.metadata.get("cargo-feature-combinations") {
        //     Some(config) => {
        //         let config: Config = serde_json::from_value(config.clone())?;
        //         config
        //     }
        //     None => Config::default(),
        // };
        // dbg!(&config);
        // let features = feature_combinations(&package, &config);
        // // .features.keys());
        // dbg!(&features.collect::<Vec<_>>());
        // dbg!(&features.count());
    }

    return Ok(());
    // return Ok(());

    // use cargo_metadata::Message;

    // args.push("--message-format=json-render-diagnostics".to_string());
    // args.extend([
    //     "--message-format".to_string(),
    //     "json-render-diagnostics".to_string(),
    // ]);

    // if let Some(stdout) = command.stdout.take() {
    //     // panic!("test");
    //     let reader = std::io::BufReader::new(stdout);
    //     for message in cargo_metadata::Message::parse_stream(reader) {
    //         match message {
    //             Ok(Message::CompilerMessage(msg)) => {
    //                 eprintln!("compiler message: {:?}", msg);
    //             }
    //             Ok(Message::CompilerArtifact(artifact)) => {
    //                 // eprintln!("compiler artifact: {:?}", artifact);
    //             }
    //             Ok(Message::BuildScriptExecuted(script)) => {
    //                 // eprintln!("build script: {:?}", script);
    //             }
    //             Ok(Message::BuildFinished(finished)) => {
    //                 eprintln!("build finished: {:?}", finished);
    //             }
    //             Ok(other) => println!("unknown message: {:?}", other),
    //             // Err(err) => println!("error: {:?}", err),
    //             _ => {}
    //         }
    //     }
    // }

    // let output = command.wait().expect("");
    Ok(())
}
