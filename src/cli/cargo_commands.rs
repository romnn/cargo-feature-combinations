//! Knowledge about the Cargo CLI surface: built-in subcommands, aliases,
//! plugin commands, and which leading flags take values.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum CargoSubcommand {
    Build,
    Check,
    Lint,
    Test,
    Doc,
    Run,
    Other,
}

/// Determine the cargo subcommand implied by the argument list.
pub(crate) fn cargo_subcommand(args: &[impl AsRef<str>]) -> CargoSubcommand {
    match cargo_subcommand_token(args) {
        Some(token) => subcommand_from_token(&token),
        None => CargoSubcommand::Other,
    }
}

fn subcommand_from_token(arg: &str) -> CargoSubcommand {
    builtin_command(arg).map_or(CargoSubcommand::Other, |command| command.subcommand)
}

/// Extract the raw cargo subcommand token from the argument list.
///
/// Unlike [`cargo_subcommand`], this preserves the literal token (e.g. `lint`,
/// `clippy`, `c`) so the command target-capability registry can reason about
/// aliases that the [`CargoSubcommand`] enum collapses to `Other`.
///
/// Returns `None` when no subcommand token is present (e.g. an unknown leading
/// flag or an early `--`).
pub(crate) fn cargo_subcommand_token(args: &[impl AsRef<str>]) -> Option<String> {
    subcommand_token_index(args)
        .and_then(|idx| args.get(idx))
        .map(|arg| arg.as_ref().to_string())
}

/// Index of the subcommand token in `args`, using the same skip rules as
/// [`cargo_subcommand_token`]. Used by alias expansion to replace the token in
/// place.
pub(crate) fn subcommand_token_index(args: &[impl AsRef<str>]) -> Option<usize> {
    let mut skip_next = false;
    for (idx, arg) in args.iter().map(AsRef::as_ref).enumerate() {
        if skip_next {
            skip_next = false;
            if arg == "--" {
                return None;
            }
            continue;
        }
        if arg == "--" {
            return None;
        }
        if arg.starts_with('+') {
            continue;
        }
        if is_cargo_no_value_flag(arg) {
            continue;
        }
        if cargo_flag_takes_value(arg) {
            skip_next = true;
            continue;
        }
        if cargo_flag_has_inline_value(arg) {
            continue;
        }

        if arg.starts_with("--") {
            return None;
        }

        if arg.starts_with('-') {
            return None;
        }

        return Some(idx);
    }

    None
}

pub(super) fn is_cargo_no_value_flag(arg: &str) -> bool {
    matches!(
        arg,
        "-q" | "--quiet"
            | "--frozen"
            | "--locked"
            | "--offline"
            | "-h"
            | "--help"
            | "-V"
            | "--version"
            | "--list"
            | "--verbose"
    ) || arg.starts_with("-v")
}

pub(super) fn cargo_flag_takes_value(arg: &str) -> bool {
    matches!(arg, "--color" | "--config" | "--explain" | "-C" | "-Z")
}

pub(super) fn cargo_flag_has_inline_value(arg: &str) -> bool {
    arg.starts_with("--color=")
        || arg.starts_with("--config=")
        || arg.starts_with("--explain=")
        || (arg.starts_with("-C") && arg.len() > 2)
        || (arg.starts_with("-Z") && arg.len() > 2)
}

/// Extract a rustup toolchain override from forwarded Cargo args.
///
/// Cargo accepts `+toolchain` before the subcommand. When cargo-fc installs
/// missing target components, rustup must receive the same override or it may
/// install targets into the default toolchain instead.
pub(crate) fn rustup_toolchain(args: &[impl AsRef<str>]) -> Option<String> {
    let first = args.first()?.as_ref();
    first
        .strip_prefix('+')
        .filter(|toolchain| !toolchain.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Debug, Clone, Copy)]
struct BuiltinCommand {
    canonical: &'static str,
    diagnostics_safe: bool,
    subcommand: CargoSubcommand,
}

/// Built-in Cargo subcommands cargo-fc understands without user config.
///
/// Every command in this table accepts Cargo's `--target` flag. Diagnostics
/// safety is narrower: broad config may enable diagnostics-only output for
/// commands that emit rustc JSON diagnostics by default.
fn builtin_command(token: &str) -> Option<BuiltinCommand> {
    let (canonical, diagnostics_safe, subcommand) = match token {
        "build" | "b" => ("build", true, CargoSubcommand::Build),
        "check" | "c" => ("check", true, CargoSubcommand::Check),
        "clippy" => ("clippy", true, CargoSubcommand::Lint),
        "doc" | "d" => ("doc", true, CargoSubcommand::Doc),
        "test" | "t" => ("test", false, CargoSubcommand::Test),
        "run" | "r" => ("run", false, CargoSubcommand::Run),
        _ => return None,
    };
    Some(BuiltinCommand {
        canonical,
        diagnostics_safe,
        subcommand,
    })
}

