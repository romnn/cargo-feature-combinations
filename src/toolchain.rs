//! `+toolchain` overrides taken from the forwarded Cargo args.
//!
//! Cargo never sees `+toolchain` itself: rustup's proxy consumes it from
//! `argv[1]` and then execs the real Cargo binary of that toolchain. Two things
//! follow from that. A cargo alias body cannot carry a toolchain, because by the
//! time `cargo unused` expands to `fc +nightly udeps` the proxy has already
//! dispatched. And forwarding the token to a child is useless, because Cargo
//! sets `CARGO` to its own toolchain-pinned binary for external subcommands, and
//! that binary rejects `+nightly` as an unknown subcommand.
//!
//! cargo-fc therefore consumes the token and re-applies it to every child
//! invocation, which is what makes `unused = "fc +nightly udeps ..."` work as an
//! alias.

use crate::config::ResolvedEnv;
use crate::print_warning;
use color_eyre::eyre::{self, WrapErr};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Adapter for the toolchain manager that resolves a `+toolchain` override.
pub(crate) trait ToolchainResolver {
    /// Return the `cargo` binary of `toolchain`.
    ///
    /// `None` means the toolchain manager is not installed at all, so the
    /// override can not be interpreted and must be left in the forwarded args.
    ///
    /// # Errors
    ///
    /// Returns an error if the toolchain manager is present but can not resolve
    /// the toolchain, for example because it is not installed.
    fn cargo(&self, toolchain: &str) -> eyre::Result<Option<PathBuf>>;
}

/// Production toolchain resolver backed by `rustup`.
#[derive(Debug)]
pub(crate) struct RustupToolchainResolver;

impl ToolchainResolver for RustupToolchainResolver {
    fn cargo(&self, toolchain: &str) -> eyre::Result<Option<PathBuf>> {
        let output = match Command::new("rustup")
            .args(["which", "--toolchain", toolchain, "cargo"])
            .output()
        {
            Ok(output) => output,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                print_warning!(
                    "could not find `rustup` to resolve toolchain `{toolchain}`; forwarding `+{toolchain}` to the build driver unchanged"
                );
                return Ok(None);
            }
            Err(err) => {
                return Err(err).wrap_err_with(|| {
                    format!("failed to invoke rustup to resolve toolchain `{toolchain}`")
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eyre::bail!(
                "rustup could not resolve toolchain `{toolchain}`: {}",
                stderr.trim(),
            );
        }

        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            eyre::bail!("rustup reported no cargo binary for toolchain `{toolchain}`");
        }
        Ok(Some(PathBuf::from(path)))
    }
}

/// A `+toolchain` override cargo-fc consumed from the forwarded Cargo args.
#[derive(Debug, Clone)]
pub(crate) struct ToolchainOverride {
    name: String,
    cargo: PathBuf,
}

impl ToolchainOverride {
    /// The toolchain name as the user spelled it, without the leading `+`.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The `cargo` binary of the overridden toolchain.
    pub(crate) fn cargo(&self) -> &Path {
        &self.cargo
    }

    /// Pin one child invocation to the overridden toolchain.
    ///
    /// `RUSTUP_TOOLCHAIN` is what rustup's proxies read, so it decides which
    /// `rustc` the child resolves and which toolchain any nested Cargo runs
    /// under. `CARGO` pins the same toolchain for wrapper drivers that spawn
    /// Cargo themselves; without it they would inherit the toolchain-pinned
    /// `CARGO` that launched cargo-fc. Both yield to explicit child `env`
    /// configuration.
    pub(crate) fn apply_to(&self, command: &mut Command, env: &ResolvedEnv) {
        if !env.mentions("RUSTUP_TOOLCHAIN") {
            command.env("RUSTUP_TOOLCHAIN", &self.name);
        }
        if !env.mentions("CARGO") {
            command.env("CARGO", &self.cargo);
        }
    }
}

/// Split a leading `+toolchain` off `args` and resolve it.
///
/// The token is only removed once it resolved, so a setup without rustup keeps
/// forwarding it to the build driver exactly as before.
///
/// # Errors
///
/// Returns an error if the toolchain can not be resolved.
pub(crate) fn take_override(
    args: &mut Vec<&str>,
    resolver: &impl ToolchainResolver,
) -> eyre::Result<Option<ToolchainOverride>> {
    let Some(name) = crate::cli::rustup_toolchain(args) else {
        return Ok(None);
    };
    let Some(cargo) = resolver.cargo(&name)? else {
        return Ok(None);
    };
    args.remove(0);
    Ok(Some(ToolchainOverride { name, cargo }))
}

#[cfg(test)]
mod test {
    use super::{ToolchainOverride, ToolchainResolver, take_override};
    use crate::config::ResolvedEnv;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::process::Command;

