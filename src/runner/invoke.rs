//! One Cargo child invocation: environment, spawning, and output capture.

use crate::cli::{CargoSubcommand, cargo_subcommand};
use crate::config::{ResolvedEnv, ResolvedFlags};
use crate::print_warning;
use crate::toolchain::ToolchainOverride;

use color_eyre::eyre;
use itertools::Itertools;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, IsTerminal as _, Write};
use std::process;
use termcolor::{StandardStream, WriteColor};

use super::summary::{Summary, error_counts, warning_counts};
use super::{CYAN, DIMMED, Invocation, Progress, RunContext};

/// Force colored output on a subprocess.
///
/// Subprocesses see a pipe (not a TTY) on stderr because we capture their
/// output, so most tools auto-disable color. We counteract this with two env
/// vars:
///
/// - `CARGO_TERM_COLOR=always` — Cargo's documented env var, equivalent to
///   `[term] color = "always"`. Forces colors even when stderr is piped and
///   propagates `--color=always` to rustc. Stable since Rust 1.42.
/// - `FORCE_COLOR=1` — widely adopted convention (Node.js, Python, Ruby, many
///   Rust crates via `anstream`).
///
/// A more universal fix would be to allocate a pseudo-TTY (e.g. via
/// `portable-pty`) so that `isatty()` returns true in the subprocess, but the
/// env-var approach covers the vast majority of real-world cases.
fn force_color(cmd: &mut process::Command, env: &ResolvedEnv) {
    // Gate on stderr: captured child compiler output is re-streamed to our
    // stderr, and cargo itself keys auto-color off stderr. This can still put
    // ANSI into a redirected stdout for `run`/`test` program output (child
    // stdout is inherited), but favors the dominant check/build/clippy case.
    let no_color = env.effective_var("NO_COLOR");
    let cargo_term_color = env.effective_var("CARGO_TERM_COLOR");
    let force_color = env.effective_var("FORCE_COLOR");
    let decision = force_color_env(
        std::io::stderr().is_terminal(),
        no_color.as_deref(),
        cargo_term_color.as_deref(),
        force_color.as_deref(),
    );
    if decision.set_cargo_term_color {
        cmd.env("CARGO_TERM_COLOR", "always");
    }
    if decision.set_force_color {
        cmd.env("FORCE_COLOR", "1");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ForceColorEnv {
    set_cargo_term_color: bool,
    set_force_color: bool,
}

fn force_color_env(
    stderr_is_terminal: bool,
    no_color: Option<&std::ffi::OsStr>,
    cargo_term_color: Option<&std::ffi::OsStr>,
    force_color: Option<&std::ffi::OsStr>,
) -> ForceColorEnv {
    let forcing_enabled = stderr_is_terminal && no_color.is_none();
    ForceColorEnv {
        set_cargo_term_color: forcing_enabled && cargo_term_color.is_none(),
        set_force_color: forcing_enabled && force_color.is_none(),
    }
}

fn driver_label(driver: Option<&str>) -> &str {
    driver.unwrap_or("cargo")
}

fn warn_missing_driver(driver: Option<&str>) {
    match driver {
        Some("cargo-zigbuild") => print_warning!(
            "build driver `cargo-zigbuild` was not found; install cargo-zigbuild and zig to cross-compile, or set --driver <bin> / [workspace.metadata.cargo-fc].driver to another driver (use `cargo` to force plain Cargo)"
        ),
        Some(driver) => print_warning!(
            "build driver `{driver}` was not found; install it, or set --driver <bin> / [workspace.metadata.cargo-fc].driver to another driver"
        ),
        None => print_warning!(
            "could not find `cargo`; install Cargo or set the CARGO environment variable"
        ),
    }
}

fn spawn_cargo_command(
    mut cmd: process::Command,
    driver: Option<&str>,
    capture_stdout: bool,
) -> eyre::Result<process::Child> {
    if capture_stdout {
        cmd.stdout(process::Stdio::piped());
    }
    cmd.stderr(process::Stdio::piped());

    match cmd.spawn() {
        Ok(child) => Ok(child),
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                warn_missing_driver(driver);
            }
            Err(eyre::eyre!(
                "failed to invoke build driver `{}`: {err}",
                driver_label(driver),
            ))
        }
    }
}

