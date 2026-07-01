use super::schema::{CommandCapabilities, WorkspaceConfig};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

macro_rules! for_each_flag_config_field {
    ($callback:ident) => {
        $callback!(
            summary_only,
            diagnostics_only,
            dedupe,
            verbose,
            pedantic,
            errors_only,
            packages_only,
            fail_fast,
            no_prune_implied,
            prune_implied,
            show_pruned,
            aggregate_targets,
            no_targets,
            install_missing_targets,
            only_packages_with_lib_target,
        );
    };
}

// Fields copied by the default `unwrap_or(false)` rule. Diagnostics and
// normalized pruning are resolved explicitly because they have implications.
macro_rules! for_each_simple_resolved_flag_field {
    ($callback:ident) => {
        $callback!(
            summary_only,
            verbose,
            pedantic,
            errors_only,
            packages_only,
            fail_fast,
            show_pruned,
            aggregate_targets,
            no_targets,
            install_missing_targets,
            only_packages_with_lib_target,
        );
    };
}

macro_rules! define_flag_keys {
    ($($field:ident),+ $(,)?) => {
        pub(crate) const FLAG_KEYS: &[&str] = &[$(stringify!($field),)+ "dedup"];
    };
}
for_each_flag_config_field!(define_flag_keys);

/// Raw configurable cargo-fc flag defaults.
///
/// Each field is tri-state: absent means "inherit from the next broader
/// scope", while `true`/`false` explicitly override broader config. CLI flags
/// are converted into this same shape with `true` entries and overlaid last.
#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlagConfig {
    /// Whether to hide cargo output and only print summaries.
    #[serde(default)]
    pub summary_only: Option<bool>,
    /// Whether to show only rendered compiler diagnostics.
    #[serde(default)]
    pub diagnostics_only: Option<bool>,
    /// Whether to deduplicate rendered diagnostics across combinations.
    ///
    /// Accept `dedup` as a spelling alias for users who mirror the CLI alias.
    #[serde(default, alias = "dedup")]
    pub dedupe: Option<bool>,
    /// Whether to print cargo-fc's verbose command header details.
    #[serde(default)]
    pub verbose: Option<bool>,
    /// Whether warnings should fail summaries and fail-fast checks.
    #[serde(default)]
    pub pedantic: Option<bool>,
    /// Whether to allow warnings and show errors only.
    #[serde(default)]
    pub errors_only: Option<bool>,
    /// Whether matrix output should emit one row per package-target.
    #[serde(default)]
    pub packages_only: Option<bool>,
    /// Whether execution should stop at the first bad combination.
    #[serde(default)]
    pub fail_fast: Option<bool>,
    /// Whether to disable automatic implied-feature pruning.
    #[serde(default)]
    pub no_prune_implied: Option<bool>,
    /// Positive spelling kept for existing config and readability.
    #[serde(default)]
    pub prune_implied: Option<bool>,
    /// Whether pruned combinations should be shown in the summary.
    #[serde(default)]
    pub show_pruned: Option<bool>,
    /// Whether compatible target invocations may be aggregated.
    #[serde(default)]
    pub aggregate_targets: Option<bool>,
    /// Whether configured target lists should be ignored.
    #[serde(default)]
    pub no_targets: Option<bool>,
    /// Whether missing Rust target components should be installed with rustup.
    #[serde(default)]
    pub install_missing_targets: Option<bool>,
    /// Whether packages without a library target should be skipped.
    #[serde(default)]
    pub only_packages_with_lib_target: Option<bool>,
}

impl CommandCapabilities {
    /// Overlay explicitly configured values from `other`.
    pub fn merge(&mut self, other: &Self) {
        overlay_bool(&mut self.targets, other.targets);
        self.flags.overlay(other.flags);
    }
}

impl FlagConfig {
    /// Validate contradictions that can be expressed inside one raw scope.
    pub(crate) fn validate(self) -> color_eyre::eyre::Result<()> {
        if self.no_prune_implied.is_some() && self.prune_implied.is_some() {
            color_eyre::eyre::bail!(
                "`no_prune_implied` and `prune_implied` are contradictory; configure only one spelling in the same scope"
            );
        }
        if self.dedupe == Some(true) && self.diagnostics_only == Some(false) {
            color_eyre::eyre::bail!(
                "`dedupe = true` requires diagnostics-only output; do not set `diagnostics_only = false` in the same scope"
            );
        }
        Ok(())
    }

