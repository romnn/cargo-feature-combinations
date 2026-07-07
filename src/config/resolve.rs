use crate::cfg_eval::CfgEvaluator;
use crate::target::TargetTriple;
use color_eyre::eyre::{self, WrapErr};
use std::collections::{BTreeMap, HashSet};

use super::patch::{
    FeatureSetVecPatch, SetPatchOps, StringSetPatch, TargetListOps, TargetListPatch,
    combine_set_patches, combine_target_list_patches,
};
use super::scope::{Chain, Layer, ScopeView};
use super::{Config, FeatureMatrixPatch, FlagConfig, ResolvedFlags, WorkspaceConfig};

/// Feature-matrix output read by feature generation.
#[derive(Debug, Clone, Default)]
pub struct ResolvedFeatures {
    /// Features excluded from the powerset.
    pub exclude_features: HashSet<String>,
    /// Features included in every generated combination.
    pub include_features: HashSet<String>,
    /// Features to consider when generating the powerset.
    pub only_features: HashSet<String>,
    /// Feature sets that must be tested independently.
    pub isolated_feature_sets: Vec<HashSet<String>>,
    /// Feature-set patterns to exclude.
    pub exclude_feature_sets: Vec<HashSet<String>>,
    /// Feature sets to include exactly.
    pub include_feature_sets: Vec<HashSet<String>>,
    /// Explicitly allowed feature sets.
    pub allow_feature_sets: Vec<HashSet<String>>,
    /// Whether implicit optional-dependency features are excluded.
    pub skip_optional_dependencies: bool,
    /// Whether the empty feature set is omitted.
    pub no_empty_feature_set: bool,
    /// Arbitrary user-defined matrix metadata.
    pub matrix: serde_json::Map<String, serde_json::Value>,
    /// Maximum generated feature combinations before failing.
    pub max_combinations: Option<u128>,
}

impl ResolvedFeatures {
    /// Convert a raw package base config into a resolved feature view.
    ///
    /// This is used by tests and callers that intentionally want package-base
    /// feature generation without target or command layers.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut out = Self::default();
        apply_single_feature_patch(&mut out, &config.base.settings.features);
        out
    }
}

/// Everything one package-target command resolution needs.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub(crate) flags: ResolvedFlags,
    pub(crate) ignored_diagnostics_config: bool,
    pub(crate) driver: Option<String>,
    pub(crate) targets_enabled: bool,
    pub(crate) targets_explicit: bool,
    pub(crate) features: ResolvedFeatures,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CliOverlay<'a> {
    pub(crate) flags: FlagConfig,
    pub(crate) driver: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvePolicy {
    pub(crate) default_diagnostics_allowed: bool,
    pub(crate) default_targets_enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TargetListResolution {
    pub(crate) targets: Vec<String>,
    pub(crate) patched: bool,
    pub(crate) package_touched: bool,
}

impl Chain<'_> {
    pub(crate) fn resolve(
        &self,
        cli: CliOverlay<'_>,
        policy: ResolvePolicy,
    ) -> eyre::Result<Resolved> {
        let start = replace_start(&self.layers);
        let layers = self.layers.get(start..).unwrap_or(&[]);
        let flag_result = resolve_flags(layers, cli.flags, policy.default_diagnostics_allowed)?;
        let driver = resolve_driver(layers, cli.driver)?;
        let (targets_enabled, targets_explicit) =
            resolve_expand_targets(layers, policy.default_targets_enabled)?;
        let features = resolve_features(layers)?;

        Ok(Resolved {
            flags: flag_result.flags,
            ignored_diagnostics_config: flag_result.ignored_diagnostics_config,
            driver,
            targets_enabled,
            targets_explicit,
            features,
        })
    }

    pub(crate) fn exclude_packages(
        &self,
        base_exclude: &HashSet<String>,
    ) -> eyre::Result<HashSet<String>> {
        let start = replace_start(&self.layers);
        let mut out = if start == 0 {
            base_exclude.clone()
        } else {
            HashSet::new()
        };
        for layer in self.layers.get(start..).unwrap_or(&[]) {
            if let Some(ops) =
                combine_string_patches("exclude_packages", layer, |view| view.exclude_packages)?
            {
                out = ops.apply_to(&out);
            }
        }
        Ok(out)
    }

    pub(crate) fn targets_list(
        &self,
        workspace_base: &[String],
    ) -> eyre::Result<TargetListResolution> {
        let start = replace_start(&self.layers);
        let mut targets = if start == 0 {
            workspace_base.to_vec()
        } else {
            Vec::new()
        };
        let mut patched = start != 0;
        let mut package_touched = false;
        for layer in self.layers.get(start..).unwrap_or(&[]) {
            if let Some(ops) = combine_target_patches("targets", layer, |view| view.targets)? {
                targets = ops.apply_to(&targets);
                patched = true;
                package_touched = package_touched || layer.scope.is_package();
            }
        }
        Ok(TargetListResolution {
            targets,
            patched,
            package_touched,
        })
    }
}

