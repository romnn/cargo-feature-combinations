use crate::cfg_eval::CfgEvaluator;
use crate::target::TargetTriple;
use color_eyre::eyre::{self, WrapErr};
use std::collections::BTreeMap;

use super::patch::{FeatureSetVecPatch, StringSetPatch, combine_set_patches};
use super::{CommandCapabilities, Config, FeatureMatrixPatch, FlagConfig, TargetOverride};

/// A package's flag layers after target-specific config has been matched.
#[derive(Debug, Clone, Default)]
pub(crate) struct PackageFlagLayers {
    pub(crate) package_flags: FlagConfig,
    /// Whether the package base set `replace = true` (resets the flag chain from
    /// the package-base layer, discarding inherited workspace flags).
    pub(crate) package_replace: bool,
    /// Package-base build driver override (`[package.metadata.cargo-fc].driver`).
    pub(crate) package_driver: Option<String>,
    pub(crate) package_subcommands: BTreeMap<String, CommandCapabilities>,
    pub(crate) target_flags: FlagConfig,
    /// Whether a matching package target override set `replace = true` (resets
    /// the flag chain from the package-target layer). The flag pass applies it.
    pub(crate) target_replace: bool,
    /// Combined package-target build driver override (from matching
    /// `target.'cfg(...)'` sections).
    pub(crate) target_driver: Option<String>,
    pub(crate) target_subcommands: BTreeMap<String, CommandCapabilities>,
}

/// Target-resolved package config plus the separate flag layers that produced it.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedTargetConfig {
    pub(crate) config: Config,
    pub(crate) flag_layers: PackageFlagLayers,
}

/// Resolve a target-specific effective [`Config`] for the given base config.
///
/// The base config is read from `[package.metadata.cargo-fc]` (or any supported alias).
/// Target overrides are read from
/// `[package.metadata.cargo-fc.target.'cfg(...)']`.
///
/// This function:
///
/// - determines which cfg expressions match the given target
/// - validates and applies `replace = true`
/// - merges override patches deterministically
/// - returns an effective config with `target` metadata removed
///
/// # Errors
///
/// Returns an error if cfg evaluation fails or if overrides conflict.
pub fn resolve_config<E: CfgEvaluator>(
    base: &Config,
    target: &TargetTriple,
    evaluator: &mut E,
) -> eyre::Result<Config> {
    Ok(resolve_config_with_flag_layers(base, target, evaluator, None, None)?.config)
}