    /// Overlay explicitly configured values from `other`.
    pub fn overlay(&mut self, other: Self) {
        macro_rules! overlay_fields {
            ($($field:ident),+ $(,)?) => {
                $(overlay_bool(&mut self.$field, other.$field);)+
            };
        }
        for_each_flag_config_field!(overlay_fields);
        if other.diagnostics_only == Some(false) && other.dedupe != Some(true) {
            self.dedupe = Some(false);
        }
        if other.dedupe == Some(true)
            && other.diagnostics_only.is_none()
            && self.diagnostics_only == Some(false)
        {
            self.diagnostics_only = None;
        }
        if let Some(value) = other.no_prune_implied {
            self.no_prune_implied = Some(value);
            self.prune_implied = None;
        }
        if let Some(value) = other.prune_implied {
            self.prune_implied = Some(value);
            self.no_prune_implied = None;
        }
    }

    /// Whether this config explicitly asks cargo-fc to enable diagnostics mode.
    #[must_use]
    pub fn requests_diagnostics(self) -> bool {
        self.diagnostics_only == Some(true) || self.dedupe == Some(true)
    }

    /// Whether this config explicitly mentions diagnostics mode.
    #[must_use]
    pub fn mentions_diagnostics(self) -> bool {
        self.diagnostics_only.is_some() || self.dedupe.is_some()
    }
}

fn overlay_bool(target: &mut Option<bool>, source: Option<bool>) {
    if source.is_some() {
        *target = source;
    }
}

/// Fully resolved cargo-fc flags for one phase or package-target execution.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedFlags {
    /// Hide cargo output and print only summaries.
    pub summary_only: bool,
    /// Request JSON diagnostics and render only compiler diagnostics.
    pub diagnostics_only: bool,
    /// Deduplicate rendered diagnostics across invocations.
    pub dedupe: bool,
    /// Print full spawned commands in the per-combination header.
    pub verbose: bool,
    /// Treat warnings like failures in summaries and fail-fast checks.
    pub pedantic: bool,
    /// Suppress warnings from rustc and show errors only.
    pub errors_only: bool,
    /// Emit one matrix row per package-target instead of every feature combo.
    pub packages_only: bool,
    /// Stop execution after the first failing combination.
    pub fail_fast: bool,
    /// Disable implied-feature pruning.
    pub no_prune_implied: bool,
    /// Show pruned combinations in the final summary.
    pub show_pruned: bool,
    /// Use aggregate target execution when the whole run can do so.
    pub aggregate_targets: bool,
    /// Ignore configured target lists during target selection.
    pub no_targets: bool,
    /// Install missing Rust target components with rustup before execution.
    pub install_missing_targets: bool,
    /// Skip packages that do not expose a library target.
    pub only_packages_with_lib_target: bool,
}

impl ResolvedFlags {
    /// Convert an overlaid raw config into concrete flags.
    #[must_use]
    pub fn from_config(config: FlagConfig) -> Self {
        let mut out = Self::default();
        macro_rules! set_defaulted_fields {
            ($($field:ident),+ $(,)?) => {
                $(out.$field = config.$field.unwrap_or(false);)+
            };
        }
        for_each_simple_resolved_flag_field!(set_defaulted_fields);

        let no_prune_implied = config
            .no_prune_implied
            .or_else(|| config.prune_implied.map(|enabled| !enabled))
            .unwrap_or(false);
        let dedupe = config.dedupe.unwrap_or(false);
        out.diagnostics_only = config.diagnostics_only.unwrap_or(false) || dedupe;
        out.dedupe = dedupe;
        out.no_prune_implied = no_prune_implied;
        out
    }

    /// Convert an overlaid raw config into concrete flags.
    ///
    /// # Errors
    ///
    /// Returns an error when the merged config explicitly enables `dedupe`
    /// while explicitly disabling `diagnostics_only`. Dedupe consumes the
    /// diagnostics-only JSON stream, so those settings are contradictory.
    pub(crate) fn try_from_config(config: FlagConfig) -> color_eyre::eyre::Result<Self> {
        if config.dedupe == Some(true) && config.diagnostics_only == Some(false) {
            color_eyre::eyre::bail!(
                "`dedupe = true` requires `diagnostics_only = true`; remove `diagnostics_only = false` or set `dedupe = false`"
            );
        }
        Ok(Self::from_config(config))
    }
}

