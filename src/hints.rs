//! User-facing hints and warnings that explain mode fallbacks or ignored input.

use crate::cli::{self, Command, Options};
use crate::config;
use crate::plan;
use crate::{print_note, print_warning, ws_metadata_section};
use itertools::Itertools;
use std::collections::{BTreeSet, HashSet};

pub(crate) fn warn_unmatched_config_exclude_packages(
    exclude: &HashSet<String>,
    metadata: &cargo_metadata::Metadata,
) {
    let available = metadata
        .workspace_packages()
        .iter()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    for package in exclude
        .iter()
        .filter(|package| !available.contains(package.as_str()))
        .collect::<BTreeSet<_>>()
    {
        print_warning!(
            "configured exclude_packages entry `{package}` did not match any workspace member"
        );
    }
}

/// Warn once when configured targets were skipped only because of the built-in
/// unknown-command default.
///
/// `matrix` is not a forwarded cargo command: it always uses configured target
/// planning.
pub(crate) fn warn_if_configured_targets_ignored(
    options: &Options,
    raw_token: Option<&str>,
    resolved_token: Option<&str>,
    ws_config: &config::WorkspaceConfig,
    ws_key: &str,
    selected: &[plan::targets::SelectedPackage<'_>],
) {
    // `--no-targets` deliberately ignores configured target lists and falls back
    // to Cargo's default single target, so it should not also warn.
    if options.flags.no_targets == Some(true) {
        return;
    }

    if selected
        .iter()
        .any(|package| !package.ignore_configured_targets)
    {
        return;
    }

    // `matrix` is not a forwarded cargo command: it always uses configured
    // target planning.
    if matches!(options.command, Some(Command::FeatureMatrix { .. })) {
        return;
    }

    let has_implicitly_skipped_configured_targets = selected.iter().any(|package| {
        !package.target_decision_explicit
            && (target_patch_configures_non_empty_list(ws_config.base.settings.targets.as_ref())
                || target_patch_configures_non_empty_list(
                    package.config.base.settings.targets.as_ref(),
                ))
    });
    let warning_token = raw_token.or(resolved_token);
    if cli::known_quiet_cargo_subcommand(raw_token)
        || cli::known_quiet_cargo_subcommand(resolved_token)
    {
        return;
    }
    if has_implicitly_skipped_configured_targets
        && let Some(token) = warning_token.filter(|t| !t.is_empty())
    {
        print_warning!(
            "not passing configured targets to cargo command `{token}` because it has no targets capability"
        );
        eprintln!(
            "hint: add [{}.subcommands.{token}] expand_targets = true if this command accepts --target, or expand_targets = false to silence this warning",
            ws_metadata_section(ws_key),
        );
    }
}

fn target_patch_configures_non_empty_list(patch: Option<&config::patch::TargetListPatch>) -> bool {
    patch.is_some_and(|patch| {
        // A non-empty override or any added targets means this configures
        // targets that a no-expand command could skip.
        patch
            .override_value()
            .is_some_and(|targets| !targets.is_empty())
            || !patch.add_values().is_empty()
    })
}

pub(crate) fn warn_ignored_diagnostics_config(
    options: &Options,
    raw_token: Option<&str>,
    resolved_token: Option<&str>,
    ws_key: &str,
    plan_set: &plan::execution::ExecutionPlanSet<'_>,
) {
    let cli_flags = options.flags;
    if cli::known_quiet_cargo_subcommand(raw_token)
        || cli::known_quiet_cargo_subcommand(resolved_token)
    {
        return;
    }
    if cli_flags.diagnostics_only != Some(true)
        && cli_flags.dedupe != Some(true)
        && plan_set.plans.iter().any(|plan| {
            plan.package_plans.iter().any(|package_plan| {
                package_plan.ignored_diagnostics_config && !package_plan.flags.diagnostics_only
            })
        })
        && let Some(token) = raw_token.or(resolved_token).filter(|t| !t.is_empty())
    {
        print_warning!(
            "not enabling configured diagnostics-only/dedupe for cargo command `{token}` because it is not diagnostics-safe by default"
        );
        eprintln!(
            "hint: set [{}.subcommands.{token}] diagnostics_only = true or dedupe = true to force diagnostics mode for this command, or diagnostics_only = false to silence this warning",
            ws_metadata_section(ws_key),
        );
    }
}

/// Emit one note for run-only flags that have no effect on `cargo fc matrix`
/// output, so the silent no-op is visible to the user.
pub(crate) fn note_matrix_noop_flags(options: &Options) {
    let flags = options.flags;
    let mut ignored = Vec::new();
    for (enabled, label) in [
        (flags.diagnostics_only, "--diagnostics-only"),
        (flags.dedupe, "--dedupe"),
        (flags.summary_only, "--summary-only"),
        (flags.fail_fast, "--fail-fast"),
        (flags.errors_only, "--errors-only"),
        (flags.pedantic, "--pedantic"),
        (flags.show_pruned, "--show-pruned"),
        (flags.aggregate_targets, "--aggregate-targets"),
        (flags.install_missing_targets, "--install-missing-targets"),
    ] {
        if enabled == Some(true) {
            ignored.push(label);
        }
    }
    if options.driver.is_some() {
        ignored.push("--driver");
    }
    if !ignored.is_empty() {
        print_note!(
            "{} {} no effect for matrix output",
            ignored.iter().join(", "),
            if ignored.len() == 1 { "has" } else { "have" },
        );
    }
}
