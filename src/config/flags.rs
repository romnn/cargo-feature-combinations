use super::schema::{CommandCapabilities, FeatureMatrixPatch, WorkspaceConfig};
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
    /// Overlay the `expand_targets` capability and `flags` from `other`.
    ///
    /// The `features` payload is intentionally NOT merged here: subcommand
    /// feature-matrix patches are resolved separately, layer by layer, in
    /// `config::resolve::resolve_config_with_flag_layers` (which reads
    /// `.features` straight off the raw overrides). This combinator only feeds
    /// flag/target-capability resolution, so a merged capability's `.features`
    /// is expected to stay at its default.
    pub fn merge(&mut self, other: &Self) {
        overlay_bool(&mut self.expand_targets, other.expand_targets);
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
    pub(crate) workspace_target_replace: bool,
    pub(crate) workspace_target_driver: Option<&'a str>,
    pub(crate) workspace_target_subcommands: &'a BTreeMap<String, CommandCapabilities>,
    pub(crate) package_flags: FlagConfig,
    pub(crate) package_replace: bool,
    pub(crate) package_driver: Option<&'a str>,
    pub(crate) package_subcommands: &'a BTreeMap<String, CommandCapabilities>,
    pub(crate) package_target_flags: FlagConfig,
    pub(crate) package_target_replace: bool,
    pub(crate) package_target_driver: Option<&'a str>,
    pub(crate) package_target_subcommands: &'a BTreeMap<String, CommandCapabilities>,
    pub(crate) raw_command: Option<&'a str>,
    pub(crate) resolved_command: Option<&'a str>,
    pub(crate) cli_flags: FlagConfig,
    pub(crate) cli_driver: Option<&'a str>,
    pub(crate) default_diagnostics_allowed: bool,
    pub(crate) default_targets_enabled: bool,
}

/// Fully resolved cargo-fc behavior for one package/target command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedCommandConfig {
    pub(crate) flags: ResolvedFlags,
    pub(crate) targets_enabled: bool,
    pub(crate) targets_explicit: bool,
    pub(crate) ignored_diagnostics_config: bool,
    /// The build driver resolved from config + `--driver` along the precedence
    /// chain. `None` means "unset" — the caller applies its own default (e.g.
    /// cargo-fc's cross-target `cargo-zigbuild` fallback). Not yet normalized:
    /// an explicit `"cargo"` is preserved so the caller can distinguish it from
    /// unset and skip the fallback.
    pub(crate) driver: Option<String>,
}

/// One scope of the precedence chain: its flag/driver payloads, whether it
/// carries `replace = true`, and its matching subcommand override. Grouping the
/// four scopes here keeps `flags`, `driver`, and target-capability resolution
/// walking the *same* chain (same `replace` sources, same command overrides)
/// rather than three hand-written parallel layer lists that could drift.
struct Scope<'a> {
    flags: FlagConfig,
    driver: Option<&'a str>,
    replace: bool,
    command: Option<&'a CommandCapabilities>,
}

pub(crate) fn resolve_command_config(
    args: ResolveCommandConfigArgs<'_>,
) -> color_eyre::eyre::Result<ResolvedCommandConfig> {
    let command_for = |subcommands| {
        crate::cli::selected_command_override(args.raw_command, args.resolved_command, subcommands)
    };
    let scopes = [
        // Workspace base is the broadest layer and never carries `replace`.
        Scope {
            flags: args.workspace.flags,
            driver: args.workspace.driver.as_deref(),
            replace: false,
            command: command_for(&args.workspace.subcommand_overrides),
        },
        Scope {
            flags: args.workspace_target_flags,
            driver: args.workspace_target_driver,
            replace: args.workspace_target_replace,
            command: command_for(args.workspace_target_subcommands),
        },
        Scope {
            flags: args.package_flags,
            driver: args.package_driver,
            replace: args.package_replace,
            command: command_for(args.package_subcommands),
        },
        Scope {
            flags: args.package_target_flags,
            driver: args.package_target_driver,
            replace: args.package_target_replace,
            command: command_for(args.package_target_subcommands),
        },
    ];

    let flag_result = resolve_flags(
        scopes.iter().map(|s| (s.flags, s.replace, s.command)),
        args.cli_flags,
        args.default_diagnostics_allowed,
    )?;
    let targets = resolve_target_capability(
        args.default_targets_enabled,
        scopes.iter().map(|s| s.command),
    );
    let driver = resolve_driver_chain(
        scopes.iter().map(|s| (s.driver, s.replace, s.command)),
        args.cli_driver,
    );

    Ok(ResolvedCommandConfig {
        flags: flag_result.flags,
        targets_enabled: targets.enabled,
        targets_explicit: targets.explicit,
        ignored_diagnostics_config: flag_result.ignored_diagnostics_config,
        driver,
    })
}

