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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AliasValue {
    List(Vec<String>),
    Str(String),
}

impl AliasValue {
    fn into_tokens(self) -> Vec<String> {
        match self {
            AliasValue::List(list) => list,
            AliasValue::Str(s) => shlex::split(&s)
                .unwrap_or_else(|| s.split_whitespace().map(str::to_owned).collect()),
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

/// Expand a leading cargo command alias in `args` using the `[alias]` tables in
/// the cargo config hierarchy rooted at `workspace_root`.
///
/// Reports whether the argv changed so callers can preserve Cargo's alias
/// argument placement for generated cargo-fc arguments. Expansion is iterative
/// (an alias may reference another alias) and capped to avoid infinite loops.
pub(crate) fn expand_aliases_with_info(args: Vec<String>, workspace_root: &Path) -> AliasExpansion {
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
    let aliases = load_aliases(workspace_root);
    if aliases.is_empty() {
        return AliasExpansion {
            args,
            expanded: false,
            alias_provided_double_dash: false,
        };
    }

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
        let Some(expansion) = aliases.get(&token) else {
            break;
        };
        // An empty alias (`x = ""` / `x = []`) cannot stand in for the subcommand;
        // leave args untouched rather than dropping the subcommand token.
        if expansion.is_empty() {
            break;
        }
        let mut expanded = expansion.clone();
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
        && aliases.contains_key(token)
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

/// Merge the `[alias]` tables across the cargo config hierarchy. Configs nearer
/// the workspace take precedence (matching cargo), so they are inserted last.
fn load_aliases(workspace_root: &Path) -> BTreeMap<String, Vec<String>> {
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in config_paths(workspace_root) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(config) = toml::from_str::<CargoConfig>(&contents) else {
            continue;
        };
        for (name, value) in config.alias {
            aliases.insert(name, value.into_tokens());
        }
    }
    aliases
}

/// Cargo config files from lowest to highest precedence: `$CARGO_HOME` first,
/// then every ancestor of `workspace_root` from the filesystem root down (so the
/// nearest config wins). Within a directory `config.toml` wins over the legacy
/// `config`, so the legacy name is listed first.
fn config_paths(workspace_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = cargo_home() {
        paths.push(home.join("config"));
        paths.push(home.join("config.toml"));
    }
    let mut dirs: Vec<&Path> = workspace_root.ancestors().collect();
    dirs.reverse();
    for dir in dirs {
        paths.push(dir.join(".cargo").join("config"));
        paths.push(dir.join(".cargo").join("config.toml"));
    }
    paths
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

    fn expand_aliases(args: Vec<String>, workspace_root: &Path) -> Vec<String> {
        expand_aliases_with_info(args, workspace_root).args
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
    fn expands_quoted_string_alias() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(&tmp, "[alias]\nlint = \"clippy --features 'foo bar'\"\n")?;

        let out = expand_aliases(vec!["lint".to_string()], tmp.path());

        assert_eq!(out, ["clippy", "--features", "foo bar"]);
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