/// Fully resolved flags plus warning metadata from flag resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedFlagResult {
    flags: ResolvedFlags,
    ignored_diagnostics_config: bool,
}

/// Named inputs for resolving cargo-fc behavior for one package/target command.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolveCommandConfigArgs<'a> {
    pub(crate) workspace: &'a WorkspaceConfig,
    pub(crate) workspace_target_flags: FlagConfig,
    pub(crate) workspace_target_subcommands: &'a BTreeMap<String, CommandCapabilities>,
    pub(crate) package_flags: FlagConfig,
    pub(crate) package_subcommands: &'a BTreeMap<String, CommandCapabilities>,
    pub(crate) package_target_flags: FlagConfig,
    pub(crate) package_target_subcommands: &'a BTreeMap<String, CommandCapabilities>,
    pub(crate) raw_command: Option<&'a str>,
    pub(crate) resolved_command: Option<&'a str>,
    pub(crate) cli_flags: FlagConfig,
    pub(crate) default_diagnostics_allowed: bool,
    pub(crate) default_targets_enabled: bool,
}

/// Fully resolved cargo-fc behavior for one package/target command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedCommandConfig {
    pub(crate) flags: ResolvedFlags,
    pub(crate) targets_enabled: bool,
    pub(crate) targets_explicit: bool,
    pub(crate) ignored_diagnostics_config: bool,
}

pub(crate) fn resolve_command_config(
    args: ResolveCommandConfigArgs<'_>,
) -> color_eyre::eyre::Result<ResolvedCommandConfig> {
    let workspace_command = crate::cli::selected_command_override(
        args.raw_command,
        args.resolved_command,
        &args.workspace.subcommand_overrides,
    );
    let workspace_target_command = crate::cli::selected_command_override(
        args.raw_command,
        args.resolved_command,
        args.workspace_target_subcommands,
    );
    let package_command = crate::cli::selected_command_override(
        args.raw_command,
        args.resolved_command,
        args.package_subcommands,
    );
    let package_target_command = crate::cli::selected_command_override(
        args.raw_command,
        args.resolved_command,
        args.package_target_subcommands,
    );

    let flag_result = resolve_flags(
        [
            (args.workspace.flags, workspace_command),
            (args.workspace_target_flags, workspace_target_command),
            (args.package_flags, package_command),
            (args.package_target_flags, package_target_command),
        ],
        args.cli_flags,
        args.default_diagnostics_allowed,
    )?;
    let targets = resolve_target_capability(
        args.default_targets_enabled,
        [
            workspace_command,
            workspace_target_command,
            package_command,
            package_target_command,
        ],
    );

    Ok(ResolvedCommandConfig {
        flags: flag_result.flags,
        targets_enabled: targets.enabled,
        targets_explicit: targets.explicit,
        ignored_diagnostics_config: flag_result.ignored_diagnostics_config,
    })
}

/// Resolve ordered flag layers into one flat view.
///
/// Layers must be provided from broadest to narrowest scope. Config-driven
/// diagnostics flags from plain config scopes are applied only to built-in
/// diagnostics-safe commands. Diagnostics flags in a matching subcommand table
/// are explicit command-local behavior and bypass that safety gate. Explicit
/// CLI flags are overlaid after all config and therefore always win.
fn resolve_flags<'a>(
    layers: impl IntoIterator<Item = (FlagConfig, Option<&'a CommandCapabilities>)>,
    cli_flags: FlagConfig,
    default_diagnostics_allowed: bool,
) -> color_eyre::eyre::Result<ResolvedFlagResult> {
    let mut merged = FlagConfig::default();
    let mut ignored_diagnostics_config = false;

    for (flags, command) in layers {
        flags.validate()?;
        let plain_flags = if default_diagnostics_allowed {
            flags
        } else {
            gated_plain_diagnostics(flags, &mut ignored_diagnostics_config)
        };
        merged.overlay(plain_flags);

        if let Some(command) = command {
            command.flags.validate()?;
            if command.flags.mentions_diagnostics() {
                ignored_diagnostics_config = false;
            }
            merged.overlay(command.flags);
        }
    }

    merged.overlay(cli_flags);

    let flags = ResolvedFlags::try_from_config(merged)?;
    Ok(ResolvedFlagResult {
        ignored_diagnostics_config: ignored_diagnostics_config && !flags.diagnostics_only,
        flags,
    })
}