/// Return the canonical built-in command name for a token or short alias.
#[must_use]
pub(crate) fn builtin_canonical_command(token: &str) -> Option<&'static str> {
    builtin_command(token).map(|command| command.canonical)
}

/// Whether cargo-fc's built-in registry knows this command accepts `--target`.
#[must_use]
pub(crate) fn builtin_target_capability(token: Option<&str>) -> bool {
    token.and_then(builtin_command).is_some()
}

/// Whether broad config-driven diagnostics output is safe for this built-in command.
#[must_use]
pub(crate) fn builtin_diagnostics_safe(token: Option<&str>) -> bool {
    token
        .and_then(builtin_command)
        .is_some_and(|command| command.diagnostics_safe)
}

/// Whether cargo-fc should avoid capability hints for a known cargo command.
///
/// This is intentionally only a warning policy. These commands do not gain
/// target or diagnostics capability unless a user configures it explicitly.
#[must_use]
pub(crate) fn known_quiet_cargo_subcommand(token: Option<&str>) -> bool {
    let Some(token) = token else {
        return false;
    };
    matches!(
        token,
        "about"
            | "add"
            | "apk"
            | "asm"
            | "audit"
            | "binstall"
            | "bloat"
            | "bundle"
            | "cache"
            | "careful"
            | "chef"
            | "component"
            | "contract"
            | "cov"
            | "crev"
            | "criterion"
            | "deb"
            | "deny"
            | "dist"
            | "dylint"
            | "edit"
            | "espflash"
            | "expand"
            | "flamegraph"
            | "fuzz"
            | "geiger"
            | "generate"
            | "generate-rpm"
            | "hack"
            | "insta"
            | "info"
            | "install-update"
            | "lambda"
            | "leptos"
            | "license"
            | "llvm-cov"
            | "llvm-lines"
            | "machete"
            | "make"
            | "miri"
            | "modules"
            | "msrv"
            | "mutants"
            | "ndk"
            | "nextest"
            | "nm"
            | "objcopy"
            | "objdump"
            | "outdated"
            | "pgrx"
            | "profdata"
            | "public-api"
            | "public-items"
            | "quickinstall"
            | "readelf"
            | "readme"
            | "readobj"
            | "release"
            | "remove"
            | "rm"
            | "rpm"
            | "semver-checks"
            | "set-version"
            | "shear"
            | "shuttle"
            | "size"
            | "sort"
            | "sqlx"
            | "strip"
            | "sweep"
            | "tauri"
            | "tarpaulin"
            | "udeps"
            | "upgrade"
            | "vet"
            | "watch"
            | "wasi"
            | "whatfeatures"
            | "workspaces"
            | "wix"
            | "zigbuild"
    )
}

#[cfg(test)]
mod test {
    use super::{
        CargoSubcommand, builtin_diagnostics_safe, cargo_subcommand, cargo_subcommand_token,
        known_quiet_cargo_subcommand, rustup_toolchain,
    };
    use similar_asserts::assert_eq as sim_assert_eq;

    #[test]
    fn cargo_subcommand_detects_build_and_short_build() {
        sim_assert_eq!(cargo_subcommand(&["build"]), CargoSubcommand::Build);
        sim_assert_eq!(cargo_subcommand(&["b"]), CargoSubcommand::Build);
    }

    #[test]
    fn cargo_subcommand_detects_check_and_short_check() {
        sim_assert_eq!(cargo_subcommand(&["check"]), CargoSubcommand::Check);
        sim_assert_eq!(cargo_subcommand(&["c"]), CargoSubcommand::Check);
    }

    #[test]
    fn cargo_subcommand_detects_clippy_as_lint() {
        sim_assert_eq!(cargo_subcommand(&["clippy"]), CargoSubcommand::Lint);
    }

    #[test]
    fn cargo_subcommand_detects_test_and_short_test() {
        sim_assert_eq!(cargo_subcommand(&["test"]), CargoSubcommand::Test);
        sim_assert_eq!(cargo_subcommand(&["t"]), CargoSubcommand::Test);
    }

