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

use crate::config::ResolvedEnv;
use crate::print_warning;
use color_eyre::eyre;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

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

/// Built-in cargo subcommands that an alias can never shadow — cargo always
/// resolves these to the built-in, so cargo-fc must too.
///
/// The short forms and `clippy` are left out; see [`is_shadowable_builtin`].
fn is_builtin(token: &str) -> bool {
    matches!(
        token,
        "add"
            | "bench"
            | "build"
            | "check"
            | "clean"
            | "config"
            | "doc"
            | "fetch"
            | "fix"
            | "generate-lockfile"
            | "git-checkout"
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
            | "report"
            | "run"
            | "rustc"
            | "rustdoc"
            | "search"
            | "test"
            | "tree"
            | "uninstall"
            | "update"
            | "vendor"
            | "verify-project"
            | "version"
            | "yank"
    )
}

/// Built-in short forms, which are built-in aliases, and the external `clippy`,
/// all of which a user alias of the same name shadows in Cargo.
///
/// cargo-fc follows such an alias only when it resolves to a command cargo-fc
/// has a policy for; otherwise the name keeps its built-in meaning, as it did
/// before cargo-fc followed these aliases, and Cargo expands it.
fn is_shadowable_builtin(token: &str) -> bool {
    matches!(token, "b" | "c" | "d" | "r" | "t" | "rm" | "clippy")
}

/// Expand a leading cargo command alias in `args` the way Cargo would when run
/// from `cwd`, or from the directory a leading `-C <dir>` names.
///
/// A `CARGO_ALIAS_<NAME>` environment variable, as `env` resolves it, wins over
/// the `[alias]` tables of the cargo config hierarchy Cargo discovers from that
/// directory.
///
/// Reports whether the argv changed so callers can preserve Cargo's alias
/// argument placement for generated cargo-fc arguments. Expansion is iterative,
/// since an alias may name another alias.
///
/// # Errors
///
/// Returns an error, as Cargo does, for an alias that names itself again
/// through a chain.
pub(crate) fn expand_aliases_with_info(
    args: Vec<String>,
    cwd: &Path,
    env: &ResolvedEnv,
) -> eyre::Result<AliasExpansion> {
    expand_aliases_with_env(args, cwd, |key| {
        env.effective_var(key)
            .and_then(|value| value.into_string().ok())
    })
}

fn expand_aliases_with_env(
    args: Vec<String>,
    cwd: &Path,
    env_var: impl Fn(&str) -> Option<String>,
) -> eyre::Result<AliasExpansion> {
    let unchanged = |args| AliasExpansion {
        args,
        expanded: false,
        alias_provided_double_dash: false,
    };
    let Some(first) = crate::cli::subcommand_token_index(&args) else {
        return Ok(unchanged(args));
    };
    if args.get(first).is_none_or(|token| is_builtin(token)) {
        return Ok(unchanged(args));
    }
    let config_dir = config_dir(args.get(..first).unwrap_or_default(), cwd);
    let file_aliases = load_aliases(&config_dir);
    let alias = |name: &str| resolve_alias(name, file_aliases.get(name), &env_var);

    // Like Cargo, expansion replaces the leading token and goes on while the
    // new leading token is another alias, so a body that starts with a flag or
    // a `+toolchain` ends the chain and Cargo resolves the rest.
    let mut args = args;
    let mut rest = args.split_off(first);
    let mut chain: Vec<String> = Vec::new();
    let mut alias_provided_double_dash = false;
    // The state before the chain first expanded a shadowable name, restored
    // when the chain ends at a command cargo-fc has no policy for.
    let mut before_shadowing = None;
    while let Some(token) = rest.first().filter(|token| !is_builtin(token)).cloned() {
        // An empty alias (`x = ""` / `x = []`) cannot stand in for the
        // subcommand; leave args untouched rather than dropping the token.
        let Some(expanded) = alias(&token).filter(|expanded| !expanded.is_empty()) else {
            break;
        };
        if let Some(first) = chain.first().filter(|_| chain.contains(&token)) {
            eyre::bail!(
                "alias {first} has unresolvable recursive definition: {} -> {token}",
                chain.join(" -> ")
            );
        }
        if is_shadowable_builtin(&token) && before_shadowing.is_none() {
            before_shadowing = Some((rest.clone(), chain.len(), alias_provided_double_dash));
        }
        alias_provided_double_dash |= expanded.iter().any(|arg| arg == "--");
        rest.splice(..1, expanded);
        chain.push(token);
    }

    // A shadowed built-in keeps its built-in meaning unless its alias resolves
    // to a command cargo-fc plans for, so `t = "nextest run"` still plans as
    // `test` while `c = "clippy"` plans as `clippy`.
    let ends_at_known_command = rest
        .first()
        .and_then(|token| crate::cli::builtin_canonical_command(token))
        .is_some();
    if let Some((saved_rest, saved_chain, saved_double_dash)) = before_shadowing
        && !ends_at_known_command
    {
        rest = saved_rest;
        chain.truncate(saved_chain);
        alias_provided_double_dash = saved_double_dash;
    }

    args.extend(rest);
    Ok(AliasExpansion {
        args,
        expanded: !chain.is_empty(),
        alias_provided_double_dash,
    })
}