struct ResolvedFlagResult {
    flags: ResolvedFlags,
    ignored_diagnostics_config: bool,
}

fn replace_start(layers: &[Layer<'_>]) -> usize {
    layers
        .iter()
        .rposition(|layer| layer.entries.iter().any(|(_, view)| view.replace))
        .unwrap_or(0)
}

fn resolve_flags(
    layers: &[Layer<'_>],
    cli_flags: FlagConfig,
    default_diagnostics_allowed: bool,
) -> eyre::Result<ResolvedFlagResult> {
    let mut merged = FlagConfig::default();
    let mut ignored_diagnostics_config = false;
    for layer in layers {
        let mut flags = combine_flags(layer)?;
        flags.validate()?;
        if layer.scope.is_command() {
            if flags.mentions_diagnostics() {
                ignored_diagnostics_config = false;
            }
        } else if !default_diagnostics_allowed {
            flags = super::flags::gated_plain_diagnostics(flags, &mut ignored_diagnostics_config);
        }
        merged.overlay(flags);
    }
    merged.overlay(cli_flags);
    let flags = ResolvedFlags::try_from_config(merged)?;
    Ok(ResolvedFlagResult {
        ignored_diagnostics_config: ignored_diagnostics_config && !flags.diagnostics_only,
        flags,
    })
}

fn combine_flags(layer: &Layer<'_>) -> eyre::Result<FlagConfig> {
    let prefix = layer
        .scope
        .is_command()
        .then(|| format!("subcommands.{}", layer.command.unwrap_or_default()));
    super::combine_flag_configs(
        prefix.as_deref(),
        layer.scope.source_kind(),
        layer.entries.iter().map(|(expr, view)| (*expr, view.flags)),
    )
}

fn resolve_driver(layers: &[Layer<'_>], cli_driver: Option<&str>) -> eyre::Result<Option<String>> {
    let mut out = None;
    for layer in layers {
        if let Some(driver) = super::combine_driver(
            "driver",
            layer.scope.source_kind(),
            &layer.entries,
            |view| view.driver,
        )? {
            out = Some(driver);
        }
    }
    if let Some(driver) = cli_driver {
        out = Some(driver.to_string());
    }
    Ok(out)
}

fn resolve_expand_targets(
    layers: &[Layer<'_>],
    default_enabled: bool,
) -> eyre::Result<(bool, bool)> {
    let mut value = None;
    for layer in layers.iter().filter(|layer| layer.scope.is_command()) {
        let name = format!(
            "subcommands.{}.expand_targets",
            layer.command.unwrap_or_default()
        );
        if let Some(enabled) =
            super::combine_bool(&name, layer.scope.source_kind(), &layer.entries, |view| {
                view.expand_targets
            })?
        {
            value = Some(enabled);
        }
    }
    Ok((value.unwrap_or(default_enabled), value.is_some()))
}

fn resolve_features(layers: &[Layer<'_>]) -> eyre::Result<ResolvedFeatures> {
    let mut out = ResolvedFeatures::default();
    for layer in layers.iter().filter(|layer| layer.scope.is_package()) {
        apply_feature_layer(&mut out, layer)?;
    }
    Ok(out)
}

fn apply_feature_layer(out: &mut ResolvedFeatures, layer: &Layer<'_>) -> eyre::Result<()> {
    let patches: Vec<(&str, &FeatureMatrixPatch)> = layer
        .entries
        .iter()
        .filter_map(|(expr, view)| view.features.map(|features| (*expr, features)))
        .collect();
    if patches.is_empty() {
        return Ok(());
    }

    apply_feature_patches(out, layer.scope.source_kind(), patches)
}

fn apply_single_feature_patch(out: &mut ResolvedFeatures, features: &FeatureMatrixPatch) {
    if let Some(value) = features.skip_optional_dependencies {
        out.skip_optional_dependencies = value;
    }
    if let Some(value) = features.no_empty_feature_set {
        out.no_empty_feature_set = value;
    }
    if let Some(value) = features.max_combinations {
        out.max_combinations = Some(value);
    }

    if let Some(patch) = &features.exclude_features {
        out.exclude_features = SetPatchOps::from_single(patch).apply_to(&out.exclude_features);
    }
    if let Some(patch) = &features.include_features {
        out.include_features = SetPatchOps::from_single(patch).apply_to(&out.include_features);
    }
    if let Some(patch) = &features.only_features {
        out.only_features = SetPatchOps::from_single(patch).apply_to(&out.only_features);
    }

    if let Some(patch) = &features.isolated_feature_sets {
        out.isolated_feature_sets =
            SetPatchOps::from_single(patch).apply_to_feature_sets(&out.isolated_feature_sets);
    }
    if let Some(patch) = &features.exclude_feature_sets {
        out.exclude_feature_sets =
            SetPatchOps::from_single(patch).apply_to_feature_sets(&out.exclude_feature_sets);
    }
    if let Some(patch) = &features.include_feature_sets {
        out.include_feature_sets =
            SetPatchOps::from_single(patch).apply_to_feature_sets(&out.include_feature_sets);
    }
    if let Some(patch) = &features.allow_feature_sets {
        out.allow_feature_sets =
            SetPatchOps::from_single(patch).apply_to_feature_sets(&out.allow_feature_sets);
    }

    if let Some(matrix) = &features.matrix {
        merge_matrix(&mut out.matrix, matrix);
    }
}

fn apply_feature_patches<'a>(
    out: &mut ResolvedFeatures,
    source_kind: &str,
    patches: impl IntoIterator<Item = (&'a str, &'a FeatureMatrixPatch)>,
) -> eyre::Result<()> {
    let patches = patches.into_iter().collect::<Vec<_>>();

    if let Some(value) = super::combine_bool(
        "skip_optional_dependencies",
        source_kind,
        &patches,
        |features| features.skip_optional_dependencies,
    )? {
        out.skip_optional_dependencies = value;
    }
    if let Some(value) =
        super::combine_bool("no_empty_feature_set", source_kind, &patches, |features| {
            features.no_empty_feature_set
        })?
    {
        out.no_empty_feature_set = value;
    }
    if let Some(value) =
        super::combine_u128("max_combinations", source_kind, &patches, |features| {
            features.max_combinations
        })?
    {
        out.max_combinations = Some(value);
    }

    macro_rules! resolve_string_set {
        ($field:ident) => {
            if let Some(ops) = combine_set_patches(
                stringify!($field),
                source_kind,
                patches.iter().filter_map(|(expr, features)| {
                    features.$field.as_ref().map(|patch| (*expr, patch))
                }),
            )? {
                out.$field = ops.apply_to(&out.$field);
            }
        };
    }
    resolve_string_set!(exclude_features);
    resolve_string_set!(include_features);
    resolve_string_set!(only_features);

    macro_rules! resolve_feature_sets {
        ($field:ident) => {
            if let Some(ops) = combine_set_patches(
                stringify!($field),
                source_kind,
                patches.iter().filter_map(|(expr, features)| {
                    features.$field.as_ref().map(|patch| (*expr, patch))
                }),
            )? {
                out.$field = ops.apply_to_feature_sets(&out.$field);
            }
        };
    }
    resolve_feature_sets!(isolated_feature_sets);
    resolve_feature_sets!(exclude_feature_sets);
    resolve_feature_sets!(include_feature_sets);

    let allow_patches: Vec<(&str, &FeatureSetVecPatch)> = patches
        .iter()
        .filter_map(|(expr, features)| {
            features
                .allow_feature_sets
                .as_ref()
                .map(|patch| (*expr, patch))
        })
        .collect();
    if allow_patches.len() > 1 {
        let exprs = allow_patches
            .iter()
            .map(|(expr, _patch)| *expr)
            .collect::<Vec<_>>();
        eyre::bail!(
            "multiple matching {} entries set allow_feature_sets: {}",
            source_kind,
            exprs.join(", ")
        );
    }
    if let Some(ops) = combine_set_patches("allow_feature_sets", source_kind, allow_patches)? {
        out.allow_feature_sets = ops.apply_to_feature_sets(&out.allow_feature_sets);
    }

    for (_expr, features) in patches {
        if let Some(matrix) = features.matrix.as_ref() {
            merge_matrix(&mut out.matrix, matrix);
        }
    }
    Ok(())
}

fn combine_string_patches(
    name: &str,
    layer: &Layer<'_>,
    get: impl Fn(ScopeView<'_>) -> Option<&StringSetPatch>,
) -> eyre::Result<Option<SetPatchOps<String>>> {
    combine_set_patches(
        name,
        layer.scope.source_kind(),
        layer
            .entries
            .iter()
            .filter_map(|(expr, view)| get(*view).map(|patch| (*expr, patch))),
    )
}

fn combine_target_patches(
    name: &str,
    layer: &Layer<'_>,
    get: impl Fn(ScopeView<'_>) -> Option<&TargetListPatch>,
) -> eyre::Result<Option<TargetListOps>> {
    combine_target_list_patches(
        name,
        layer.scope.source_kind(),
        layer
            .entries
            .iter()
            .filter_map(|(expr, view)| get(*view).map(|patch| (*expr, patch))),
    )
}

fn merge_matrix(
    base: &mut serde_json::Map<String, serde_json::Value>,
    patch: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in patch {
        match (base.get_mut(key), value) {
            (Some(serde_json::Value::Object(base_obj)), serde_json::Value::Object(patch_obj)) => {
                merge_matrix(base_obj, patch_obj);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Resolve target-specific feature config with no workspace and no command.
///
/// # Errors
///
/// Returns an error if cfg evaluation fails or if matching overrides conflict.
pub fn resolve_config<E: CfgEvaluator>(
    base: &Config,
    target: &TargetTriple,
    evaluator: &mut E,
) -> eyre::Result<ResolvedFeatures> {
    let ws = WorkspaceConfig::default();
    let matched = matching_overrides(&base.targets, target, evaluator)?;
    Ok(Chain::full(&ws, &[], base, matched, None, None)
        .resolve(
            CliOverlay {
                flags: FlagConfig::default(),
                driver: None,
            },
            ResolvePolicy {
                default_diagnostics_allowed: true,
                default_targets_enabled: true,
            },
        )?
        .features)
}

/// Return the `target.'cfg(...)'` overrides whose cfg expression matches
/// `target`, preserving map order.
pub(crate) fn matching_overrides<'a, V, E: CfgEvaluator>(
    overrides: &'a BTreeMap<String, V>,
    target: &TargetTriple,
    evaluator: &mut E,
) -> eyre::Result<Vec<(&'a str, &'a V)>> {
    let mut matched = Vec::new();
    for (expr, ov) in overrides {
        let is_match = evaluator
            .matches(expr, target)
            .wrap_err_with(|| format!("failed to evaluate cfg expression `{expr}`"))?;
        if is_match {
            matched.push((expr.as_str(), ov));
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::{Chain, CliOverlay, ResolvePolicy, resolve_config};
    use crate::cfg_eval::CfgEvaluator;
    use crate::config::{
        Config, FlagConfig, ScopeConfig, WorkspaceConfig, WorkspaceTargetOverride,
    };
    use crate::target::TargetTriple;
    use color_eyre::eyre;
    use std::collections::{BTreeMap, HashSet};

    struct MatchAll;

    impl CfgEvaluator for MatchAll {
        fn matches(&mut self, _cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            Ok(true)
        }
    }

    fn resolve_base(
        ws: &WorkspaceConfig,
        pkg: Option<&Config>,
        raw: Option<&str>,
        resolved: Option<&str>,
        cli_flags: FlagConfig,
        cli_driver: Option<&str>,
        default_diagnostics_allowed: bool,
    ) -> eyre::Result<super::Resolved> {
        Chain::base(ws, pkg, raw, resolved).resolve(
            CliOverlay {
                flags: cli_flags,
                driver: cli_driver,
            },
            ResolvePolicy {
                default_diagnostics_allowed,
                default_targets_enabled: true,
            },
        )
    }

    #[test]
    fn broad_diagnostics_config_is_gated_for_unsafe_command() -> eyre::Result<()> {
        let mut ws = WorkspaceConfig::default();
        ws.base.settings.flags.diagnostics_only = Some(true);

        let resolved = resolve_base(&ws, None, None, None, FlagConfig::default(), None, false)?;

        assert!(!resolved.flags.diagnostics_only);
        assert!(resolved.ignored_diagnostics_config);
        Ok(())
    }

    #[test]
    fn broad_diagnostics_true_with_dedupe_false_still_warns() -> eyre::Result<()> {
        let mut ws = WorkspaceConfig::default();
        ws.base.settings.flags.diagnostics_only = Some(true);
        ws.base.settings.flags.dedupe = Some(false);

        let resolved = resolve_base(&ws, None, None, None, FlagConfig::default(), None, false)?;

        assert!(!resolved.flags.diagnostics_only);
        assert!(!resolved.flags.dedupe);
        assert!(resolved.ignored_diagnostics_config);
        Ok(())
    }

    #[test]
    fn cli_diagnostics_rescues_gated_broad_config_warning() -> eyre::Result<()> {
        let mut ws = WorkspaceConfig::default();
        ws.base.settings.flags.diagnostics_only = Some(true);

        let resolved = resolve_base(
            &ws,
            None,
            None,
            None,
            FlagConfig {
                diagnostics_only: Some(true),
                ..FlagConfig::default()
            },
            None,
            false,
        )?;

        assert!(resolved.flags.diagnostics_only);
        assert!(!resolved.ignored_diagnostics_config);
        Ok(())
    }

    #[test]
    fn command_dedupe_bypasses_diagnostics_gate() -> eyre::Result<()> {
        let mut ws = WorkspaceConfig::default();
        ws.base.subcommands.insert(
            "test".to_string(),
            ScopeConfig {
                flags: FlagConfig {
                    dedupe: Some(true),
                    ..FlagConfig::default()
                },
                ..ScopeConfig::default()
            },
        );

        let resolved = resolve_base(
            &ws,
            None,
            Some("test"),
            Some("test"),
            FlagConfig::default(),
            None,
            false,
        )?;

        assert!(resolved.flags.diagnostics_only);
        assert!(resolved.flags.dedupe);
        assert!(!resolved.ignored_diagnostics_config);
        Ok(())
    }

    #[test]
    fn package_replace_discards_broader_flags_and_driver() -> eyre::Result<()> {
        let mut ws = WorkspaceConfig::default();
        ws.base.settings.flags.pedantic = Some(true);
        ws.base.settings.driver = Some("cargo-zigbuild".to_string());
        let mut pkg = Config::default();
        pkg.base.settings.replace = true;
        pkg.base.settings.flags.verbose = Some(true);
        pkg.base.settings.driver = Some("cross".to_string());

        let resolved = resolve_base(
            &ws,
            Some(&pkg),
            None,
            None,
            FlagConfig::default(),
            None,
            true,
        )?;

        assert!(!resolved.flags.pedantic);
        assert!(resolved.flags.verbose);
        assert_eq!(resolved.driver.as_deref(), Some("cross"));
        Ok(())
    }

    #[test]
    fn cli_driver_overrides_resolved_driver() -> eyre::Result<()> {
        let mut ws = WorkspaceConfig::default();
        ws.base.settings.driver = Some("cargo-zigbuild".to_string());
        let mut pkg = Config::default();
        pkg.base.settings.driver = Some("cross".to_string());

        let resolved = resolve_base(
            &ws,
            Some(&pkg),
            None,
            None,
            FlagConfig::default(),
            Some("cargo"),
            true,
        )?;

        assert_eq!(resolved.driver.as_deref(), Some("cargo"));
        Ok(())
    }

    #[test]
    fn target_command_errors_use_selected_command_name_for_short_alias() {
        let ws = WorkspaceConfig {
            targets: BTreeMap::from([
                (
                    "cfg(a)".to_string(),
                    WorkspaceTargetOverride {
                        subcommands: BTreeMap::from([(
                            "test".to_string(),
                            ScopeConfig {
                                flags: FlagConfig {
                                    pedantic: Some(true),
                                    ..FlagConfig::default()
                                },
                                ..ScopeConfig::default()
                            },
                        )]),
                        ..WorkspaceTargetOverride::default()
                    },
                ),
                (
                    "cfg(b)".to_string(),
                    WorkspaceTargetOverride {
                        subcommands: BTreeMap::from([(
                            "test".to_string(),
                            ScopeConfig {
                                flags: FlagConfig {
                                    pedantic: Some(false),
                                    ..FlagConfig::default()
                                },
                                ..ScopeConfig::default()
                            },
                        )]),
                        ..WorkspaceTargetOverride::default()
                    },
                ),
            ]),
            ..WorkspaceConfig::default()
        };
        let matched = ws
            .targets
            .iter()
            .map(|(expr, section)| (expr.clone(), section))
            .collect::<Vec<_>>();

        let err = Chain::workspace(&ws, &matched, Some("t"), Some("test"))
            .resolve(
                CliOverlay {
                    flags: FlagConfig::default(),
                    driver: None,
                },
                ResolvePolicy {
                    default_diagnostics_allowed: true,
                    default_targets_enabled: true,
                },
            )
            .expect_err("conflicting target command flags should fail");
        let message = err.to_string();

        assert!(message.contains("subcommands.test.pedantic"), "{message}");
        assert!(!message.contains("subcommands.t.pedantic"), "{message}");
    }

    #[test]
    fn non_replacing_sibling_add_applies_onto_reset_base() -> eyre::Result<()> {
        let raw = serde_json::json!({
            "exclude_features": ["base"],
            "target": {
                "cfg(a)": { "replace": true },
                "cfg(b)": { "exclude_features": { "add": ["sibling"] } },
            },
        });
        crate::config::validate_package_metadata(&raw, "package.metadata.cargo-fc")?;
        let config = serde_json::from_value(raw)?;

        let resolved = resolve_config(
            &config,
            &TargetTriple("x86_64-unknown-linux-gnu".to_string()),
            &mut MatchAll,
        )?;

        assert!(resolved.exclude_features.contains("sibling"));
        assert!(!resolved.exclude_features.contains("base"));
        Ok(())
    }

    #[test]
    fn multiple_matching_replacing_sections_combine_agreeing_overrides() -> eyre::Result<()> {
        let raw = serde_json::json!({
            "exclude_features": ["base"],
            "target": {
                "cfg(a)": {
                    "replace": true,
                    "exclude_features": ["fresh"],
                },
                "cfg(b)": {
                    "replace": true,
                    "exclude_features": ["fresh"],
                },
            },
        });
        crate::config::validate_package_metadata(&raw, "package.metadata.cargo-fc")?;
        let config = serde_json::from_value(raw)?;

        let resolved = resolve_config(
            &config,
            &TargetTriple("x86_64-unknown-linux-gnu".to_string()),
            &mut MatchAll,
        )?;

        assert_eq!(
            resolved.exclude_features,
            HashSet::from(["fresh".to_string()])
        );
        Ok(())
    }
}