/// Result of processing cargo output for a single feature combination.
pub(crate) struct ProcessResult {
    pub num_warnings: usize,
    pub num_errors: usize,
    pub num_suppressed: usize,
    pub output: Vec<u8>,
}

/// Capture cargo stderr, optionally tee-ing it to the terminal.
///
/// In summary-only mode the output is buffered only; otherwise it is streamed
/// to stderr while also being captured for later analysis.
fn capture_stderr(
    child: &mut process::Child,
    summary_only: bool,
    stderr: &mut StandardStream,
) -> io::Result<ProcessResult> {
    let output_buffer = Vec::<u8>::new();
    let mut output_cursor = io::Cursor::new(output_buffer);

    if let Some(proc_stderr) = child.stderr.take() {
        let mut proc_reader = io::BufReader::new(proc_stderr);
        if summary_only {
            io::copy(&mut proc_reader, &mut output_cursor)?;
        } else {
            let mut tee_reader = crate::tee::Reader::new(proc_reader, stderr, true);
            io::copy(&mut tee_reader, &mut output_cursor)?;
        }
    } else {
        print_warning!("failed to redirect child stderr");
    }

    let stripped = strip_ansi_escapes::strip(output_cursor.get_ref());
    let stripped = String::from_utf8_lossy(&stripped);
    let num_warnings = warning_counts(&stripped).sum::<usize>();
    let num_errors = error_counts(&stripped).sum::<usize>();

    Ok(ProcessResult {
        num_warnings,
        num_errors,
        num_suppressed: 0,
        output: output_cursor.into_inner(),
    })
}

fn print_package_cmd(
    inv: &Invocation<'_>,
    all_args: &[&str],
    diagnostics_only: bool,
    driver: Option<&str>,
    toolchain: Option<&str>,
    progress: Progress,
    stderr: &mut StandardStream,
) {
    let compact = inv.flags.summary_only || diagnostics_only;
    if !compact {
        let _ = writeln!(stderr);
    }
    let subcommand = cargo_subcommand(all_args);
    stderr.set_color(&CYAN).ok();
    match subcommand {
        CargoSubcommand::Test => {
            let _ = write!(stderr, "     Testing ");
        }
        CargoSubcommand::Doc => {
            let _ = write!(stderr, "     Documenting ");
        }
        CargoSubcommand::Lint => {
            let _ = write!(stderr, "     Linting ");
        }
        CargoSubcommand::Check => {
            let _ = write!(stderr, "     Checking ");
        }
        CargoSubcommand::Run => {
            let _ = write!(stderr, "     Running ");
        }
        CargoSubcommand::Build => {
            let _ = write!(stderr, "     Building ");
        }
        CargoSubcommand::Other => {
            let _ = write!(stderr, "     ");
        }
    }
    // The progress counter sits immediately to the left of the package name.
    // It is always dimmed; for known subcommands only the verb is cyan, while
    // for unknown subcommands (Other) the rest of the line stays cyan so the
    // header remains visually distinct.
    stderr.set_color(&DIMMED).ok();
    let _ = write!(
        stderr,
        "[{idx:>width$}/{total}]",
        idx = progress.index,
        total = progress.total,
        width = progress.width,
    );
    if subcommand == CargoSubcommand::Other {
        stderr.set_color(&CYAN).ok();
    } else {
        stderr.reset().ok();
    }
    let _ = write!(
        stderr,
        " {} ( {}features = [{}] )",
        inv.package.name,
        inv.summary_target.field_prefix(),
        inv.features.iter().join(", ")
    );
    if inv.flags.verbose {
        // Spelled the way a user would type it, even though the override is
        // applied through the environment rather than an argument.
        let toolchain = toolchain
            .map(|name| format!(" +{name}"))
            .unwrap_or_default();
        let _ = write!(
            stderr,
            " [{}{toolchain} {}]",
            driver_label(driver),
            all_args.join(" "),
        );
    }
    stderr.reset().ok();
    let _ = writeln!(stderr);
    if !compact {
        let _ = writeln!(stderr);
    }
}