/// Resolve the effective feature-matrix [`Config`] for one package, target, and
/// command.
///
/// Feature-shaping fields are resolved by overlaying patch layers in
/// broadest-to-narrowest precedence order, mirroring flag resolution:
///
/// 1. package base config (the starting point)
/// 2. package subcommand override (`subcommands.<cmd>`)
/// 3. package target override (`target.'cfg(...)'`)
/// 4. package target × subcommand override (`target.'cfg(...)'.subcommands.<cmd>`)
///
/// `raw_command` / `resolved_command` select which subcommand override applies;
/// pass `None` (as [`resolve_config`] does) to resolve without any
/// command-scoped feature overrides.
pub(crate) fn resolve_config_with_flag_layers<E: CfgEvaluator>(
    base: &Config,
    target: &TargetTriple,
    evaluator: &mut E,
    raw_command: Option<&str>,
    resolved_command: Option<&str>,
) -> eyre::Result<ResolvedTargetConfig> {
    let matched = matching_overrides(&base.target_overrides, target, evaluator)?;

    // The package feature layers, broadest → narrowest:
    //   L1 = package base config
    //   L2 = package subcommand override (`subcommands.<cmd>`)
    //   L3 = package target override (`target.'cfg(...)'`)
    //   L4 = package target × subcommand override
    // `replace = true` on a layer resets the feature config: the narrowest
    // layer with `replace` discards every broader layer, so resolution starts
    // from defaults and applies from that layer onward. (Config/L1 `replace`
    // concerns the flag/selection chain, not features, which have no broader
    // source than the package base, so it is not a feature reset here.)
    let base_command = crate::cli::selected_command_override(
        raw_command,
        resolved_command,
        &base.subcommand_overrides,
    );
    let target_command_caps: Vec<(&str, &CommandCapabilities)> = matched
        .iter()
        .filter_map(|(expr, ov)| {
            crate::cli::selected_command_override(
                raw_command,
                resolved_command,
                &ov.subcommand_overrides,
            )
            .map(|cap| (*expr, cap))
        })
        .collect();

    let feature_replace_from = feature_replace_layer(&matched, base_command, &target_command_caps)?;

    let mut out = if feature_replace_from.is_some() {
        Config::default()
    } else {
        base.clone()
    };
    // The flag pass (`resolve_command_config`) applies `replace` across the full
    // flag chain, so the package flag inputs are passed through unchanged here;
    // `target_replace` tells that pass whether a matching target override resets.
    let flag_layers = PackageFlagLayers {
        package_flags: base.flags,
        package_replace: base.replace,
        package_driver: base.driver.clone(),
        package_subcommands: base.subcommand_overrides.clone(),
        target_flags: super::combine_flag_configs(
            None,
            "target override",
            matched.iter().map(|(expr, ov)| (*expr, ov.flags)),
        )?,
        target_replace: matched.iter().any(|(_, ov)| ov.replace),
        target_driver: super::combine_driver("driver", "target override", &matched, |ov| {
            ov.driver.as_deref()
        })?,
        target_subcommands: super::combine_command_capability_maps(
            "target override",
            matched
                .iter()
                .map(|(expr, ov)| (*expr, &ov.subcommand_overrides)),
        )?,
    };

    let apply_from = feature_replace_from.unwrap_or(0);

    // Layer 2: package subcommand feature override (skipped if a narrower layer reset).
    if apply_from <= 2
        && let Some(cap) = base_command
    {
        apply_feature_layer(
            &mut out,
            &[("subcommands", &cap.features)],
            "package subcommand override",
        )?;
    }

    // Layer 3: package target feature overrides.
    if apply_from <= 3 {
        let target_feature_layers: Vec<(&str, &FeatureMatrixPatch)> = matched
            .iter()
            .map(|(expr, ov)| (*expr, &ov.features))
            .collect();
        apply_feature_layer(&mut out, &target_feature_layers, "target override")?;
    }

    // Flag layers touch disjoint (non-feature) fields, so their order relative
    // to the feature layers does not matter.
    out.flags.overlay(flag_layers.target_flags);
    for (name, capability) in &flag_layers.target_subcommands {
        out.subcommand_overrides
            .entry(name.clone())
            .or_default()
            .merge(capability);
    }

    // Layer 4: package target × subcommand feature overrides (always the
    // narrowest, so always applied).
    let target_command_layers: Vec<(&str, &FeatureMatrixPatch)> = target_command_caps
        .iter()
        .map(|(expr, cap)| (*expr, &cap.features))
        .collect();
    apply_feature_layer(
        &mut out,
        &target_command_layers,
        "target subcommand override",
    )?;

    // Remove target metadata from the resolved config.
    out.target_overrides.clear();
    out.deprecated = super::schema::DeprecatedTomlKeys::default();
    // `targets` is a selection field consumed before resolution; clear it so the
    // resolved (feature-matrix) config never carries it.
    out.package_targets = None;

    Ok(ResolvedTargetConfig {
        config: out,
        flag_layers,
    })
}

/// Determine the narrowest package feature layer that declares `replace` (L2 =
/// package subcommand, L3 = package target, L4 = target × subcommand), and
/// validate that the resetting layer uses only plain overrides (no add/remove,
/// which would have nothing to add to after the reset). Returns the layer
/// index, or `None` if no feature layer resets.
fn feature_replace_layer(
    matched: &[(&str, &TargetOverride)],
    base_command: Option<&CommandCapabilities>,
    target_command_caps: &[(&str, &CommandCapabilities)],
) -> eyre::Result<Option<usize>> {
    let l3_replace: Vec<&str> = matched
        .iter()
        .filter_map(|(expr, ov)| ov.replace.then_some(*expr))
        .collect();
    if l3_replace.len() > 1 {
        eyre::bail!(
            "multiple matching target overrides have replace = true: {}",
            l3_replace.join(", ")
        );
    }
    let l4_replace: Vec<&str> = target_command_caps
        .iter()
        .filter_map(|(expr, cap)| cap.replace.then_some(*expr))
        .collect();
    if l4_replace.len() > 1 {
        eyre::bail!(
            "multiple matching target subcommand overrides have replace = true: {}",
            l4_replace.join(", ")
        );
    }

    // The narrowest resetting layer wins; validate that layer's patches (a reset
    // may only carry plain overrides) as we select it, so the predicates are
    // evaluated once.
    if !l4_replace.is_empty() {
        for (expr, cap) in target_command_caps {
            validate_replace_feature_patches(expr, "target subcommand override", &cap.features)?;
        }
        Ok(Some(4))
    } else if !l3_replace.is_empty() {
        for (expr, ov) in matched {
            validate_replace_feature_patches(expr, "target override", &ov.features)?;
        }
        Ok(Some(3))
    } else if let Some(cap) = base_command.filter(|cap| cap.replace) {
        validate_replace_feature_patches(
            "subcommands",
            "package subcommand override",
            &cap.features,
        )?;
        Ok(Some(2))
    } else {
        Ok(None)
    }
}