/// Resolve the scalar `driver` along the same broad→narrow layer chain as
/// [`resolve_flags`], with the same `replace` semantics: a section or command
/// `replace = true` discards everything broader, and the CLI `--driver` (passed
/// as `cli_driver`) overlays last and always wins. Returns the raw resolved
/// string (unnormalized); `None` means unset.
fn resolve_driver_chain<'a>(
    layers: impl IntoIterator<Item = (Option<&'a str>, bool, Option<&'a CommandCapabilities>)>,
    cli_driver: Option<&'a str>,
) -> Option<String> {
    // Thread `&str` and allocate a single owned `String` at the end, rather than
    // materializing a `String` for each layer that is overwritten by a narrower
    // one (mirroring `combine_driver`).
    let mut merged: Option<&str> = None;
    for (section_driver, section_replace, command) in layers {
        if section_replace {
            merged = None;
        }
        if let Some(driver) = section_driver {
            merged = Some(driver);
        }
        if let Some(command) = command {
            if command.replace {
                merged = None;
            }
            if let Some(driver) = &command.driver {
                merged = Some(driver);
            }
        }
    }
    if let Some(driver) = cli_driver {
        merged = Some(driver);
    }
    merged.map(ToString::to_string)
}

/// Resolve ordered flag layers into one flat view.
///
/// Layers must be provided from broadest to narrowest scope. Each layer carries
/// a `replace` flag for its section and (via the [`CommandCapabilities`]) for
/// its subcommand: `replace = true` resets the accumulated flags to defaults,
/// discarding everything broader in the chain. Config-driven diagnostics flags
/// from plain config scopes are applied only to built-in diagnostics-safe
/// commands. Diagnostics flags in a matching subcommand table are explicit
/// command-local behavior and bypass that safety gate. Explicit CLI flags are
/// overlaid after all config and therefore always win.
fn resolve_flags<'a>(
    layers: impl IntoIterator<Item = (FlagConfig, bool, Option<&'a CommandCapabilities>)>,
    cli_flags: FlagConfig,
    default_diagnostics_allowed: bool,
) -> color_eyre::eyre::Result<ResolvedFlagResult> {
    let mut merged = FlagConfig::default();
    let mut ignored_diagnostics_config = false;

    for (flags, section_replace, command) in layers {
        if section_replace {
            merged = FlagConfig::default();
            ignored_diagnostics_config = false;
        }
        flags.validate()?;
        let plain_flags = if default_diagnostics_allowed {
            flags
        } else {
            gated_plain_diagnostics(flags, &mut ignored_diagnostics_config)
        };
        merged.overlay(plain_flags);

        if let Some(command) = command {
            if command.replace {
                merged = FlagConfig::default();
                ignored_diagnostics_config = false;
            }
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
        if let Some(expand) = command.expand_targets {
            enabled = expand;
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

/// Fold a scalar field across the sibling entries that make up one precedence
/// layer (e.g. several `target.'cfg(...)'` sections matching one triple),
/// erroring if two siblings set differing values. Returns `None` when no sibling
/// set the field.
fn combine_scalar<'a, T, V: PartialEq>(
    name: &str,
    source_kind: &str,
    entries: &'a [(&'a str, T)],
    get: impl Fn(&'a T) -> Option<V>,
) -> color_eyre::eyre::Result<Option<V>> {
    let mut out: Option<V> = None;
    for (expr, item) in entries {
        if let Some(value) = get(item) {
            match &out {
                None => out = Some(value),
                Some(existing) if *existing == value => {}
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

pub(crate) fn combine_bool<T>(
    name: &str,
    source_kind: &str,
    entries: &[(&str, T)],
    get: impl Fn(&T) -> Option<bool>,
) -> color_eyre::eyre::Result<Option<bool>> {
    combine_scalar(name, source_kind, entries, get)
}

/// Combine the scalar `driver` across sibling entries that match one scope.
/// Values are trimmed before comparison, so two siblings differing only in
/// surrounding whitespace name the same program and do not conflict. An empty
/// (or whitespace-only) driver is rejected here — matching `normalize_driver` at
/// the other scopes — so `driver = ""` fails the same way at every scope rather
/// than being silently dropped. Differing non-empty values conflict, mirroring
/// [`combine_bool`].
pub(crate) fn combine_driver<T>(
    name: &str,
    source_kind: &str,
    entries: &[(&str, T)],
    get: impl Fn(&T) -> Option<&str>,
) -> color_eyre::eyre::Result<Option<String>> {
    for (expr, item) in entries {
        if get(item).is_some_and(|value| value.trim().is_empty()) {
            color_eyre::eyre::bail!("`{name}` must not be empty in {source_kind} `{expr}`");
        }
    }
    let combined = combine_scalar(name, source_kind, entries, |item| get(item).map(str::trim))?;
    Ok(combined.map(ToString::to_string))
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
        let combined = combine_command_capabilities(&name, source_kind, &entries)?;
        out.insert(name, combined);
    }
    Ok(out)
}

// Combines `replace`, `expand_targets`, `driver`, `exclude_packages`, and
// `flags` across the sibling cfg-sections matching one target. `features` and
// `targets` are NOT combined here: `features` is resolved separately in
// `config::resolve` straight off the raw overrides (see
// [`CommandCapabilities::merge`]), and a `targets` list is not valid at the
// target×subcommand scope (validation rejects it).
fn combine_command_capabilities(
    command: &str,
    source_kind: &str,
    entries: &[(&str, &CommandCapabilities)],
) -> color_eyre::eyre::Result<CommandCapabilities> {
    // `replace = true` in any matching sibling resets the chain from this layer.
    let replace = entries.iter().any(|(_, capability)| capability.replace);

    let expand_targets = combine_bool(
        &format!("subcommands.{command}.expand_targets"),
        source_kind,
        entries,
        |capability| capability.expand_targets,
    )?;

    let driver = combine_driver(
        &format!("subcommands.{command}.driver"),
        source_kind,
        entries,
        |capability| capability.driver.as_deref(),
    )?;

    let exclude_packages = super::patch::combine_set_patches(
        &format!("subcommands.{command}.exclude_packages"),
        source_kind,
        entries.iter().filter_map(|(expr, capability)| {
            capability
                .exclude_packages
                .as_ref()
                .map(|patch| (*expr, patch))
        }),
    )?
    .map(super::patch::SetPatchOps::into_string_set_patch);

    let flags = combine_flag_configs(
        Some(&format!("subcommands.{command}")),
        source_kind,
        entries
            .iter()
            .map(|(expr, capability)| (*expr, capability.flags)),
    )?;

    // `targets` (the list) and `features` are intentionally NOT combined: a
    // `targets` list is invalid at the target×subcommand scope (validation
    // rejects it) and `features` is resolved separately in `config::resolve`
    // straight off the raw overrides. They are listed explicitly (no
    // `..default()`) so a new `CommandCapabilities` field becomes a compile
    // error here — forcing a combine decision instead of a silent drop.
    Ok(CommandCapabilities {
        replace,
        expand_targets,
        exclude_packages,
        targets: None,
        driver,
        features: FeatureMatrixPatch::default(),
        flags,
    })
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

        let resolved = resolve_flags(
            [(workspace, false, None), (package, false, Some(&command))],
            cli,
            true,
        )?
        .flags;

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

        let resolved = resolve_flags([(workspace, false, None)], cli, false)?;

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

        let resolved = resolve_flags([(workspace, false, None)], FlagConfig::default(), false)?;

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
            [(workspace, false, None), (package, false, None)],
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
            [(workspace, false, None), (package, false, None)],
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
                false,
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
                (FlagConfig::default(), false, Some(&workspace_command)),
                (package, false, None),
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
                false,
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
            [(FlagConfig::default(), false, Some(&command))],
            FlagConfig::default(),
            false,
        )?;

        assert!(!resolved.ignored_diagnostics_config);
        assert!(resolved.flags.diagnostics_only);
        assert!(resolved.flags.dedupe);
        Ok(())
    }

    #[test]
    fn section_replace_discards_broader_flag_layers() -> color_eyre::eyre::Result<()> {
        let workspace = FlagConfig {
            verbose: Some(true),
            pedantic: Some(true),
            ..FlagConfig::default()
        };
        let package = FlagConfig {
            pedantic: Some(false),
            ..FlagConfig::default()
        };

        // Without replace: `verbose` inherits the workspace, `pedantic` is overridden.
        let inherited = resolve_flags(
            [(workspace, false, None), (package, false, None)],
            FlagConfig::default(),
            true,
        )?
        .flags;
        assert!(inherited.verbose);
        assert!(!inherited.pedantic);

        // `replace = true` on the package layer discards the workspace layer, so
        // the unset `verbose` falls back to its default instead of inheriting.
        let replaced = resolve_flags(
            [(workspace, false, None), (package, true, None)],
            FlagConfig::default(),
            true,
        )?
        .flags;
        assert!(!replaced.verbose);
        assert!(!replaced.pedantic);
        Ok(())
    }

    #[test]
    fn subcommand_replace_discards_broader_flag_layers() -> color_eyre::eyre::Result<()> {
        let workspace = FlagConfig {
            verbose: Some(true),
            ..FlagConfig::default()
        };
        let command = CommandCapabilities {
            replace: true,
            flags: FlagConfig {
                pedantic: Some(true),
                ..FlagConfig::default()
            },
            ..CommandCapabilities::default()
        };

        // The subcommand's `replace` resets before its own flags apply, so the
        // workspace `verbose` is discarded.
        let resolved = resolve_flags(
            [(workspace, false, Some(&command))],
            FlagConfig::default(),
            true,
        )?
        .flags;
        assert!(!resolved.verbose);
        assert!(resolved.pedantic);
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
                false,
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
            expand_targets: Some(false),
            ..CommandCapabilities::default()
        };
        let package = CommandCapabilities {
            expand_targets: Some(true),
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
                expand_targets: Some(false),
                ..CommandCapabilities::default()
            },
        );
        workspace.subcommand_overrides.insert(
            "clippy".to_string(),
            CommandCapabilities {
                expand_targets: Some(true),
                ..CommandCapabilities::default()
            },
        );
        let empty = BTreeMap::new();

        let resolved = resolve_command_config(ResolveCommandConfigArgs {
            workspace: &workspace,
            workspace_target_flags: FlagConfig::default(),
            workspace_target_replace: false,
            workspace_target_driver: None,
            workspace_target_subcommands: &empty,
            package_flags: FlagConfig::default(),
            package_replace: false,
            package_driver: None,
            package_subcommands: &empty,
            package_target_flags: FlagConfig::default(),
            package_target_replace: false,
            package_target_driver: None,
            package_target_subcommands: &empty,
            raw_command: Some("lint"),
            resolved_command: Some("clippy"),
            cli_flags: FlagConfig::default(),
            cli_driver: None,
            default_diagnostics_allowed: true,
            default_targets_enabled: true,
        })?;

        assert!(!resolved.targets_enabled);
        assert!(resolved.targets_explicit);
        Ok(())
    }

    fn driver_args<'a>(
        workspace: &'a WorkspaceConfig,
        empty: &'a BTreeMap<String, CommandCapabilities>,
    ) -> ResolveCommandConfigArgs<'a> {
        ResolveCommandConfigArgs {
            workspace,
            workspace_target_flags: FlagConfig::default(),
            workspace_target_replace: false,
            workspace_target_driver: None,
            workspace_target_subcommands: empty,
            package_flags: FlagConfig::default(),
            package_replace: false,
            package_driver: None,
            package_subcommands: empty,
            package_target_flags: FlagConfig::default(),
            package_target_replace: false,
            package_target_driver: None,
            package_target_subcommands: empty,
            raw_command: None,
            resolved_command: None,
            cli_flags: FlagConfig::default(),
            cli_driver: None,
            default_diagnostics_allowed: true,
            default_targets_enabled: true,
        }
    }

    #[test]
    fn driver_inherits_workspace_until_a_narrower_scope_overrides() -> color_eyre::eyre::Result<()>
    {
        let workspace = WorkspaceConfig {
            driver: Some("cargo-zigbuild".to_string()),
            ..WorkspaceConfig::default()
        };
        let empty = BTreeMap::new();

        // Unset everywhere below the workspace → inherit the workspace driver.
        let inherited = resolve_command_config(driver_args(&workspace, &empty))?;
        assert_eq!(inherited.driver.as_deref(), Some("cargo-zigbuild"));

        // A package driver overrides the inherited workspace driver.
        let package_over = resolve_command_config(ResolveCommandConfigArgs {
            package_driver: Some("cross"),
            ..driver_args(&workspace, &empty)
        })?;
        assert_eq!(package_over.driver.as_deref(), Some("cross"));

        // A package-target driver overrides the package driver in turn.
        let target_over = resolve_command_config(ResolveCommandConfigArgs {
            package_driver: Some("cross"),
            package_target_driver: Some("cargo"),
            ..driver_args(&workspace, &empty)
        })?;
        assert_eq!(target_over.driver.as_deref(), Some("cargo"));
        Ok(())
    }

    #[test]
    fn driver_replace_at_package_discards_inherited_workspace_driver()
    -> color_eyre::eyre::Result<()> {
        let workspace = WorkspaceConfig {
            driver: Some("cargo-zigbuild".to_string()),
            ..WorkspaceConfig::default()
        };
        let empty = BTreeMap::new();

        // `replace = true` at the package resets the driver chain; with no
        // package driver, resolution falls back to unset.
        let reset = resolve_command_config(ResolveCommandConfigArgs {
            package_replace: true,
            ..driver_args(&workspace, &empty)
        })?;
        assert_eq!(reset.driver, None);
        Ok(())
    }

    #[test]
    fn driver_subcommand_and_cli_override() -> color_eyre::eyre::Result<()> {
        let workspace = WorkspaceConfig {
            driver: Some("cargo-zigbuild".to_string()),
            ..WorkspaceConfig::default()
        };
        let mut package_subcommands = BTreeMap::new();
        package_subcommands.insert(
            "test".to_string(),
            CommandCapabilities {
                driver: Some("cross".to_string()),
                ..CommandCapabilities::default()
            },
        );
        let empty = BTreeMap::new();

        // The subcommand driver applies for its command.
        let sub = resolve_command_config(ResolveCommandConfigArgs {
            package_subcommands: &package_subcommands,
            raw_command: Some("test"),
            resolved_command: Some("test"),
            ..driver_args(&workspace, &empty)
        })?;
        assert_eq!(sub.driver.as_deref(), Some("cross"));

        // `--driver` overlays last and wins over every config scope.
        let cli = resolve_command_config(ResolveCommandConfigArgs {
            package_subcommands: &package_subcommands,
            raw_command: Some("test"),
            resolved_command: Some("test"),
            cli_driver: Some("cargo"),
            ..driver_args(&workspace, &empty)
        })?;
        assert_eq!(cli.driver.as_deref(), Some("cargo"));
        Ok(())
    }

    #[test]
    fn dedupe_in_subcommand_resolves_per_command() -> color_eyre::eyre::Result<()> {
        let workspace = WorkspaceConfig::default();
        let mut package_subcommands = BTreeMap::new();
        package_subcommands.insert(
            "test".to_string(),
            CommandCapabilities {
                flags: FlagConfig {
                    dedupe: Some(true),
                    ..FlagConfig::default()
                },
                ..CommandCapabilities::default()
            },
        );
        let empty = BTreeMap::new();

        // For `test`, the subcommand's `dedupe` applies and forces
        // diagnostics-only output, even though broad diagnostics config is gated
        // off for this command (subcommand tables are explicit command-local
        // behavior and bypass that gate).
        let test = resolve_command_config(ResolveCommandConfigArgs {
            package_subcommands: &package_subcommands,
            raw_command: Some("test"),
            resolved_command: Some("test"),
            default_diagnostics_allowed: false,
            ..driver_args(&workspace, &empty)
        })?;
        assert!(test.flags.dedupe);
        assert!(test.flags.diagnostics_only);

        // For `build`, the `test` subcommand is inert, so `dedupe` stays off.
        let build = resolve_command_config(ResolveCommandConfigArgs {
            package_subcommands: &package_subcommands,
            raw_command: Some("build"),
            resolved_command: Some("build"),
            default_diagnostics_allowed: false,
            ..driver_args(&workspace, &empty)
        })?;
        assert!(!build.flags.dedupe);
        assert!(!build.flags.diagnostics_only);
        Ok(())
    }

    #[test]
    fn combine_driver_trims_whitespace_and_rejects_empty() -> color_eyre::eyre::Result<()> {
        // The same driver with differing surrounding whitespace names one program
        // (both trim to `cross`) and must not be reported as a conflict.
        let entries = [("cfg(a)", Some("cross")), ("cfg(b)", Some("cross "))];
        let out = super::combine_driver("driver", "target override", &entries, |value| *value)?;
        assert_eq!(out.as_deref(), Some("cross"));

        // An empty (whitespace-only) driver is rejected — the same way
        // `normalize_driver` rejects it at base/subcommand/CLI scopes — rather
        // than being silently dropped.
        let entries = [("cfg(a)", Some("cross")), ("cfg(b)", Some("  "))];
        let err = super::combine_driver("driver", "target override", &entries, |value| *value)
            .expect_err("empty driver should be rejected");
        assert!(err.to_string().contains("must not be empty"), "{err}");

        // Genuinely different drivers still conflict.
        let entries = [
            ("cfg(a)", Some("cross")),
            ("cfg(b)", Some("cargo-zigbuild")),
        ];
        assert!(
            super::combine_driver("driver", "target override", &entries, |value| *value).is_err()
        );
        Ok(())
    }
}