fn gated_plain_diagnostics(
    mut flags: FlagConfig,
    ignored_diagnostics_config: &mut bool,
) -> FlagConfig {
    if flags.requests_diagnostics() {
        *ignored_diagnostics_config = true;
    } else if flags.mentions_diagnostics() {
        *ignored_diagnostics_config = false;
    }

    if flags.diagnostics_only == Some(true) {
        flags.diagnostics_only = None;
    }
    if flags.dedupe == Some(true) {
        flags.dedupe = None;
    }

    flags
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedTargetCapability {
    enabled: bool,
    explicit: bool,
}

fn resolve_target_capability<'a>(
    default_enabled: bool,
    commands: impl IntoIterator<Item = Option<&'a CommandCapabilities>>,
) -> ResolvedTargetCapability {
    let mut enabled = default_enabled;
    let mut explicit = false;
    for command in commands.into_iter().flatten() {
        if let Some(targets) = command.targets {
            enabled = targets;
            explicit = true;
        }
    }
    ResolvedTargetCapability { enabled, explicit }
}

pub(crate) fn combine_flag_configs<'a>(
    field_prefix: Option<&str>,
    source_kind: &str,
    entries: impl IntoIterator<Item = (&'a str, FlagConfig)>,
) -> color_eyre::eyre::Result<FlagConfig> {
    let entries: Vec<_> = entries.into_iter().collect();
    let mut out = FlagConfig::default();
    macro_rules! combine {
        ($($field:ident),+ $(,)?) => {
            $(
                let field_name = if let Some(prefix) = field_prefix {
                    format!("{prefix}.{}", stringify!($field))
                } else {
                    stringify!($field).to_string()
                };
                if let Some(value) = combine_bool(
                    &field_name,
                    source_kind,
                    &entries,
                    |flags| flags.$field,
                )? {
                    out.$field = Some(value);
                }
            )+
        };
    }
    for_each_flag_config_field!(combine);
    out.validate()?;
    Ok(out)
}