/// Return the `target.'cfg(...)'` overrides whose cfg expression matches
/// `target`, preserving map order. Generic over the override value so both
/// package [`TargetOverride`]s and [`WorkspaceTargetOverride`]s share one
/// cfg-matching loop.
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

fn validate_replace_feature_patches(
    expr: &str,
    source_kind: &str,
    ov: &FeatureMatrixPatch,
) -> eyre::Result<()> {
    let string_set =
        |p: &Option<StringSetPatch>| p.as_ref().is_some_and(StringSetPatch::has_add_or_remove);
    let feature_sets = |p: &Option<FeatureSetVecPatch>| {
        p.as_ref()
            .is_some_and(FeatureSetVecPatch::has_add_or_remove)
    };

    let invalid_fields: Vec<&str> = [
        (
            "isolated_feature_sets",
            feature_sets(&ov.isolated_feature_sets),
        ),
        ("exclude_features", string_set(&ov.exclude_features)),
        ("include_features", string_set(&ov.include_features)),
        ("only_features", string_set(&ov.only_features)),
        (
            "exclude_feature_sets",
            feature_sets(&ov.exclude_feature_sets),
        ),
        (
            "include_feature_sets",
            feature_sets(&ov.include_feature_sets),
        ),
        ("allow_feature_sets", feature_sets(&ov.allow_feature_sets)),
    ]
    .into_iter()
    .filter_map(|(name, uses_add_remove)| uses_add_remove.then_some(name))
    .collect();

    if !invalid_fields.is_empty() {
        eyre::bail!(
            "{source_kind} `{expr}` uses add/remove patch operations while replace=true: {}",
            invalid_fields.join(", ")
        );
    }

    Ok(())
}

/// Combine the sibling patches in one feature layer (conflict-checking any
/// disagreeing overrides) and apply the result onto `out`.
///
/// `entries` are the matched overrides that make up a single precedence layer:
/// either the sibling `cfg(...)` sections that all match the current target, or
/// the single selected subcommand override. Later calls overlay earlier ones,
/// so callers invoke this once per layer in broadest-to-narrowest order.
fn apply_feature_layer(
    out: &mut Config,
    entries: &[(&str, &FeatureMatrixPatch)],
    source_kind: &str,
) -> eyre::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    // Scalar overrides: a present value from a narrower layer replaces the base.
    if let Some(v) = super::combine_bool("skip_optional_dependencies", source_kind, entries, |o| {
        o.skip_optional_dependencies
    })? {
        out.skip_optional_dependencies = v;
    }
    if let Some(v) = super::combine_bool("no_empty_feature_set", source_kind, entries, |o| {
        o.no_empty_feature_set
    })? {
        out.no_empty_feature_set = v;
    }

    // Every set-like field resolves identically: combine this layer's sibling
    // patches, then apply the result onto the running value. The apply method
    // differs only because plain string sets and feature-set lists hold
    // different element types.
    macro_rules! resolve_set_field {
        ($field:ident, $apply:ident) => {
            if let Some(ops) = combine_set_patches(
                stringify!($field),
                source_kind,
                entries
                    .iter()
                    .filter_map(|(expr, o)| o.$field.as_ref().map(|p| (*expr, p))),
            )? {
                out.$field = ops.$apply(&out.$field);
            }
        };
    }
    resolve_set_field!(exclude_features, apply_to);
    resolve_set_field!(include_features, apply_to);
    resolve_set_field!(only_features, apply_to);
    resolve_set_field!(isolated_feature_sets, apply_to_feature_sets);
    resolve_set_field!(exclude_feature_sets, apply_to_feature_sets);
    resolve_set_field!(include_feature_sets, apply_to_feature_sets);

    // allow_feature_sets is treated as a singleton mode switch: at most one
    // matching override in this layer may specify it.
    let allow_patches: Vec<(&str, &FeatureSetVecPatch)> = entries
        .iter()
        .filter_map(|(expr, o)| o.allow_feature_sets.as_ref().map(|p| (*expr, p)))
        .collect();
    if allow_patches.len() > 1 {
        let exprs = allow_patches.iter().map(|(e, _)| *e).collect::<Vec<_>>();
        eyre::bail!(
            "multiple matching {source_kind} entries set allow_feature_sets: {}",
            exprs.join(", ")
        );
    }
    if let Some(ops) = combine_set_patches("allow_feature_sets", source_kind, allow_patches)? {
        out.allow_feature_sets = ops.apply_to_feature_sets(&out.allow_feature_sets);
    }

    // Matrix metadata: deep-merge in deterministic order (cfg key order).
    for (_expr, o) in entries {
        if let Some(matrix) = o.matrix.as_ref() {
            merge_matrix(&mut out.matrix, matrix);
        }
    }

    Ok(())
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

