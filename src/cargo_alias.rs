//! Resolve cargo command aliases (`.cargo/config.toml` `[alias]`) so cargo-fc
//! sees the underlying built-in subcommand.
//!
//! cargo-fc forwards the subcommand it is given to the build driver. A cargo
//! alias such as `lint = "clippy --all-targets --no-deps"` is meaningful to
//! cargo, but a driver like `cargo-zigbuild` only recognizes the *built-in*
//! subcommand (`clippy`) when deciding whether to configure the zig
//! cross-compiler — and cargo-fc's own target-capability registry only knows
//! built-ins accept `--target`. Expanding the alias up front makes the built-in
//! subcommand visible to both, so `cargo fc lint` behaves like `cargo fc clippy`
//! by default.

use crate::print_warning;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

const MAX_ALIAS_EXPANSIONS: usize = 50;

pub(crate) struct AliasExpansion {
    pub(crate) args: Vec<String>,
    pub(crate) expanded: bool,
    /// Whether a resolved alias body, not the user's trailing args, contributed
    /// a `--` separator. This is true for
    /// `lint = "run --package wrapper -- lint"` and false for
    /// `serve = "run --package app"` invoked as `cargo fc serve -- arg`.
    pub(crate) alias_provided_double_dash: bool,
}

#[derive(Debug, Deserialize)]
struct CargoConfig {
    #[serde(default)]
    alias: BTreeMap<String, AliasValue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum AliasValue {
    List(Vec<String>),
    Str(String),
}

impl AliasValue {
    /// Splits a string alias on whitespace without shell quoting, as Cargo does,
    /// so `cargo fc <alias>` runs exactly the argv `cargo <alias>` would.
    fn into_tokens(self) -> Vec<String> {
        match self {
            AliasValue::List(list) => list,
            AliasValue::Str(s) => s.split_whitespace().map(str::to_owned).collect(),
        }
    }
}

/// Built-in cargo subcommands (and their short forms) that an alias can never
/// shadow — cargo always resolves these to the built-in, so cargo-fc must too.
fn is_builtin(token: &str) -> bool {
    matches!(
        token,
        "add"
            | "bench"
            | "build"
            | "b"
            | "check"
            | "c"
            | "clean"
            | "clippy"
            | "config"
            | "doc"
            | "d"
            | "fetch"
            | "fix"
            | "generate-lockfile"
            | "help"
            | "info"
            | "init"
            | "install"
            | "locate-project"
            | "login"
            | "logout"
            | "metadata"
            | "new"
            | "owner"
            | "package"
            | "pkgid"
            | "publish"
            | "read-manifest"
            | "remove"
            | "rm"
            | "run"
            | "r"
            | "rustc"
            | "rustdoc"
            | "search"
            | "test"
            | "t"
            | "tree"
            | "uninstall"
            | "update"
            | "vendor"
            | "verify-project"
            | "version"
            | "yank"
    )
}

/// Expand a leading cargo command alias in `args` the way Cargo would when run
/// from `cwd`.
///
/// A `CARGO_ALIAS_<NAME>` environment variable wins over the `[alias]` tables
/// of the cargo config hierarchy that Cargo discovers from `cwd`.
///
/// Reports whether the argv changed so callers can preserve Cargo's alias
/// argument placement for generated cargo-fc arguments. Expansion is iterative
/// (an alias may reference another alias) and capped to avoid infinite loops.
pub(crate) fn expand_aliases_with_info(args: Vec<String>, cwd: &Path) -> AliasExpansion {
    expand_aliases_with_env(args, cwd, |key| std::env::var(key).ok())
}

fn expand_aliases_with_env(
    args: Vec<String>,
    cwd: &Path,
    env_var: impl Fn(&str) -> Option<String>,
) -> AliasExpansion {
    let Some(idx) = crate::cli::subcommand_token_index(&args) else {
        return AliasExpansion {
            args,
            expanded: false,
            alias_provided_double_dash: false,
        };
    };
    match args.get(idx) {
        Some(token) if !is_builtin(token) => {}
        _ => {
            return AliasExpansion {
                args,
                expanded: false,
                alias_provided_double_dash: false,
            };
        }
    }
    let file_aliases = load_aliases(cwd);
    let alias = |name: &str| resolve_alias(name, file_aliases.get(name), &env_var);

    // Args before the subcommand token (e.g. `+toolchain`) are preserved.
    let mut head: Vec<String> = args.get(..idx).unwrap_or_default().to_vec();
    let original_rest: Vec<String> = args.get(idx..).unwrap_or_default().to_vec();
    let mut rest: Vec<String> = args.get(idx..).unwrap_or_default().to_vec();
    let mut expansions = 0;
    let mut changed = false;
    let mut alias_provided_double_dash = false;
    for _ in 0..MAX_ALIAS_EXPANSIONS {
        let Some(token) = rest.first().cloned() else {
            break;
        };
        if is_builtin(&token) {
            break;
        }
        let Some(mut expanded) = alias(&token) else {
            break;
        };
        // An empty alias (`x = ""` / `x = []`) cannot stand in for the subcommand;
        // leave args untouched rather than dropping the subcommand token.
        if expanded.is_empty() {
            break;
        }
        alias_provided_double_dash |= expanded.iter().any(|arg| arg == "--");
        expanded.extend_from_slice(rest.get(1..).unwrap_or_default());
        rest = expanded;
        expansions += 1;
        changed = true;
    }

    // Only a run that exhausted the iteration cap is a suspected cycle; breaking
    // early (builtin reached, unknown token, empty alias) is normal termination.
    if expansions == MAX_ALIAS_EXPANSIONS
        && let Some(token) = rest.first()
        && !is_builtin(token)
        && alias(token).is_some()
    {
        print_warning!(
            "stopped expanding cargo aliases after {MAX_ALIAS_EXPANSIONS} expansions; possible alias cycle involving `{token}`"
        );
        rest = original_rest;
        changed = false;
        alias_provided_double_dash = false;
    }

    head.extend(rest);
    AliasExpansion {
        args: head,
        expanded: changed,
        alias_provided_double_dash,
    }
}

/// Combine an alias's config-file value with its `CARGO_ALIAS_<NAME>`
/// override, which Cargo derives from the config key `alias.<name>`.
///
/// Like Cargo, the environment replaces a string alias but extends an array
/// alias with its whitespace-separated tokens.
fn resolve_alias(
    name: &str,
    file_value: Option<&AliasValue>,
    env_var: impl Fn(&str) -> Option<String>,
) -> Option<Vec<String>> {
    let key = format!("CARGO_ALIAS_{}", name.to_uppercase().replace('-', "_"));
    let env_value = env_var(&key).map(AliasValue::Str);
    match (file_value.cloned(), env_value) {
        (Some(AliasValue::List(mut list)), Some(env_value)) => {
            list.extend(env_value.into_tokens());
            Some(list)
        }
        (_, Some(env_value)) => Some(env_value.into_tokens()),
        (file_value, None) => file_value.map(AliasValue::into_tokens),
    }
}

/// Merge the `[alias]` tables across the cargo config hierarchy, as Cargo
/// merges its config.
///
/// A config nearer `cwd` replaces a string alias, but an array alias defined at
/// several levels is joined with the nearer elements last.
fn load_aliases(cwd: &Path) -> BTreeMap<String, AliasValue> {
    let mut aliases: BTreeMap<String, AliasValue> = BTreeMap::new();
    for path in config_paths(cwd) {
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                print_warning!(
                    "failed to read cargo alias config {}: {err}",
                    path.display()
                );
                continue;
            }
        };
        let config = match toml::from_str::<CargoConfig>(&contents) {
            Ok(config) => config,
            Err(err) => {
                print_warning!(
                    "failed to parse cargo alias config {}: {err}",
                    path.display()
                );
                continue;
            }
        };
        for (name, value) in config.alias {
            match (aliases.get_mut(&name), value) {
                (Some(AliasValue::List(farther)), AliasValue::List(nearer)) => {
                    farther.extend(nearer);
                }
                (_, value) => {
                    aliases.insert(name, value);
                }
            }
        }
    }
    aliases
}