    #[test]
    fn cargo_subcommand_detects_doc_and_short_doc() {
        sim_assert_eq!(cargo_subcommand(&["doc"]), CargoSubcommand::Doc);
        sim_assert_eq!(cargo_subcommand(&["d"]), CargoSubcommand::Doc);
    }

    #[test]
    fn cargo_subcommand_detects_run_and_short_run() {
        sim_assert_eq!(cargo_subcommand(&["run"]), CargoSubcommand::Run);
        sim_assert_eq!(cargo_subcommand(&["r"]), CargoSubcommand::Run);
    }

    #[test]
    fn cargo_subcommand_skips_known_leading_cargo_flags_and_values() {
        let subcommand = cargo_subcommand(&[
            "+nightly",
            "--config",
            "net.retry=2",
            "--color=always",
            "-vv",
            "--frozen",
            "clippy",
            "build",
        ]);

        sim_assert_eq!(subcommand, CargoSubcommand::Lint);
    }

    #[test]
    fn cargo_subcommand_handles_help_and_version_flags_before_subcommand() {
        sim_assert_eq!(
            cargo_subcommand(&["--verbose", "--help", "clippy"]),
            CargoSubcommand::Lint
        );
        sim_assert_eq!(
            cargo_subcommand(&["-vv", "--frozen", "test"]),
            CargoSubcommand::Test
        );
    }

    #[test]
    fn cargo_subcommand_returns_other_for_unknown_leading_flag() {
        let subcommand = cargo_subcommand(&["--mystery-flag", "clippy"]);

        sim_assert_eq!(subcommand, CargoSubcommand::Other);
    }

    #[test]
    fn cargo_subcommand_token_stops_at_double_dash_after_value_flag() {
        sim_assert_eq!(cargo_subcommand_token(&["--config", "--", "clippy"]), None);
    }

    #[test]
    fn cargo_subcommand_treats_unknown_aliases_as_other() {
        sim_assert_eq!(cargo_subcommand(&["lint"]), CargoSubcommand::Other);
        sim_assert_eq!(cargo_subcommand(&["lint", "build"]), CargoSubcommand::Other);
    }

    #[test]
    fn cargo_subcommand_token_preserves_literal_token() {
        sim_assert_eq!(
            cargo_subcommand_token(&["clippy"]),
            Some("clippy".to_string())
        );
        sim_assert_eq!(cargo_subcommand_token(&["lint"]), Some("lint".to_string()));
        sim_assert_eq!(cargo_subcommand_token(&["c"]), Some("c".to_string()));
        sim_assert_eq!(
            cargo_subcommand_token(&["+nightly", "--frozen", "lint", "build"]),
            Some("lint".to_string())
        );
    }

    #[test]
    fn cargo_subcommand_token_none_for_missing_command() {
        let empty: [&str; 0] = [];
        sim_assert_eq!(cargo_subcommand_token(&empty), None);
        sim_assert_eq!(cargo_subcommand_token(&["--mystery-flag"]), None);
        sim_assert_eq!(cargo_subcommand_token(&["--"]), None);
    }

    #[test]
    fn rustup_toolchain_detects_cargo_toolchain_override() {
        sim_assert_eq!(
            rustup_toolchain(&["+nightly", "--frozen", "check"]),
            Some("nightly".to_string())
        );
    }

    #[test]
    fn rustup_toolchain_ignores_args_after_double_dash() {
        sim_assert_eq!(rustup_toolchain(&["run", "--", "+nightly"]), None);
    }

    #[test]
    fn rustup_toolchain_ignores_plus_values_after_leading_position() {
        sim_assert_eq!(rustup_toolchain(&["check", "--target-dir", "+out"]), None);
    }

    #[test]
    fn builtin_clippy_is_diagnostics_safe() {
        assert!(builtin_diagnostics_safe(Some("clippy")));
    }

    #[test]
    fn builtin_test_does_not_have_config_diagnostics_by_default() {
        assert!(!builtin_diagnostics_safe(Some("test")));
    }

    #[test]
    fn known_quiet_subcommands_do_not_gain_builtin_capabilities() {
        for token in [
            "add",
            "generate",
            "license",
            "msrv",
            "nextest",
            "machete",
            "objdump",
            "public-api",
            "udeps",
            "leptos",
            "audit",
        ] {
            assert!(known_quiet_cargo_subcommand(Some(token)));
            assert!(!super::builtin_target_capability(Some(token)));
            assert!(!builtin_diagnostics_safe(Some(token)));
        }
        assert!(!known_quiet_cargo_subcommand(Some("clippy")));
    }
}