#[cfg(test)]
mod test {
    use super::{resolve_config, resolve_config_with_flag_layers};
    use crate::cfg_eval::CfgEvaluator;
    use crate::config::patch::{FeatureSetVecPatch, StringSetPatch};
    use crate::config::{
        CommandCapabilities, Config, FeatureMatrixPatch, FlagConfig, ResolveCommandConfigArgs,
        ResolvedFlags, TargetOverride, WorkspaceConfig, resolve_command_config,
    };
    use crate::target::TargetTriple;
    use color_eyre::eyre;
    use std::collections::{BTreeMap, HashSet};

    #[derive(Default)]
    struct StubEval {
        matches: HashSet<String>,
    }

    impl CfgEvaluator for StubEval {
        fn matches(&mut self, cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            Ok(self.matches.contains(cfg_expr))
        }
    }

    fn hs(values: &[&str]) -> HashSet<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    fn hss(sets: &[&[&str]]) -> Vec<HashSet<String>> {
        sets.iter().map(|s| hs(s)).collect()
    }

    /// A `TargetOverride` carrying only the given feature-matrix patch.
    fn target_features(features: FeatureMatrixPatch) -> TargetOverride {
        TargetOverride {
            features,
            ..TargetOverride::default()
        }
    }

    /// A `CommandCapabilities` carrying only the given feature-matrix patch.
    fn command_features(features: FeatureMatrixPatch) -> CommandCapabilities {
        CommandCapabilities {
            features,
            ..CommandCapabilities::default()
        }
    }