/// Result of [`run_single_combination`] for one feature combination.
pub(super) struct CombinationResult {
    pub(super) summary: Summary,
    /// Raw (colored) output buffer for potential `--fail-fast` dumping.
    pub(super) colored_output: Vec<u8>,
    pub(super) flags: ResolvedFlags,
}

/// Format the generated `--features` flag for one combination.
///
/// Features are qualified as `<package>/<feature>` so they keep selecting the
/// planned package's features even when forwarded Cargo arguments broaden the
/// package selection (for example `--workspace`).
fn feature_selection_flag(package_name: &str, features: &[String]) -> String {
    format!(
        "--features={}",
        features
            .iter()
            .map(|feature| format!("{package_name}/{feature}"))
            .join(",")
    )
}

/// Build the child process for one invocation: the program to spawn and the
/// environment it runs with, but not yet its arguments or working directory.
fn cargo_command(inv: &Invocation<'_>, toolchain: Option<&ToolchainOverride>) -> process::Command {
    let cargo: std::ffi::OsString = match (inv.driver, toolchain) {
        (Some(driver), _) => std::ffi::OsString::from(driver),
        // `$CARGO` is the binary of whatever toolchain launched cargo-fc, so it
        // would silently ignore the requested override.
        (None, Some(toolchain)) => toolchain.cargo().as_os_str().to_owned(),
        (None, None) => std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
    };
    let mut cmd = process::Command::new(&cargo);
    inv.env.apply_to(&mut cmd);
    if let Some(toolchain) = toolchain {
        toolchain.apply_to(&mut cmd, inv.env);
    }
    // Propagate the resolved driver to the child via `CARGO_DRIVER` so wrapper
    // aliases (e.g. `lint = "run --package clippy-wrapper -- lint"`) can spawn
    // the same driver for their inner Cargo invocation. Without this, the inner
    // command falls back to plain `cargo` and native-C deps fail to
    // cross-compile even though this outer invocation used `cargo-zigbuild`.
    apply_cargo_driver(&mut cmd, &cargo, inv.env);
    force_color(&mut cmd, inv.env);

    if inv.flags.errors_only {
        apply_errors_only_rustflags(&mut cmd, inv.env);
    }
    cmd
}