/// Cargo config files from lowest to highest precedence, mirroring Cargo's
/// discovery from `cwd`.
///
/// Every ancestor of `cwd` contributes its `.cargo` directory from the
/// filesystem root down, so the nearest config wins.
/// `$CARGO_HOME` comes first unless it already is one of those directories, in
/// which case it keeps its ancestor position.
fn config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = cwd.ancestors().map(|dir| dir.join(".cargo")).collect();
    dirs.reverse();
    if let Some(home) = cargo_home()
        && !dirs.contains(&home)
    {
        dirs.insert(0, home);
    }
    dirs.iter().filter_map(|dir| config_file(dir)).collect()
}

/// The config file Cargo reads in `dir`: the legacy extensionless `config`
/// wins over `config.toml` when both exist.
fn config_file(dir: &Path) -> Option<PathBuf> {
    ["config", "config.toml"]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
}

fn cargo_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".cargo"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use color_eyre::eyre;

    fn write(dir: &TempDir, body: &str) -> eyre::Result<()> {
        dir.child(".cargo").create_dir_all()?;
        dir.child(".cargo/config.toml").write_str(body)?;
        Ok(())
    }

    fn expand_aliases(args: Vec<String>, cwd: &Path) -> Vec<String> {
        expand_aliases_with_env(args, cwd, |_| None).args
    }

    #[test]
    fn expands_string_alias_in_place() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = \"clippy --all-targets --no-deps\"\n")?;
        let args = vec!["lint".to_string(), "--workspace".to_string()];

        let out = expand_aliases(args, tmp.path());

        assert_eq!(out, ["clippy", "--all-targets", "--no-deps", "--workspace"]);
        Ok(())
    }

    #[test]
    fn expands_nested_aliases_until_builtin() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            "[alias]\nlint = \"ci --all-targets\"\nci = \"clippy --no-deps\"\n",
        )?;
        let args = vec!["lint".to_string(), "--workspace".to_string()];

        let out = expand_aliases(args, tmp.path());

        assert_eq!(out, ["clippy", "--no-deps", "--all-targets", "--workspace"]);
        Ok(())
    }

    #[test]
    fn splits_string_alias_on_whitespace_like_cargo() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            "[alias]\nlint = \"clippy --features 'foo bar' --target-dir C:\\\\fc\"\n",
        )?;

        let out = expand_aliases(vec!["lint".to_string()], tmp.path());

        // Cargo applies no shell quoting or escaping to string aliases; an
        // argument containing spaces needs the array form.
        assert_eq!(
            out,
            [
                "clippy",
                "--features",
                "'foo",
                "bar'",
                "--target-dir",
                r"C:\fc"
            ]
        );
        Ok(())
    }

    #[test]
    fn keeps_array_alias_elements_verbatim() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            "[alias]\nlint = [\"clippy\", \"--features\", \"foo bar\"]\n",
        )?;

        let out = expand_aliases(vec!["lint".to_string()], tmp.path());

        assert_eq!(out, ["clippy", "--features", "foo bar"]);
        Ok(())
    }

    #[test]
    fn legacy_config_wins_over_config_toml_in_one_directory() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = \"build\"\nonly-toml = \"check\"\n")?;
        tmp.child(".cargo/config")
            .write_str("[alias]\nlint = \"clippy\"\n")?;

        // Cargo reads only the extensionless file when both exist, so aliases
        // defined only in `config.toml` are not visible either.
        assert_eq!(
            expand_aliases(vec!["lint".to_string()], tmp.path()),
            ["clippy"]
        );
        assert_eq!(
            expand_aliases(vec!["only-toml".to_string()], tmp.path()),
            ["only-toml"]
        );
        Ok(())
    }

    #[test]
    fn tracks_only_alias_provided_double_dash() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            "[alias]\nwrapper = \"run --package wrapper -- lint\"\nserve = \"run --package app\"\n",
        )?;

        let wrapper = expand_aliases_with_info(vec!["wrapper".to_string()], tmp.path());
        let serve = expand_aliases_with_info(
            vec![
                "serve".to_string(),
                "--".to_string(),
                "program-arg".to_string(),
            ],
            tmp.path(),
        );

        // Only separators from alias bodies select after-separator placement;
        // user-provided trailing args stay program argv for a normal run alias.
        assert!(wrapper.alias_provided_double_dash);
        assert!(!serve.alias_provided_double_dash);
        Ok(())
    }

    #[test]
    fn leaves_builtin_subcommand_untouched() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nclippy = \"build\"\n")?;
        let args = vec!["clippy".to_string()];

        assert_eq!(expand_aliases(args.clone(), tmp.path()), args);
        Ok(())
    }

    #[test]
    fn leaves_non_target_builtin_subcommand_untouched() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nclean = \"clippy\"\n")?;
        let args = vec!["clean".to_string()];

        assert_eq!(expand_aliases(args.clone(), tmp.path()), args);
        Ok(())
    }

    #[test]
    fn preserves_toolchain_prefix() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = \"clippy\"\n")?;
        let args = vec!["+nightly".to_string(), "lint".to_string()];

        assert_eq!(expand_aliases(args, tmp.path()), ["+nightly", "clippy"]);
        Ok(())
    }

    #[test]
    fn empty_alias_keeps_subcommand_token() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = \"\"\n")?;

        let out = expand_aliases(
            vec!["lint".to_string(), "--workspace".to_string()],
            tmp.path(),
        );

        assert_eq!(out, ["lint", "--workspace"]);
        Ok(())
    }

    #[test]
    fn env_alias_overrides_config_alias() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            "[alias]\nmy-tool = \"run --manifest-path ../tool/Cargo.toml --\"\n",
        )?;
        let env_var = |key: &str| {
            (key == "CARGO_ALIAS_MY_TOOL")
                .then(|| "run --manifest-path /abs/tool/Cargo.toml --".to_string())
        };

        let out = expand_aliases_with_env(vec!["my-tool".to_string()], tmp.path(), env_var).args;

        // Cargo maps `alias.my-tool` to `CARGO_ALIAS_MY_TOOL` and lets the
        // environment win over config files.
        assert_eq!(
            out,
            ["run", "--manifest-path", "/abs/tool/Cargo.toml", "--"]
        );
        Ok(())
    }

    #[test]
    fn env_alias_expands_without_config_files() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        let env_var =
            |key: &str| (key == "CARGO_ALIAS_LINT").then(|| "clippy --no-deps".to_string());

        let out = expand_aliases_with_env(vec!["lint".to_string()], tmp.path(), env_var).args;

        assert_eq!(out, ["clippy", "--no-deps"]);
        Ok(())
    }

    #[test]
    fn env_alias_extends_array_alias() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = [\"check\"]\n")?;
        let env_var =
            |key: &str| (key == "CARGO_ALIAS_LINT").then(|| "--all-targets --locked".to_string());

        let out = expand_aliases_with_env(vec!["lint".to_string()], tmp.path(), env_var).args;

        // Cargo merges an environment value into a list instead of replacing it.
        assert_eq!(out, ["check", "--all-targets", "--locked"]);
        Ok(())
    }

    #[test]
    fn array_aliases_join_across_config_levels() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = [\"clippy\"]\n")?;
        let nested = tmp.child("ws");
        nested.child(".cargo").create_dir_all()?;
        nested
            .child(".cargo/config.toml")
            .write_str("[alias]\nlint = [\"--no-deps\"]\n")?;

        // Cargo joins arrays from every config level, nearer elements last.
        assert_eq!(
            expand_aliases(vec!["lint".to_string()], nested.path()),
            ["clippy", "--no-deps"]
        );
        Ok(())
    }

    #[test]
    fn nearest_config_to_cwd_wins() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = \"clippy\"\n")?;
        let nested = tmp.child("ws");
        nested.child(".cargo").create_dir_all()?;
        nested
            .child(".cargo/config.toml")
            .write_str("[alias]\nlint = \"check\"\n")?;

        // Running from the nested directory sees its config over the parent's,
        // while the parent directory only sees its own.
        assert_eq!(
            expand_aliases(vec!["lint".to_string()], nested.path()),
            ["check"]
        );
        assert_eq!(
            expand_aliases(vec!["lint".to_string()], tmp.path()),
            ["clippy"]
        );
        Ok(())
    }

    #[test]
    fn caps_cyclic_alias_expansion() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nalpha = \"beta\"\nbeta = \"alpha\"\n")?;

        let out = expand_aliases(
            vec!["alpha".to_string(), "--locked".to_string()],
            tmp.path(),
        );

        assert_eq!(out, ["alpha", "--locked"]);
        Ok(())
    }
}