    #[test]
    fn additive_exclude_features() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["cuda"]),
                    remove: HashSet::new(),
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("cuda"));
        Ok(())
    }

    fn resolve_for_command(
        base: &Config,
        command: Option<&str>,
        matches: &[&str],
    ) -> eyre::Result<Config> {
        let mut eval = StubEval::default();
        for m in matches {
            eval.matches.insert((*m).to_string());
        }
        Ok(resolve_config_with_flag_layers(
            base,
            &TargetTriple("x".to_string()),
            &mut eval,
            command,
            command,
        )?
        .config)
    }

    #[test]
    fn package_subcommand_feature_override_applies_only_for_that_command() -> eyre::Result<()> {
        let base = Config {
            subcommand_overrides: BTreeMap::from([(
                "test".to_string(),
                command_features(FeatureMatrixPatch {
                    exclude_features: Some(StringSetPatch::Override(hs(&["gpu"]))),
                    ..FeatureMatrixPatch::default()
                }),
            )]),
            ..Config::default()
        };

        // The `test` subcommand override shapes the matrix for `cargo fc test`.
        let for_test = resolve_for_command(&base, Some("test"), &[])?;
        assert!(for_test.exclude_features.contains("gpu"));

        // A different command and the command-less path are both unaffected.
        let for_build = resolve_for_command(&base, Some("build"), &[])?;
        assert!(!for_build.exclude_features.contains("gpu"));
        let no_command = resolve_for_command(&base, None, &[])?;
        assert!(!no_command.exclude_features.contains("gpu"));
        Ok(())
    }

    #[test]
    fn target_subcommand_feature_override_applies() -> eyre::Result<()> {
        let base = Config {
            target_overrides: BTreeMap::from([(
                "cfg(unix)".to_string(),
                TargetOverride {
                    subcommand_overrides: BTreeMap::from([(
                        "test".to_string(),
                        command_features(FeatureMatrixPatch {
                            only_features: Some(StringSetPatch::Override(hs(&["a"]))),
                            ..FeatureMatrixPatch::default()
                        }),
                    )]),
                    ..TargetOverride::default()
                },
            )]),
            ..Config::default()
        };

        let for_test = resolve_for_command(&base, Some("test"), &["cfg(unix)"])?;
        assert_eq!(for_test.only_features, hs(&["a"]));

        // Same target, different command: the target×subcommand layer is inert.
        let for_build = resolve_for_command(&base, Some("build"), &["cfg(unix)"])?;
        assert!(for_build.only_features.is_empty());
        Ok(())
    }

    #[test]
    fn feature_layer_precedence_target_beats_subcommand() -> eyre::Result<()> {
        // Each layer overrides `only_features`; the narrowest applicable layer
        // must win. Precedence (narrowest last): package base → package
        // subcommand → package target → package target×subcommand.
        let base = Config {
            only_features: hs(&["base"]),
            subcommand_overrides: BTreeMap::from([(
                "test".to_string(),
                command_features(FeatureMatrixPatch {
                    only_features: Some(StringSetPatch::Override(hs(&["sub"]))),
                    ..FeatureMatrixPatch::default()
                }),
            )]),
            target_overrides: BTreeMap::from([(
                "cfg(unix)".to_string(),
                TargetOverride {
                    features: FeatureMatrixPatch {
                        only_features: Some(StringSetPatch::Override(hs(&["target"]))),
                        ..FeatureMatrixPatch::default()
                    },
                    subcommand_overrides: BTreeMap::from([(
                        "test".to_string(),
                        command_features(FeatureMatrixPatch {
                            only_features: Some(StringSetPatch::Override(hs(&["target-sub"]))),
                            ..FeatureMatrixPatch::default()
                        }),
                    )]),
                    ..TargetOverride::default()
                },
            )]),
            ..Config::default()
        };

        // All four layers present: target×subcommand is narrowest.
        let all = resolve_for_command(&base, Some("test"), &["cfg(unix)"])?;
        assert_eq!(all.only_features, hs(&["target-sub"]));

        // Drop the target×subcommand layer (different command): package target
        // beats the package subcommand layer.
        let no_target_sub = resolve_for_command(&base, Some("build"), &["cfg(unix)"])?;
        assert_eq!(no_target_sub.only_features, hs(&["target"]));

        // No matching target: the package subcommand layer beats the base.
        let sub_only = resolve_for_command(&base, Some("test"), &[])?;
        assert_eq!(sub_only.only_features, hs(&["sub"]));
        Ok(())
    }

    #[test]
    fn replace_resets_base_subcommand_layer_but_keeps_target_subcommand() -> eyre::Result<()> {
        // `replace = true` on the matching target discards everything
        // package-base-scoped (including the base subcommand layer), while the
        // target×subcommand layer is target-scoped and still applies.
        let base = Config {
            only_features: hs(&["base"]),
            subcommand_overrides: BTreeMap::from([(
                "test".to_string(),
                command_features(FeatureMatrixPatch {
                    only_features: Some(StringSetPatch::Override(hs(&["sub"]))),
                    ..FeatureMatrixPatch::default()
                }),
            )]),
            target_overrides: BTreeMap::from([(
                "cfg(unix)".to_string(),
                TargetOverride {
                    replace: true,
                    subcommand_overrides: BTreeMap::from([(
                        "test".to_string(),
                        command_features(FeatureMatrixPatch {
                            only_features: Some(StringSetPatch::Override(hs(&["target-sub"]))),
                            ..FeatureMatrixPatch::default()
                        }),
                    )]),
                    ..TargetOverride::default()
                },
            )]),
            ..Config::default()
        };

        let out = resolve_for_command(&base, Some("test"), &["cfg(unix)"])?;
        // Base and base-subcommand layers are gone; only the target×subcommand
        // override remains.
        assert_eq!(out.only_features, hs(&["target-sub"]));
        Ok(())
    }

    #[test]
    fn replace_at_package_subcommand_resets_base_features() -> eyre::Result<()> {
        // `replace = true` on a package subcommand override discards the package
        // base features for that command.
        let base = Config {
            only_features: hs(&["base"]),
            subcommand_overrides: BTreeMap::from([(
                "test".to_string(),
                CommandCapabilities {
                    replace: true,
                    features: FeatureMatrixPatch {
                        only_features: Some(StringSetPatch::Override(hs(&["sub"]))),
                        ..FeatureMatrixPatch::default()
                    },
                    ..CommandCapabilities::default()
                },
            )]),
            ..Config::default()
        };

        // For `test` the base is discarded; only the subcommand's features remain.
        let for_test = resolve_for_command(&base, Some("test"), &[])?;
        assert_eq!(for_test.only_features, hs(&["sub"]));

        // For a command with no replacing override, the base is intact.
        let for_build = resolve_for_command(&base, Some("build"), &[])?;
        assert_eq!(for_build.only_features, hs(&["base"]));
        Ok(())
    }

    #[test]
    fn replace_at_target_subcommand_resets_all_broader_layers() -> eyre::Result<()> {
        // The target×subcommand layer is the narrowest, so its `replace` discards
        // the base, package-subcommand, and target layers.
        let base = Config {
            only_features: hs(&["base"]),
            subcommand_overrides: BTreeMap::from([(
                "test".to_string(),
                command_features(FeatureMatrixPatch {
                    only_features: Some(StringSetPatch::Override(hs(&["sub"]))),
                    ..FeatureMatrixPatch::default()
                }),
            )]),
            target_overrides: BTreeMap::from([(
                "cfg(unix)".to_string(),
                TargetOverride {
                    features: FeatureMatrixPatch {
                        only_features: Some(StringSetPatch::Override(hs(&["target"]))),
                        ..FeatureMatrixPatch::default()
                    },
                    subcommand_overrides: BTreeMap::from([(
                        "test".to_string(),
                        CommandCapabilities {
                            replace: true,
                            features: FeatureMatrixPatch {
                                only_features: Some(StringSetPatch::Override(hs(&["target-sub"]))),
                                ..FeatureMatrixPatch::default()
                            },
                            ..CommandCapabilities::default()
                        },
                    )]),
                    ..TargetOverride::default()
                },
            )]),
            ..Config::default()
        };

        let out = resolve_for_command(&base, Some("test"), &["cfg(unix)"])?;
        assert_eq!(out.only_features, hs(&["target-sub"]));
        Ok(())
    }

    #[test]
    fn replace_at_subcommand_disallows_add_remove() {
        // A resetting section starts from defaults, so add/remove there is
        // rejected — there is nothing to add to.
        let base = Config {
            subcommand_overrides: BTreeMap::from([(
                "test".to_string(),
                CommandCapabilities {
                    replace: true,
                    features: FeatureMatrixPatch {
                        exclude_features: Some(StringSetPatch::Patch {
                            r#override: None,
                            add: hs(&["a"]),
                            remove: HashSet::new(),
                        }),
                        ..FeatureMatrixPatch::default()
                    },
                    ..CommandCapabilities::default()
                },
            )]),
            ..Config::default()
        };

        let err = resolve_for_command(&base, Some("test"), &[])
            .expect_err("replace + add/remove should fail");
        assert!(err.to_string().contains("replace=true"));
    }

    #[test]
    fn override_exclude_features_array_syntax() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Override(hs(&["cuda"]))),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(!out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("cuda"));
        Ok(())
    }

    #[test]
    fn conflicting_override_errors() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Override(hs(&["a"]))),
                ..FeatureMatrixPatch::default()
            }),
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Override(hs(&["b"]))),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let Err(err) = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval) else {
            eyre::bail!("expected conflicting override resolution to fail");
        };
        assert!(err.to_string().contains("conflicting overrides"));
        Ok(())
    }

    #[test]
    fn replace_disallows_add_remove() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                replace: true,
                features: FeatureMatrixPatch {
                    exclude_features: Some(StringSetPatch::Patch {
                        r#override: None,
                        add: hs(&["cuda"]),
                        remove: HashSet::new(),
                    }),
                    ..FeatureMatrixPatch::default()
                },
                ..TargetOverride::default()
            },
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let Err(err) = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval) else {
            eyre::bail!("expected replace=true add/remove validation to fail");
        };
        assert!(err.to_string().contains("replace=true"));
        Ok(())
    }

    #[test]
    fn replace_starts_from_default() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            skip_optional_dependencies: true,
            ..Config::default()
        };
        base.matrix
            .insert("k".to_string(), serde_json::json!({"a": 1}));

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                replace: true,
                features: FeatureMatrixPatch {
                    exclude_features: Some(StringSetPatch::Override(hs(&["cuda"]))),
                    matrix: Some({
                        let mut m = serde_json::Map::new();
                        m.insert("k".to_string(), serde_json::json!({"b": 2}));
                        m
                    }),
                    ..FeatureMatrixPatch::default()
                },
                ..TargetOverride::default()
            },
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;

        // base fields are not inherited
        assert!(!out.exclude_features.contains("default"));
        assert!(!out.skip_optional_dependencies);

        // override is applied
        assert!(out.exclude_features.contains("cuda"));

        // matrix is merged onto default (empty)
        let v = out
            .matrix
            .get("k")
            .ok_or_else(|| eyre::eyre!("missing matrix key"))?;
        assert!(v.get("b").is_some());

        Ok(())
    }

    #[test]
    fn no_match_returns_base_unchanged() -> eyre::Result<()> {
        let base = Config {
            exclude_features: hs(&["default"]),
            skip_optional_dependencies: true,
            ..Config::default()
        };

        let mut eval = StubEval::default();
        // No matches configured, so nothing matches.

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.exclude_features, hs(&["default"]));
        assert!(out.skip_optional_dependencies);
        assert!(
            out.target_overrides.is_empty(),
            "target metadata should be cleared"
        );
        Ok(())
    }

    #[test]
    fn remove_exclude_features() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default", "cuda", "metal"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: HashSet::new(),
                    remove: hs(&["cuda"]),
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("metal"));
        assert!(
            !out.exclude_features.contains("cuda"),
            "cuda should be removed"
        );
        Ok(())
    }

    #[test]
    fn multiple_matching_sections_combine_adds() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["a"]),
                    remove: HashSet::new(),
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["b"]),
                    remove: HashSet::new(),
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("a"));
        assert!(out.exclude_features.contains("b"));
        Ok(())
    }

    #[test]
    fn add_wins_over_remove_for_same_value() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["cuda"]),
                    remove: hs(&["cuda"]),
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        // add is applied after remove, so "cuda" should be present.
        assert!(out.exclude_features.contains("cuda"));
        Ok(())
    }

    #[test]
    fn boolean_override_no_empty_feature_set() -> eyre::Result<()> {
        let mut base = Config {
            no_empty_feature_set: false,
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                no_empty_feature_set: Some(true),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.no_empty_feature_set);
        Ok(())
    }

    #[test]
    fn boolean_override_prune_implied() -> eyre::Result<()> {
        let mut base = Config::default();
        assert_eq!(base.flags.prune_implied, None);

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                flags: FlagConfig {
                    prune_implied: Some(false),
                    ..FlagConfig::default()
                },
                ..TargetOverride::default()
            },
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.flags.prune_implied, Some(false));
        Ok(())
    }

    #[test]
    fn boolean_override_diagnostics_config() -> eyre::Result<()> {
        let mut base = Config {
            flags: FlagConfig {
                diagnostics_only: Some(false),
                dedupe: Some(false),
                ..FlagConfig::default()
            },
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                flags: FlagConfig {
                    diagnostics_only: Some(true),
                    dedupe: Some(true),
                    ..FlagConfig::default()
                },
                ..TargetOverride::default()
            },
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.flags.diagnostics_only, Some(true));
        assert_eq!(out.flags.dedupe, Some(true));
        Ok(())
    }

    #[test]
    fn target_flag_layers_resolve_after_package_subcommand_layers() -> eyre::Result<()> {
        let mut base = Config {
            flags: FlagConfig {
                pedantic: Some(false),
                ..FlagConfig::default()
            },
            ..Config::default()
        };
        base.subcommand_overrides.insert(
            "check".to_string(),
            CommandCapabilities {
                flags: FlagConfig {
                    pedantic: Some(true),
                    ..FlagConfig::default()
                },
                ..CommandCapabilities::default()
            },
        );

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                flags: FlagConfig {
                    pedantic: Some(false),
                    errors_only: Some(false),
                    ..FlagConfig::default()
                },
                subcommand_overrides: BTreeMap::from([(
                    "check".to_string(),
                    CommandCapabilities {
                        flags: FlagConfig {
                            errors_only: Some(true),
                            ..FlagConfig::default()
                        },
                        ..CommandCapabilities::default()
                    },
                )]),
                ..TargetOverride::default()
            },
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let resolved = resolve_config_with_flag_layers(
            &base,
            &TargetTriple("x".to_string()),
            &mut eval,
            Some("check"),
            Some("check"),
        )?;
        let workspace = WorkspaceConfig::default();
        let empty_target_subcommands = BTreeMap::new();
        let flags = resolve_command_config(ResolveCommandConfigArgs {
            workspace: &workspace,
            workspace_target_flags: FlagConfig::default(),
            workspace_target_replace: false,
            workspace_target_driver: None,
            workspace_target_subcommands: &empty_target_subcommands,
            package_flags: resolved.flag_layers.package_flags,
            package_replace: resolved.flag_layers.package_replace,
            package_driver: resolved.flag_layers.package_driver.as_deref(),
            package_subcommands: &resolved.flag_layers.package_subcommands,
            package_target_flags: resolved.flag_layers.target_flags,
            package_target_replace: resolved.flag_layers.target_replace,
            package_target_driver: resolved.flag_layers.target_driver.as_deref(),
            package_target_subcommands: &resolved.flag_layers.target_subcommands,
            raw_command: Some("check"),
            resolved_command: Some("check"),
            cli_flags: FlagConfig::default(),
            cli_driver: None,
            default_diagnostics_allowed: true,
            default_targets_enabled: true,
        })?
        .flags;

        assert_eq!(
            flags,
            ResolvedFlags {
                pedantic: false,
                errors_only: true,
                ..ResolvedFlags::default()
            }
        );
        Ok(())
    }

    #[test]
    fn conflicting_target_subcommand_flags_error() {
        let mut base = Config::default();
        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                subcommand_overrides: BTreeMap::from([(
                    "check".to_string(),
                    CommandCapabilities {
                        flags: FlagConfig {
                            pedantic: Some(true),
                            ..FlagConfig::default()
                        },
                        ..CommandCapabilities::default()
                    },
                )]),
                ..TargetOverride::default()
            },
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                subcommand_overrides: BTreeMap::from([(
                    "check".to_string(),
                    CommandCapabilities {
                        flags: FlagConfig {
                            pedantic: Some(false),
                            ..FlagConfig::default()
                        },
                        ..CommandCapabilities::default()
                    },
                )]),
                ..TargetOverride::default()
            },
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let err = resolve_config_with_flag_layers(
            &base,
            &TargetTriple("x".to_string()),
            &mut eval,
            Some("check"),
            Some("check"),
        )
        .expect_err("conflicting target subcommand flags should fail");

        assert!(err.to_string().contains("subcommands.check.pedantic"));
    }

    #[test]
    fn feature_set_vec_patch_add_include_feature_sets() -> eyre::Result<()> {
        let mut base = Config {
            include_feature_sets: hss(&[&["a", "b"]]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                include_feature_sets: Some(FeatureSetVecPatch::Patch {
                    r#override: None,
                    add: hss(&[&["c", "d"]]),
                    remove: Vec::new(),
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.include_feature_sets.len(), 2);
        let sets: Vec<HashSet<String>> = out.include_feature_sets;
        assert!(sets.contains(&hs(&["a", "b"])));
        assert!(sets.contains(&hs(&["c", "d"])));
        Ok(())
    }

    #[test]
    fn feature_set_vec_patch_remove() -> eyre::Result<()> {
        let mut base = Config {
            include_feature_sets: hss(&[&["a", "b"], &["c"]]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                include_feature_sets: Some(FeatureSetVecPatch::Patch {
                    r#override: None,
                    add: Vec::new(),
                    remove: hss(&[&["a", "b"]]),
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.include_feature_sets.len(), 1);
        assert!(out.include_feature_sets.contains(&hs(&["c"])));
        Ok(())
    }

    #[test]
    fn matrix_metadata_merge_adds_new_key() -> eyre::Result<()> {
        let mut base = Config::default();
        base.matrix
            .insert("existing".to_string(), serde_json::json!("keep"));
        base.matrix.insert(
            "nested".to_string(),
            serde_json::json!({
                "keep": true,
                "replace": "base"
            }),
        );
        base.matrix
            .insert("tags".to_string(), serde_json::json!(["base"]));

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                matrix: Some({
                    let mut m = serde_json::Map::new();
                    m.insert("added".to_string(), serde_json::json!("new"));
                    m.insert(
                        "nested".to_string(),
                        serde_json::json!({
                            "replace": "override",
                            "added": true
                        }),
                    );
                    m.insert("tags".to_string(), serde_json::json!(["override"]));
                    m
                }),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(
            out.matrix.get("existing"),
            Some(&serde_json::json!("keep")),
            "original key preserved"
        );
        assert_eq!(
            out.matrix.get("added"),
            Some(&serde_json::json!("new")),
            "new key added from patch"
        );
        assert_eq!(
            out.matrix.get("nested"),
            Some(&serde_json::json!({
                "keep": true,
                "replace": "override",
                "added": true
            })),
            "nested objects are merged recursively"
        );
        assert_eq!(
            out.matrix.get("tags"),
            Some(&serde_json::json!(["override"])),
            "arrays replace the base value"
        );
        Ok(())
    }

    #[test]
    fn allow_feature_sets_singleton_conflict() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            target_features(FeatureMatrixPatch {
                allow_feature_sets: Some(FeatureSetVecPatch::Override(hss(&[&["a"]]))),
                ..FeatureMatrixPatch::default()
            }),
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            target_features(FeatureMatrixPatch {
                allow_feature_sets: Some(FeatureSetVecPatch::Override(hss(&[&["b"]]))),
                ..FeatureMatrixPatch::default()
            }),
        );
        base.target_overrides = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let Err(err) = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval) else {
            eyre::bail!("expected allow_feature_sets singleton conflict");
        };
        assert!(err.to_string().contains("allow_feature_sets"));
        Ok(())
    }

    #[test]
    fn target_override_prune_spelling_conflict_errors() -> eyre::Result<()> {
        let mut base = Config::default();
        base.target_overrides.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                flags: FlagConfig {
                    prune_implied: Some(true),
                    ..FlagConfig::default()
                },
                ..TargetOverride::default()
            },
        );
        base.target_overrides.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                flags: FlagConfig {
                    no_prune_implied: Some(false),
                    ..FlagConfig::default()
                },
                ..TargetOverride::default()
            },
        );
        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let Err(err) = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval) else {
            eyre::bail!("expected prune spelling conflict");
        };

        assert!(err.to_string().contains("no_prune_implied"));
        Ok(())
    }
}