    /// Resolver that answers every toolchain the same way, without a process.
    struct FakeResolver(fn(&str) -> eyre::Result<Option<PathBuf>>);

    impl ToolchainResolver for FakeResolver {
        fn cargo(&self, toolchain: &str) -> eyre::Result<Option<PathBuf>> {
            (self.0)(toolchain)
        }
    }

    fn resolving() -> FakeResolver {
        FakeResolver(|toolchain| {
            Ok(Some(PathBuf::from(format!(
                "/toolchains/{toolchain}/cargo"
            ))))
        })
    }

    fn without_rustup() -> FakeResolver {
        FakeResolver(|_| Ok(None))
    }

    fn failing() -> FakeResolver {
        FakeResolver(|toolchain| eyre::bail!("toolchain `{toolchain}` is not installed"))
    }

    fn toolchain_override() -> ToolchainOverride {
        ToolchainOverride {
            name: "nightly".to_string(),
            cargo: PathBuf::from("/toolchains/nightly/bin/cargo"),
        }
    }

    fn child_env(command: &Command) -> BTreeMap<&OsStr, Option<&OsStr>> {
        command.get_envs().collect()
    }

    fn resolved_env(value: serde_json::Value) -> eyre::Result<ResolvedEnv> {
        let patch: crate::config::EnvPatch = serde_json::from_value(value)?;
        let operations = crate::config::env::combine_env_patches("test", [("", &patch)])?
            .ok_or_else(|| eyre::eyre!("test env patch unexpectedly absent"))?;
        let mut resolved = ResolvedEnv::default();
        resolved.apply_patch(&operations);
        Ok(resolved)
    }

    #[test]
    fn resolved_toolchain_is_removed_from_the_forwarded_args() -> eyre::Result<()> {
        let mut args = vec!["+nightly", "udeps", "--all-targets"];

        let toolchain = take_override(&mut args, &resolving())?
            .ok_or_else(|| eyre::eyre!("expected a resolved toolchain"))?;

        sim_assert_eq!(toolchain.name(), "nightly");
        sim_assert_eq!(
            toolchain.cargo(),
            PathBuf::from("/toolchains/nightly/cargo")
        );
        sim_assert_eq!(args, vec!["udeps", "--all-targets"]);
        Ok(())
    }

    #[test]
    fn args_without_a_toolchain_are_left_alone() -> eyre::Result<()> {
        let mut args = vec!["udeps", "--all-targets"];

        let toolchain = take_override(&mut args, &failing())?;

        assert!(toolchain.is_none());
        sim_assert_eq!(args, vec!["udeps", "--all-targets"]);
        Ok(())
    }

    #[test]
    fn toolchain_stays_in_the_args_without_a_toolchain_manager() -> eyre::Result<()> {
        let mut args = vec!["+nightly", "udeps"];

        let toolchain = take_override(&mut args, &without_rustup())?;

        assert!(toolchain.is_none());
        sim_assert_eq!(args, vec!["+nightly", "udeps"]);
        Ok(())
    }

    #[test]
    fn unresolvable_toolchain_fails_instead_of_running_the_wrong_one() {
        let mut args = vec!["+nightly", "udeps"];

        let err = take_override(&mut args, &failing())
            .expect_err("an unresolvable toolchain must not be ignored");

        assert!(err.to_string().contains("nightly"), "{err}");
    }

    #[test]
    fn toolchain_pins_both_rustup_and_cargo_for_the_child() {
        let mut command = Command::new("cargo");

        toolchain_override().apply_to(&mut command, &ResolvedEnv::default());

        let env = child_env(&command);
        sim_assert_eq!(
            env.get(OsStr::new("RUSTUP_TOOLCHAIN")),
            Some(&Some(OsStr::new("nightly")))
        );
        sim_assert_eq!(
            env.get(OsStr::new("CARGO")),
            Some(&Some(OsStr::new("/toolchains/nightly/bin/cargo")))
        );
    }

    #[test]
    fn configured_child_env_wins_over_the_toolchain_override() -> eyre::Result<()> {
        let env = resolved_env(serde_json::json!({
            "add": { "RUSTUP_TOOLCHAIN": "beta" },
            "remove": ["CARGO"],
        }))?;
        let mut command = Command::new("cargo");
        env.apply_to(&mut command);

        toolchain_override().apply_to(&mut command, &env);

        let child_env = child_env(&command);
        sim_assert_eq!(
            child_env.get(OsStr::new("RUSTUP_TOOLCHAIN")),
            Some(&Some(OsStr::new("beta")))
        );
        sim_assert_eq!(child_env.get(OsStr::new("CARGO")), Some(&None));
        Ok(())
    }
}