/// Run a single cargo invocation for one feature combination and collect
/// its output into a [`Summary`].
pub(super) fn run_single_combination(
    inv: &Invocation<'_>,
    ctx: &RunContext<'_>,
    progress: Progress,
    seen_diagnostics: &mut HashSet<String>,
    stderr: &mut StandardStream,
) -> eyre::Result<CombinationResult> {
    let package = inv.package;
    let features = inv.features;
    let mut diagnostics_only = inv.flags.diagnostics_only;
    let mut dedupe = inv.flags.dedupe;
    if ctx
        .invocation_args
        .has_message_format_arg_for_generated_args()
    {
        // `--message-format` is a forwarded Cargo argument, so it wins at
        // execution time instead of becoming part of cargo-fc config
        // resolution.
        diagnostics_only = false;
        dedupe = false;
    }
    // We set the command working dir to the package manifest parent dir.
    // This works well for now, but one could also consider `--manifest-path` or `-p`
    let Some(working_dir) = package.manifest_path.parent() else {
        eyre::bail!(
            "could not find parent dir of package {}",
            package.manifest_path.to_string()
        )
    };

    let mut cmd = cargo_command(inv, ctx.toolchain);

    let features_flag = feature_selection_flag(package.name.as_str(), features);
    let mut generated_args = Vec::new();
    if diagnostics_only {
        generated_args.push(crate::diagnostics_only::MESSAGE_FORMAT);
    }
    for triple in inv.inject_targets {
        generated_args.push("--target");
        generated_args.push(triple.as_str());
    }
    generated_args.push("--no-default-features");
    generated_args.push(&features_flag);
    let args = ctx.invocation_args.with_generated_args(generated_args);
    print_package_cmd(
        inv,
        &args,
        diagnostics_only,
        inv.driver,
        ctx.toolchain.map(ToolchainOverride::name),
        progress,
        stderr,
    );

    cmd.args(&args).current_dir(working_dir);
    let mut child = spawn_cargo_command(cmd, inv.driver, diagnostics_only)?;

    let mut result = if diagnostics_only {
        crate::diagnostics_only::process_output(
            &mut child,
            inv.flags.summary_only,
            dedupe,
            seen_diagnostics,
            stderr,
        )?
    } else {
        capture_stderr(&mut child, inv.flags.summary_only, stderr)?
    };

    let exit_status = child.wait()?;

    // Print per-combination dedup note after diagnostics
    if result.num_suppressed > 0 && !inv.flags.summary_only {
        stderr.set_color(&CYAN).ok();
        let _ = write!(stderr, "       Note ");
        stderr.reset().ok();
        let _ = writeln!(
            stderr,
            "{} duplicate diagnostic{} suppressed",
            result.num_suppressed,
            if result.num_suppressed > 1 { "s" } else { "" },
        );
    }

    let fail = !exit_status.success();

    // In diagnostics-only mode, cargo-level failures (bad CLI arguments,
    // dependency resolution errors, …) produce no JSON diagnostics — so the
    // user would only see "FAIL … 0 errors, 0 warnings" with no explanation.
    // When that happens the output buffer holds the captured stderr which is
    // the only clue about what went wrong. Print it unconditionally (even in
    // --summary-only mode) so the failure is never silent.
    if diagnostics_only && fail && result.num_errors == 0 && !result.output.is_empty() {
        stderr.write_all(&result.output)?;
        stderr.flush().ok();
        // Clear the buffer so the --fail-fast dump does not print it a
        // second time.
        result.output.clear();
    }

    let pedantic_fail = inv.flags.pedantic && (result.num_errors > 0 || result.num_warnings > 0);

    let summary = Summary {
        features: features.to_vec(),
        target: inv.summary_target.clone(),
        num_errors: result.num_errors,
        num_warnings: result.num_warnings,
        num_suppressed: result.num_suppressed,
        package_name: package.name.to_string(),
        exit_code: exit_status.code(),
        pedantic_success: !(fail || pedantic_fail),
        equivalent_to: None,
    };

    Ok(CombinationResult {
        summary,
        colored_output: result.output,
        flags: inv.flags,
    })
}

fn apply_cargo_driver(cmd: &mut process::Command, cargo: &std::ffi::OsStr, env: &ResolvedEnv) {
    if !env.mentions("CARGO_DRIVER") {
        cmd.env("CARGO_DRIVER", cargo);
    }
}

fn apply_errors_only_rustflags(cmd: &mut process::Command, env: &ResolvedEnv) {
    let (key, value) = errors_only_rustflags_env(
        env.effective_var("CARGO_ENCODED_RUSTFLAGS"),
        env.effective_var("RUSTFLAGS"),
    );
    cmd.env(key, value);
}

fn errors_only_rustflags_env(
    encoded: Option<OsString>,
    rustflags: Option<OsString>,
) -> (&'static str, OsString) {
    const ALLOW_WARNINGS: &str = "-Awarnings";
    if let Some(mut value) = encoded {
        if !value.is_empty() {
            value.push("\x1f");
        }
        value.push(ALLOW_WARNINGS);
        return ("CARGO_ENCODED_RUSTFLAGS", value);
    }

    let mut value = rustflags.unwrap_or_default();
    if !value.is_empty() {
        value.push(" ");
    }
    value.push(ALLOW_WARNINGS);
    ("RUSTFLAGS", value)
}