fn combine_bool<T>(
    name: &str,
    source_kind: &str,
    entries: &[(&str, T)],
    get: impl Fn(&T) -> Option<bool>,
) -> color_eyre::eyre::Result<Option<bool>> {
    let mut out = None;
    for (expr, item) in entries {
        if let Some(value) = get(item) {
            match out {
                None => out = Some(value),
                Some(existing) if existing == value => {}
                Some(_) => {
                    color_eyre::eyre::bail!(
                        "conflicting values for `{name}` in {source_kind} `{expr}`"
                    );
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn combine_command_capability_maps<'a>(
    source_kind: &str,
    maps: impl IntoIterator<Item = (&'a str, &'a BTreeMap<String, CommandCapabilities>)>,
) -> color_eyre::eyre::Result<BTreeMap<String, CommandCapabilities>> {
    let maps: Vec<_> = maps.into_iter().collect();
    let mut names = BTreeSet::new();
    for (_expr, commands) in &maps {
        names.extend(commands.keys().cloned());
    }

    let mut out = BTreeMap::new();
    for name in names {
        let entries: Vec<_> = maps
            .iter()
            .filter_map(|(expr, commands)| commands.get(&name).map(|command| (*expr, command)))
            .collect();
        out.insert(
            name.clone(),
            combine_command_capabilities(&name, source_kind, &entries)?,
        );
    }
    Ok(out)
}

fn combine_command_capabilities(
    command: &str,
    source_kind: &str,
    entries: &[(&str, &CommandCapabilities)],
) -> color_eyre::eyre::Result<CommandCapabilities> {
    let mut out = CommandCapabilities::default();
    let target_name = format!("subcommands.{command}.targets");
    out.targets = combine_bool(&target_name, source_kind, entries, |capability| {
        capability.targets
    })?;

    let flag_prefix = format!("subcommands.{command}");
    out.flags = combine_flag_configs(
        Some(&flag_prefix),
        source_kind,
        entries
            .iter()
            .map(|(expr, capability)| (*expr, capability.flags)),
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        CommandCapabilities, FlagConfig, ResolveCommandConfigArgs, ResolvedFlags, WorkspaceConfig,
        resolve_command_config, resolve_flags, resolve_target_capability,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn flag_subset_macros_match_documented_special_cases() {
        macro_rules! define_master_fields {
            ($($field:ident),+ $(,)?) => {
                fn master_fields() -> BTreeSet<&'static str> {
                    [$(stringify!($field),)+].into_iter().collect()
                }
            };
        }
        macro_rules! define_simple_resolved_fields {
            ($($field:ident),+ $(,)?) => {
                fn simple_resolved_fields() -> BTreeSet<&'static str> {
                    [$(stringify!($field),)+].into_iter().collect()
                }
            };
        }

        for_each_flag_config_field!(define_master_fields);
        for_each_simple_resolved_flag_field!(define_simple_resolved_fields);

        let master = master_fields();
        let simple_resolved = simple_resolved_fields();

        let mut expected_simple_resolved = master.clone();
        expected_simple_resolved.remove("prune_implied");
        expected_simple_resolved.remove("diagnostics_only");
        expected_simple_resolved.remove("dedupe");
        expected_simple_resolved.remove("no_prune_implied");
        assert_eq!(simple_resolved, expected_simple_resolved);
    }

    #[test]
    fn resolve_flags_overlays_broad_to_narrow_then_cli() -> color_eyre::eyre::Result<()> {
        let workspace = FlagConfig {
            fail_fast: Some(true),
            pedantic: Some(false),
            ..FlagConfig::default()
        };
        let package = FlagConfig {
            pedantic: Some(true),
            errors_only: Some(false),
            ..FlagConfig::default()
        };
        let command = CommandCapabilities {
            flags: FlagConfig {
                errors_only: Some(true),
                ..FlagConfig::default()
            },
            ..CommandCapabilities::default()
        };
        let cli = FlagConfig {
            fail_fast: Some(false),
            ..FlagConfig::default()
        };

        let resolved =
            resolve_flags([(workspace, None), (package, Some(&command))], cli, true)?.flags;

        assert_eq!(
            resolved,
            ResolvedFlags {
                fail_fast: false,
                pedantic: true,
                errors_only: true,
                ..ResolvedFlags::default()
            }
        );
        Ok(())
    }

    #[test]
    fn broad_config_diagnostics_are_gated_but_cli_flags_win() -> color_eyre::eyre::Result<()> {
        let workspace = FlagConfig {
            dedupe: Some(true),
            ..FlagConfig::default()
        };
        let cli = FlagConfig {
            diagnostics_only: Some(true),
            ..FlagConfig::default()
        };

        let resolved = resolve_flags([(workspace, None)], cli, false)?;

        assert!(!resolved.ignored_diagnostics_config);
        assert!(resolved.flags.diagnostics_only);
        assert!(!resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn broad_config_diagnostics_are_dropped_for_unsafe_commands() -> color_eyre::eyre::Result<()> {
        let workspace = FlagConfig {
            dedupe: Some(true),
            ..FlagConfig::default()
        };

        let resolved = resolve_flags([(workspace, None)], FlagConfig::default(), false)?;

        assert!(resolved.ignored_diagnostics_config);
        assert!(!resolved.flags.diagnostics_only);
        assert!(!resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn narrower_diagnostics_false_disables_broader_dedupe() -> color_eyre::eyre::Result<()> {
        let workspace = FlagConfig {
            dedupe: Some(true),
            ..FlagConfig::default()
        };
        let package = FlagConfig {
            diagnostics_only: Some(false),
            ..FlagConfig::default()
        };

        let resolved = resolve_flags(
            [(workspace, None), (package, None)],
            FlagConfig::default(),
            true,
        )?;

        assert!(!resolved.flags.diagnostics_only);
        assert!(!resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn narrower_dedupe_true_overrides_broader_diagnostics_false() -> color_eyre::eyre::Result<()> {
        let workspace = FlagConfig {
            diagnostics_only: Some(false),
            ..FlagConfig::default()
        };
        let package = FlagConfig {
            dedupe: Some(true),
            ..FlagConfig::default()
        };

        let resolved = resolve_flags(
            [(workspace, None), (package, None)],
            FlagConfig::default(),
            true,
        )?;

        assert!(resolved.flags.diagnostics_only);
        assert!(resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn same_scope_dedupe_true_and_diagnostics_false_errors() {
        let result = resolve_flags(
            [(
                FlagConfig {
                    diagnostics_only: Some(false),
                    dedupe: Some(true),
                    ..FlagConfig::default()
                },
                None,
            )],
            FlagConfig::default(),
            true,
        );

        assert!(result.is_err());
    }

    #[test]
    fn command_diagnostics_override_prevents_false_unsupported_warning()
    -> color_eyre::eyre::Result<()> {
        let workspace_command = CommandCapabilities {
            flags: FlagConfig {
                dedupe: Some(true),
                ..FlagConfig::default()
            },
            ..CommandCapabilities::default()
        };
        let package = FlagConfig {
            diagnostics_only: Some(true),
            ..FlagConfig::default()
        };

        let resolved = resolve_flags(
            [
                (FlagConfig::default(), Some(&workspace_command)),
                (package, None),
            ],
            FlagConfig::default(),
            false,
        )?;

        assert!(!resolved.ignored_diagnostics_config);
        assert!(resolved.flags.diagnostics_only);
        assert!(resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn broad_diagnostics_true_with_dedupe_false_still_warns() -> color_eyre::eyre::Result<()> {
        let resolved = resolve_flags(
            [(
                FlagConfig {
                    diagnostics_only: Some(true),
                    dedupe: Some(false),
                    ..FlagConfig::default()
                },
                None,
            )],
            FlagConfig::default(),
            false,
        )?;

        assert!(resolved.ignored_diagnostics_config);
        assert!(!resolved.flags.diagnostics_only);
        assert!(!resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn command_diagnostics_request_opts_unknown_command_in() -> color_eyre::eyre::Result<()> {
        let command = CommandCapabilities {
            flags: FlagConfig {
                dedupe: Some(true),
                ..FlagConfig::default()
            },
            ..CommandCapabilities::default()
        };

        let resolved = resolve_flags(
            [(FlagConfig::default(), Some(&command))],
            FlagConfig::default(),
            false,
        )?;

        assert!(!resolved.ignored_diagnostics_config);
        assert!(resolved.flags.diagnostics_only);
        assert!(resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn no_prune_and_prune_spellings_in_one_scope_error() {
        let err = resolve_flags(
            [(
                FlagConfig {
                    no_prune_implied: Some(true),
                    prune_implied: Some(true),
                    ..FlagConfig::default()
                },
                None,
            )],
            FlagConfig::default(),
            true,
        )
        .expect_err("contradictory prune spelling should fail");

        assert!(err.to_string().contains("no_prune_implied"));
    }

    #[test]
    fn target_capability_overlays_command_scopes() {
        let workspace = CommandCapabilities {
            targets: Some(false),
            ..CommandCapabilities::default()
        };
        let package = CommandCapabilities {
            targets: Some(true),
            ..CommandCapabilities::default()
        };

        let resolved = resolve_target_capability(true, [Some(&workspace), Some(&package)]);
        assert!(resolved.enabled);
        assert!(resolved.explicit);
    }

    #[test]
    fn resolve_command_config_tracks_explicit_target_decisions() -> color_eyre::eyre::Result<()> {
        let mut workspace = WorkspaceConfig::default();
        workspace.subcommand_overrides.insert(
            "lint".to_string(),
            CommandCapabilities {
                targets: Some(false),
                ..CommandCapabilities::default()
            },
        );
        workspace.subcommand_overrides.insert(
            "clippy".to_string(),
            CommandCapabilities {
                targets: Some(true),
                ..CommandCapabilities::default()
            },
        );
        let empty = BTreeMap::new();

        let resolved = resolve_command_config(ResolveCommandConfigArgs {
            workspace: &workspace,
            workspace_target_flags: FlagConfig::default(),
            workspace_target_subcommands: &empty,
            package_flags: FlagConfig::default(),
            package_subcommands: &empty,
            package_target_flags: FlagConfig::default(),
            package_target_subcommands: &empty,
            raw_command: Some("lint"),
            resolved_command: Some("clippy"),
            cli_flags: FlagConfig::default(),
            default_diagnostics_allowed: true,
            default_targets_enabled: true,
        })?;

        assert!(!resolved.targets_enabled);
        assert!(resolved.targets_explicit);
        Ok(())
    }
}