/// The directory Cargo reads config from: `cwd`, or the directory a global
/// `-C <dir>` or `-C<dir>` among the args before the subcommand names.
fn config_dir(global_args: &[String], cwd: &Path) -> PathBuf {
    let mut dir = None;
    let mut args = global_args.iter();
    while let Some(arg) = args.next() {
        if arg == "-C" {
            dir = args.next().map(String::as_str);
        } else if let Some(value) = arg.strip_prefix("-C") {
            dir = Some(value);
        }
    }
    let Some(dir) = dir else {
        return cwd.to_path_buf();
    };
    // Resolve `..` so the walk over ancestors starts where Cargo moves to.
    let dir = cwd.join(dir);
    std::fs::canonicalize(&dir).unwrap_or(dir)
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
    use indoc::indoc;

    fn write(dir: &TempDir, body: &str) -> eyre::Result<()> {
        dir.child(".cargo").create_dir_all()?;
        dir.child(".cargo/config.toml").write_str(body)?;
        Ok(())
    }

    fn expand_aliases(args: Vec<String>, cwd: &Path) -> Vec<String> {
        expand_aliases_with_env(args, cwd, |_| None)
            .expect("alias expansion without cycles")
            .args
    }

    #[test_util::test]
    fn expands_string_alias_in_place() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = "clippy --all-targets --no-deps"
            "#},
        )?;
        let args = vec!["lint".to_string(), "--workspace".to_string()];

        let out = expand_aliases(args, tmp.path());

        assert_eq!(out, ["clippy", "--all-targets", "--no-deps", "--workspace"]);
        Ok(())
    }

    #[test_util::test]
    fn expands_nested_aliases_until_builtin() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = "ci --all-targets"
                ci = "clippy --no-deps"
            "#},
        )?;
        let args = vec!["lint".to_string(), "--workspace".to_string()];

        let out = expand_aliases(args, tmp.path());

        assert_eq!(out, ["clippy", "--no-deps", "--all-targets", "--workspace"]);
        Ok(())
    }

    #[test_util::test]
    fn splits_string_alias_on_whitespace_like_cargo() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = "clippy --features 'foo bar' --target-dir C:\\fc"
            "#},
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

    #[test_util::test]
    fn keeps_array_alias_elements_verbatim() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = ["clippy", "--features", "foo bar"]
            "#},
        )?;

        let out = expand_aliases(vec!["lint".to_string()], tmp.path());

        assert_eq!(out, ["clippy", "--features", "foo bar"]);
        Ok(())
    }

    #[test_util::test]
    fn legacy_config_wins_over_config_toml_in_one_directory() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = "build"
                only-toml = "check"
            "#},
        )?;
        tmp.child(".cargo/config").write_str(indoc! {r#"
            [alias]
            lint = "clippy"
        "#})?;

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

    #[test_util::test]
    fn tracks_only_alias_provided_double_dash() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                wrapper = "run --package wrapper -- lint"
                serve = "run --package app"
            "#},
        )?;

        let wrapper = expand_aliases_with_info(
            vec!["wrapper".to_string()],
            tmp.path(),
            &ResolvedEnv::default(),
        )?;
        let serve = expand_aliases_with_info(
            vec![
                "serve".to_string(),
                "--".to_string(),
                "program-arg".to_string(),
            ],
            tmp.path(),
            &ResolvedEnv::default(),
        )?;

        // Only separators from alias bodies select after-separator placement;
        // user-provided trailing args stay program argv for a normal run alias.
        assert!(wrapper.alias_provided_double_dash);
        assert!(!serve.alias_provided_double_dash);
        Ok(())
    }

    #[test_util::test]
    fn user_alias_shadows_short_forms_and_clippy() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                clippy = "build"
                c = "test --no-run"
                check = "test"
                report = "version"
                t = "nextest run"
                lint = "t"
            "#},
        )?;
        let expand = |name: &str| expand_aliases(vec![name.to_string()], tmp.path());

        // Cargo lets a user alias shadow an external subcommand and a built-in
        // short form, warning about the first.
        assert_eq!(expand("clippy"), ["build"]);
        assert_eq!(expand("c"), ["test", "--no-run"]);
        // A shadowing alias to a command cargo-fc plans no policy for keeps the
        // built-in meaning, also when reached through another alias.
        assert_eq!(expand("t"), ["t"]);
        assert_eq!(expand("lint"), ["t"]);
        // Without an alias, a short form stays as it is.
        assert_eq!(expand("b"), ["b"]);
        // A full built-in name always wins, as in Cargo.
        assert_eq!(expand("check"), ["check"]);
        assert_eq!(expand("report"), ["report"]);
        Ok(())
    }

    /// A body that starts with a flag ends the chain, leaving the rest to
    /// Cargo, and a shadowed short form reached that way keeps its meaning.
    #[test_util::test]
    fn chain_stops_at_a_leading_flag() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                outer = "--quiet inner"
                inner = "clippy"
                c = "--quiet inner"
            "#},
        )?;
        let expand = |name: &str| expand_aliases(vec![name.to_string()], tmp.path());

        assert_eq!(expand("outer"), ["--quiet", "inner"]);
        assert_eq!(expand("c"), ["c"]);
        Ok(())
    }

    /// A `+toolchain` in an alias body stays in front of the command, where
    /// cargo-fc applies it as the toolchain override, although Cargo itself
    /// rejects one there.
    #[test_util::test]
    fn keeps_a_toolchain_from_an_alias_body() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                unused = "+nightly udeps --all-targets"
            "#},
        )?;

        let out = expand_aliases(vec!["unused".to_string()], tmp.path());

        assert_eq!(out, ["+nightly", "udeps", "--all-targets"]);
        Ok(())
    }

    /// `-C ..` leaves a directory, so that directory's aliases no longer apply.
    #[test_util::test]
    fn c_to_a_parent_leaves_the_child_directory_aliases_behind() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        let child = tmp.child("child");
        child.child(".cargo").create_dir_all()?;
        child.child(".cargo/config.toml").write_str(indoc! {r#"
            [alias]
            t = "test --no-run"
        "#})?;

        let out = expand_aliases(
            vec!["-C".to_string(), "..".to_string(), "t".to_string()],
            child.path(),
        );

        assert_eq!(out, ["-C", "..", "t"]);
        Ok(())
    }

    /// `-C <dir>` moves Cargo, and so its config discovery, to `<dir>`.
    #[test_util::test]
    fn reads_aliases_from_the_directory_c_names() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                t = "test --no-run"
            "#},
        )?;
        let elsewhere = TempDir::new()?;
        let ws = tmp.path().to_string_lossy().into_owned();

        let here = expand_aliases(vec!["t".to_string()], elsewhere.path());
        let moved = expand_aliases(
            vec!["-C".to_string(), ws.clone(), "t".to_string()],
            elsewhere.path(),
        );
        let inline = expand_aliases(vec![format!("-C{ws}"), "t".to_string()], elsewhere.path());

        assert_eq!(here, ["t"]);
        assert_eq!(moved, ["-C", ws.as_str(), "test", "--no-run"]);
        assert_eq!(inline, [format!("-C{ws}").as_str(), "test", "--no-run"]);
        Ok(())
    }

    #[test_util::test]
    fn leaves_non_target_builtin_subcommand_untouched() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                clean = "clippy"
            "#},
        )?;
        let args = vec!["clean".to_string()];

        assert_eq!(expand_aliases(args.clone(), tmp.path()), args);
        Ok(())
    }

    #[test_util::test]
    fn preserves_toolchain_prefix() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = "clippy"
            "#},
        )?;
        let args = vec!["+nightly".to_string(), "lint".to_string()];

        assert_eq!(expand_aliases(args, tmp.path()), ["+nightly", "clippy"]);
        Ok(())
    }

    #[test_util::test]
    fn empty_alias_keeps_subcommand_token() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = ""
            "#},
        )?;

        let out = expand_aliases(
            vec!["lint".to_string(), "--workspace".to_string()],
            tmp.path(),
        );

        assert_eq!(out, ["lint", "--workspace"]);
        Ok(())
    }

    #[test_util::test]
    fn env_alias_overrides_config_alias() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                my-tool = "run --manifest-path ../tool/Cargo.toml --"
            "#},
        )?;
        let env_var = |key: &str| {
            (key == "CARGO_ALIAS_MY_TOOL")
                .then(|| "run --manifest-path /abs/tool/Cargo.toml --".to_string())
        };

        let out = expand_aliases_with_env(vec!["my-tool".to_string()], tmp.path(), env_var)?.args;

        // Cargo maps `alias.my-tool` to `CARGO_ALIAS_MY_TOOL` and lets the
        // environment win over config files.
        assert_eq!(
            out,
            ["run", "--manifest-path", "/abs/tool/Cargo.toml", "--"]
        );
        Ok(())
    }

    #[test_util::test]
    fn env_alias_expands_without_config_files() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        let env_var =
            |key: &str| (key == "CARGO_ALIAS_LINT").then(|| "clippy --no-deps".to_string());

        let out = expand_aliases_with_env(vec!["lint".to_string()], tmp.path(), env_var)?.args;

        assert_eq!(out, ["clippy", "--no-deps"]);
        Ok(())
    }

    #[test_util::test]
    fn env_alias_extends_array_alias() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = ["check"]
            "#},
        )?;
        let env_var =
            |key: &str| (key == "CARGO_ALIAS_LINT").then(|| "--all-targets --locked".to_string());

        let out = expand_aliases_with_env(vec!["lint".to_string()], tmp.path(), env_var)?.args;

        // Cargo merges an environment value into a list instead of replacing it.
        assert_eq!(out, ["check", "--all-targets", "--locked"]);
        Ok(())
    }

    #[test_util::test]
    fn array_aliases_join_across_config_levels() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = ["clippy"]
            "#},
        )?;
        let nested = tmp.child("ws");
        nested.child(".cargo").create_dir_all()?;
        nested.child(".cargo/config.toml").write_str(indoc! {r#"
            [alias]
            lint = ["--no-deps"]
        "#})?;

        // Cargo joins arrays from every config level, nearer elements last.
        assert_eq!(
            expand_aliases(vec!["lint".to_string()], nested.path()),
            ["clippy", "--no-deps"]
        );
        Ok(())
    }

    #[test_util::test]
    fn nearest_config_to_cwd_wins() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                lint = "clippy"
            "#},
        )?;
        let nested = tmp.child("ws");
        nested.child(".cargo").create_dir_all()?;
        nested.child(".cargo/config.toml").write_str(indoc! {r#"
            [alias]
            lint = "check"
        "#})?;

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

    #[test_util::test]
    fn cyclic_alias_is_an_error_like_cargo() -> eyre::Result<()> {
        let tmp = TempDir::new()?;
        write(
            &tmp,
            indoc! {r#"
                [alias]
                alpha = "beta"
                beta = "alpha"
                c = "c --locked"
            "#},
        )?;
        let expand = |name: &str| {
            expand_aliases_with_env(vec![name.to_string()], tmp.path(), |_| None)
                .err()
                .map(|err| err.to_string())
        };

        assert_eq!(
            expand("alpha").as_deref(),
            Some("alias alpha has unresolvable recursive definition: alpha -> beta -> alpha")
        );
        assert_eq!(
            expand("c").as_deref(),
            Some("alias c has unresolvable recursive definition: c -> c")
        );
        Ok(())
    }
}
