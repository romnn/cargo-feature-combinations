pub mod config;
pub mod features;

use anyhow::Result;
use features::feature_combinations;
use std::collections::{HashMap, HashSet};
// use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;
// use cargo::Config;
// use cargo::util::toml::TomlManifest;

// #[derive(Subcommand, Debug)]
// enum Command {
//     #[clap(name = "feature-matrix")]
//     FeatureMatrix {
//         #[clap(multiple_values = true)]
//         args: Vec<String>,
//     },
// }

// /// Command line arguments
// #[derive(Parser, Debug)]
// #[clap(
//     name = "cargo-feature-combinations",
//     version = option_env!("CARGO_PKG_VERSION").unwrap_or("unknown"),
//     about = "run cargo commands for all feature combinations",
//     author = "romnn <contact@romnn.com>",
// )]
// // trailing_var_arg=true
// struct Args {
//     /// Cargo manifest path
//     #[clap(long = "manifest-path")]
//     manifest_path: Option<PathBuf>,

//     #[clap(subcommand)]
//     command: Option<Command>,

//     /// Command and args to execute (must be last argument).
//     // #[clap(allow_hyphen_values = true, multiple_values = true)]
//     #[clap(multiple_values = true)]
//     args: Vec<String>,
//     // /// Number of times to greet
//     // #[clap(short, long, value_parser, default_value_t = 1)]
//     // count: u8,
// }

#[derive(Debug, Default)]
struct Args {
    manifest_path: Option<String>,
    feature_matrix: bool,
}

fn main() -> Result<()> {
    println!("hello");
    // let args = Args::parse();
    //use clap::{arg, command, AppSettings, Arg, ArgAction};

    //let matches = command!()
    //    // .global_setting(AppSettings::DeriveDisplayOrder)
    //    // .allow_negative_numbers(true)
    //    .arg(Arg::new("manifest-path").long("manifest-path"))
    //    // arg!(--manifest-path <PATH>).required(false))
    //    // .arg(arg!(--two <VALUE>).action(ArgAction::Set))
    //    // .arg(arg!(--one <VALUE>).action(ArgAction::Set))
    //    //
    //    // .subcommand_required(true)
    //    // .arg_required_else_help(true)
    //    // .subcommand(
    //    //     Command::new("feature-matrix").about("output the feature matrix for cargo"), // .arg(arg!([NAME])),
    //    // )
    //    .get_matches();
    //// .get_matches();
    //dbg!(&matches);
    // dbg!(&args);

    // dbg!(&std::env::args());
    let mut args: Vec<String> = std::env::args().into_iter().skip(1).collect();
    // .skip_while(|val| !val.starts_with("--manifest-path"));

    let mut config = Args::default();
    // let mut feature_matrix = false;
    // let mut manifest_path: Option<String> = None;

    let mut args_iter = args.iter();
    while let Some(arg) = args_iter.next() {
        if arg == "--manifest-path" {
            config.manifest_path = args_iter.next().cloned();
        } else if arg.starts_with("--manifest-path=") {
            config.manifest_path = Some(arg.trim_start_matches("--manifest-path=").into());
        } else if *arg == "feature-matrix" {
            config.feature_matrix = true;
        }
        // Some(ref p) if p == "--manifest-path" => {
        //     cmd.manifest_path(args.next().unwrap());
        // }
        // Some(p) => {
        //     cmd.manifest_path(p.trim_start_matches("--manifest-path="));
        // }
        // None => {}
    }

    args.retain(|arg| arg != "feature-matrix");

    dbg!(&config);
    dbg!(&args);
    // let manifest_path = match args.next() {
    //     Some(ref p) if p == "--manifest-path" => {
    //         cmd.manifest_path(args.next().unwrap());
    //     }
    //     Some(p) => {
    //         cmd.manifest_path(p.trim_start_matches("--manifest-path="));
    //     }
    //     None => {}
    // };

    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(manifest_path) = config.manifest_path {
        cmd.manifest_path(manifest_path);
    }
    let metadata = cmd.exec()?;
    // dbg!(&metadata);
    dbg!(&metadata.workspace_members);
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
    dbg!(&packages.len());
    // let packages = metadata.packages.filter(|pkg_kd| pk

    // let root_package = metadata
    //     .root_package()
    //     .ok_or(anyhow::anyhow!("no root package"))?;
    // dbg!(&root_package.features);

    for package in packages {
        let config = match package.metadata.get("cargo-feature-combinations") {
            Some(config) => {
                let config: Config = serde_json::from_value(config.clone())?;
                config
            }
            None => Config::default(),
        };
        dbg!(&config);
        let features = feature_combinations(&package, &config);
        // .features.keys());
        dbg!(&features.collect::<Vec<_>>());
        // dbg!(&features.count());
    }

    return Ok(());
    // return Ok(());

    use cargo_metadata::Message;
    use std::process::{Command, Stdio};

    // args.push("--message-format=json-render-diagnostics".to_string());
    args.extend([
        "--message-format".to_string(),
        "json-render-diagnostics".to_string(),
    ]);
    dbg!(&args);

    let mut command = Command::new("cargo")
        // .args(&["build", "--message-format=json-render-diagnostics"])
        .args(args)
        .stdout(Stdio::piped())
        // .stdout(Stdio::null())
        .spawn()?;

    if let Some(stdout) = command.stdout.take() {
        // panic!("test");
        let reader = std::io::BufReader::new(stdout);
        for message in cargo_metadata::Message::parse_stream(reader) {
            match message {
                Ok(Message::CompilerMessage(msg)) => {
                    eprintln!("compiler message: {:?}", msg);
                }
                Ok(Message::CompilerArtifact(artifact)) => {
                    // eprintln!("compiler artifact: {:?}", artifact);
                }
                Ok(Message::BuildScriptExecuted(script)) => {
                    // eprintln!("build script: {:?}", script);
                }
                Ok(Message::BuildFinished(finished)) => {
                    eprintln!("build finished: {:?}", finished);
                }
                Ok(other) => println!("unknown message: {:?}", other),
                // Err(err) => println!("error: {:?}", err),
                _ => {}
            }
        }
    }

    let output = command.wait().expect("Couldn't get cargo's exit status");
    Ok(())
}