#[cfg(test)]
mod test {
    use super::{
        apply_cargo_driver, apply_errors_only_rustflags, errors_only_rustflags_env,
        feature_selection_flag, force_color_env,
    };
    use crate::config::ResolvedEnv;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::ffi::OsString;
    use std::process::Command;

    fn string_vec(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(String::from).collect()
    }

    #[test]
    fn feature_selection_is_qualified_to_the_planned_package() {
        let features = string_vec(&["backend-a", "shared"]);

        sim_assert_eq!(
            feature_selection_flag("my-crate", &features),
            "--features=my-crate/backend-a,my-crate/shared"
        );
        sim_assert_eq!(feature_selection_flag("my-crate", &[]), "--features=");
    }

    #[test]
    fn errors_only_appends_after_plain_rustflags() {
        let (key, value) =
            errors_only_rustflags_env(None, Some(OsString::from("-Dwarnings --cfg ci")));

        sim_assert_eq!(key, "RUSTFLAGS");
        sim_assert_eq!(value, OsString::from("-Dwarnings --cfg ci -Awarnings"));
    }

    #[test]
    fn errors_only_extends_encoded_rustflags_when_present() {
        let (key, value) = errors_only_rustflags_env(
            Some(OsString::from("-Dwarnings\x1f--cfg=ci")),
            Some(OsString::from("-Dwarnings")),
        );

        sim_assert_eq!(key, "CARGO_ENCODED_RUSTFLAGS");
        sim_assert_eq!(
            value,
            OsString::from("-Dwarnings\x1f--cfg=ci\x1f-Awarnings")
        );
    }

    #[test]
    fn errors_only_handles_empty_encoded_rustflags() {
        let (key, value) = errors_only_rustflags_env(Some(OsString::new()), None);

        sim_assert_eq!(key, "CARGO_ENCODED_RUSTFLAGS");
        sim_assert_eq!(value, OsString::from("-Awarnings"));
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
    fn errors_only_composes_with_resolved_rustflags() -> eyre::Result<()> {
        let env = resolved_env(serde_json::json!({
            "add": { "RUSTFLAGS": "-Dwarnings --cfg ci" },
            "remove": ["CARGO_ENCODED_RUSTFLAGS"],
        }))?;
        let mut command = Command::new("cargo");
        env.apply_to(&mut command);

        apply_errors_only_rustflags(&mut command, &env);

        let rustflags = command
            .get_envs()
            .find(|(key, _)| *key == "RUSTFLAGS")
            .and_then(|(_, value)| value);
        sim_assert_eq!(
            rustflags,
            Some(std::ffi::OsStr::new("-Dwarnings --cfg ci -Awarnings"))
        );
        Ok(())
    }

    #[test]
    fn color_forcing_honors_effective_user_values() -> eyre::Result<()> {
        let configured = resolved_env(serde_json::json!({
            "add": { "CARGO_TERM_COLOR": "never" },
            "remove": ["NO_COLOR", "FORCE_COLOR"],
        }))?;

        let no_color = configured.effective_var("NO_COLOR");
        let cargo_term_color = configured.effective_var("CARGO_TERM_COLOR");
        let force_color = configured.effective_var("FORCE_COLOR");
        let decision = force_color_env(
            true,
            no_color.as_deref(),
            cargo_term_color.as_deref(),
            force_color.as_deref(),
        );

        assert!(!decision.set_cargo_term_color);
        assert!(decision.set_force_color);
        Ok(())
    }

    #[test]
    fn configured_cargo_driver_is_not_clobbered() -> eyre::Result<()> {
        let env = resolved_env(serde_json::json!({
            "add": { "CARGO_DRIVER": "configured-driver" },
        }))?;
        let mut command = Command::new("cargo");
        env.apply_to(&mut command);

        apply_cargo_driver(&mut command, std::ffi::OsStr::new("resolved-driver"), &env);

        let driver = command
            .get_envs()
            .find(|(key, _)| *key == "CARGO_DRIVER")
            .and_then(|(_, value)| value);
        sim_assert_eq!(driver, Some(std::ffi::OsStr::new("configured-driver")));
        Ok(())
    }
}
