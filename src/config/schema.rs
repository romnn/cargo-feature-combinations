use super::env::EnvPatch;
use super::flags::FlagConfig;
use super::patch::{FeatureSetVecPatch, StringSetPatch, TargetListPatch};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// What one precedence-chain scope may say.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct ScopeConfig {
    /// Whether this scope inherits everything broader in the precedence chain.
    ///
    /// Defaults to `true`. Set `inherit = false` to start this scope from a
    /// fresh default config instead of inheriting.
    #[serde(default)]
    pub inherit: Option<bool>,
    /// Build driver override.
    #[serde(default)]
    pub driver: Option<String>,
    /// Environment patch for matrix-cell Cargo processes.
    #[serde(default)]
    pub env: Option<EnvPatch>,
    /// Whether this subcommand may expand configured target lists.
    #[serde(default)]
    pub expand_targets: Option<bool>,
    /// Ordered target-list patch.
    #[serde(default)]
    pub targets: Option<TargetListPatch>,
    /// Workspace package-selection patch.
    #[serde(default)]
    pub exclude_packages: Option<StringSetPatch>,
    /// Feature-matrix patches.
    #[serde(default, flatten)]
    pub features: FeatureMatrixPatch,
    /// cargo-fc flag defaults.
    #[serde(default, flatten)]
    pub flags: FlagConfig,
    /// Deprecated scope keys, still accepted as input.
    #[serde(flatten)]
    pub deprecated: DeprecatedScopeKeys,
}

/// Deprecated scope keys kept as accepted input spellings.
///
/// Grouping them here is load-bearing rather than tidy: field names match the
/// TOML spelling exactly, so the `deprecated.` path segment is the only thing
/// marking a use site as backward compatibility. Never hoist one back up into
/// [`ScopeConfig`].
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct DeprecatedScopeKeys {
    /// Former spelling of `inherit = false`.
    #[serde(default)]
    pub replace: bool,
}

impl ScopeConfig {
    /// Whether this scope inherits everything broader in the precedence chain.
    ///
    /// Delegates to the free [`should_inherit()`](fn@should_inherit) helper for
    /// the precedence rule, which config validation also uses.
    #[must_use]
    pub(crate) fn should_inherit(&self) -> bool {
        should_inherit(self.inherit, self.deprecated.replace)
    }
}

/// Whether an `inherit` / `replace` pair keeps inheriting everything broader in
/// the precedence chain (rather than starting from a fresh default config).
///
/// `inherit = false` is the canonical spelling for opting out; `replace = true`
/// is a deprecated alias kept for backward compatibility. An explicit `inherit`
/// wins over the legacy `replace` key. This single rule is shared by config
/// validation (which reads the raw TOML map before deserialization) and
/// resolution (which reads the typed [`ScopeConfig`]) so the two can never
/// disagree.
#[must_use]
pub(crate) fn should_inherit(inherit: Option<bool>, replace: bool) -> bool {
    inherit.unwrap_or(!replace)
}

/// Base or target section plus its command-local overrides.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct SectionConfig {
    /// Settings declared directly in this base or target section.
    #[serde(flatten)]
    pub settings: ScopeConfig,
    /// Command-local settings declared below this section.
    #[serde(default, rename = "subcommands")]
    pub subcommands: BTreeMap<String, ScopeConfig>,
}

/// Metadata root for both package and workspace cargo-fc config.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct RootConfig {
    /// Base settings and base command overrides.
    #[serde(flatten)]
    pub base: SectionConfig,
    /// Target-specific sections keyed by Cargo-style cfg expressions.
    #[serde(default, rename = "target")]
    pub targets: BTreeMap<String, SectionConfig>,
    /// Deprecated TOML keys accepted during config parsing.
    #[serde(flatten)]
    pub(crate) deprecated: DeprecatedTomlKeys,
}

/// Back-compatible package-config type name.
pub type Config = RootConfig;
/// Back-compatible workspace-config type name.
pub type WorkspaceConfig = RootConfig;
/// Back-compatible package target-section type name.
pub type TargetOverride = SectionConfig;
/// Back-compatible workspace target-section type name.
pub type WorkspaceTargetOverride = SectionConfig;
/// Back-compatible subcommand-scope type name.
pub type CommandCapabilities = ScopeConfig;

/// Feature-matrix-shaping patch fields.
///
/// Feature keys and flag keys share one flat TOML table through serde flatten;
/// their names must stay disjoint or one side can silently capture the other.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct FeatureMatrixPatch {
    /// Patch operations for isolated feature sets.
    #[serde(default)]
    pub isolated_feature_sets: Option<FeatureSetVecPatch>,
    /// Patch operations for groups whose members may not be combined.
    #[serde(default)]
    pub mutually_exclusive_features: Option<FeatureSetVecPatch>,
    /// Patch operations for groups of which every combination must enable a member.
    #[serde(default)]
    pub required_feature_groups: Option<FeatureSetVecPatch>,
    /// Patch operations for excluded features.
    #[serde(default)]
    pub exclude_features: Option<StringSetPatch>,
    /// Patch operations for features included in every combination.
    #[serde(default)]
    pub include_features: Option<StringSetPatch>,
    /// Patch operations for the feature allowlist considered by the powerset.
    #[serde(default)]
    pub only_features: Option<StringSetPatch>,
    /// Override for skipping implicit optional-dependency features.
    #[serde(default)]
    pub skip_optional_dependencies: Option<bool>,
    /// Patch operations for excluded feature-set patterns.
    #[serde(default)]
    pub exclude_feature_sets: Option<FeatureSetVecPatch>,
    /// Patch operations for exact feature sets to include.
    #[serde(default)]
    pub include_feature_sets: Option<FeatureSetVecPatch>,
    /// Patch operations for explicitly allowed feature sets.
    #[serde(default)]
    pub allow_feature_sets: Option<FeatureSetVecPatch>,
    /// Override for omitting the empty feature set.
    #[serde(default)]
    pub no_empty_feature_set: Option<bool>,
    /// Merge override for user-defined matrix metadata.
    #[serde(default)]
    pub matrix: Option<serde_json::Map<String, serde_json::Value>>,
    /// Maximum generated feature combinations before cargo-fc fails.
    #[serde(default)]
    pub max_combinations: Option<u128>,
}

/// Deprecated package-level feature keys kept as accepted input spellings.
///
/// Grouping them here is load-bearing rather than tidy: field names match the
/// TOML spelling exactly, so the `deprecated.` path segment is the only thing
/// marking a use site as backward compatibility. Never hoist one back up into
/// [`RootConfig`].
///
/// [`crate::Package::config`] folds these into the keys that replaced them, so
/// nothing downstream of config parsing sees them.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub(crate) struct DeprecatedTomlKeys {
    /// Former name of `exclude_feature_sets`.
    #[serde(default)]
    pub skip_feature_sets: Vec<HashSet<String>>,
    /// Former name of `exclude_features`.
    #[serde(default)]
    pub denylist: HashSet<String>,
    /// Former name of `include_feature_sets`.
    #[serde(default)]
    pub exact_combinations: Vec<HashSet<String>>,
}

#[cfg(test)]
mod tests {
    use super::{FeatureMatrixPatch, ScopeConfig, SectionConfig};
    use crate::config::FlagConfig;
    use crate::config::patch::{StringSetPatch, TargetListPatch};
    use color_eyre::eyre;
    use color_eyre::eyre::{OptionExt as _, WrapErr as _};
    use serde::Serialize;
    use serde_json::Value;
    use std::collections::BTreeSet;

    #[test_util::test]
    fn scope_config_splits_flattened_feature_and_flag_keys() -> eyre::Result<()> {
        let value = serde_json::json!({
            "replace": true,
            "exclude_features": ["gpu"],
            "only_features": { "add": ["core"] },
            "skip_optional_dependencies": true,
            "matrix": { "kind": "ci" },
            "pedantic": true,
        });
        let scope: ScopeConfig =
            serde_json::from_value(value).wrap_err("deserialize ScopeConfig")?;
        assert!(scope.deprecated.replace);
        assert_eq!(scope.flags.pedantic, Some(true));
        assert_eq!(scope.features.skip_optional_dependencies, Some(true));
        assert!(matches!(
            scope.features.exclude_features,
            Some(StringSetPatch::Override(_))
        ));
        assert!(matches!(
            scope.features.only_features,
            Some(StringSetPatch::Patch { .. })
        ));
        assert!(scope.features.matrix.is_some());
        Ok(())
    }

    #[test_util::test]
    fn should_inherit_reads_inherit_then_legacy_replace() -> eyre::Result<()> {
        let cases = [
            (
                serde_json::json!({ "inherit": false }),
                false,
                "inherit false",
            ),
            (serde_json::json!({ "inherit": true }), true, "inherit true"),
            (
                serde_json::json!({ "replace": true }),
                false,
                "legacy replace",
            ),
            // An explicit `inherit` wins over the deprecated `replace` alias.
            (
                serde_json::json!({ "inherit": true, "replace": true }),
                true,
                "explicit inherit wins",
            ),
            (serde_json::json!({}), true, "default"),
        ];
        for (value, expected, name) in cases {
            let scope: ScopeConfig = serde_json::from_value(value).wrap_err(name)?;
            assert_eq!(scope.should_inherit(), expected, "{name}");
        }

        assert!(ScopeConfig::default().should_inherit());
        Ok(())
    }

    #[test_util::test]
    fn section_config_splits_subcommands_from_settings() -> eyre::Result<()> {
        let value = serde_json::json!({
            "targets": ["wasm", "linux"],
            "subcommands": {
                "test": { "expand_targets": false, "verbose": true },
            },
        });
        let section: SectionConfig =
            serde_json::from_value(value).wrap_err("deserialize SectionConfig")?;

        assert!(matches!(
            section.settings.targets,
            Some(TargetListPatch::Override(_))
        ));
        let command = section.subcommands.get("test").ok_or_eyre("test command")?;
        assert_eq!(command.expand_targets, Some(false));
        assert_eq!(command.flags.verbose, Some(true));
        Ok(())
    }

    #[test_util::test]
    fn absent_keys_default_to_none() -> eyre::Result<()> {
        let scope: ScopeConfig =
            serde_json::from_value(serde_json::json!({})).wrap_err("deserialize empty")?;
        assert!(scope.expand_targets.is_none());
        assert!(scope.env.is_none());
        assert!(scope.features.exclude_features.is_none());
        assert!(scope.features.skip_optional_dependencies.is_none());
        Ok(())
    }

    #[test_util::test]
    fn empty_override_array_is_a_replace_not_absent() -> eyre::Result<()> {
        let scope: ScopeConfig = serde_json::from_value(serde_json::json!({ "only_features": [] }))
            .wrap_err("deserialize empty override")?;
        match scope.features.only_features {
            Some(StringSetPatch::Override(set)) => assert!(set.is_empty()),
            other => eyre::bail!("expected empty override, got {other:?}"),
        }
        Ok(())
    }

    #[test_util::test]
    fn flattened_schema_key_sets_are_disjoint() -> eyre::Result<()> {
        let scope = [
            "inherit",
            "replace",
            "driver",
            "env",
            "expand_targets",
            "targets",
            "exclude_packages",
        ]
        .into_iter()
        .map(String::from)
        .collect::<BTreeSet<_>>();
        let features = keys_for(FeatureMatrixPatch::default())?;
        let flags = keys_for(FlagConfig::default())?;

        assert_disjoint("scope", &scope, "features", &features);
        assert_disjoint("scope", &scope, "flags", &flags);
        assert_disjoint("features", &features, "flags", &flags);
        Ok(())
    }

    fn keys_for<T: Serialize>(value: T) -> eyre::Result<BTreeSet<String>> {
        match serde_json::to_value(value).wrap_err("serialize default")? {
            Value::Object(map) => Ok(map.keys().cloned().collect()),
            other => eyre::bail!("expected object, got {other:?}"),
        }
    }

    fn assert_disjoint(a_name: &str, a: &BTreeSet<String>, b_name: &str, b: &BTreeSet<String>) {
        let overlap = a.intersection(b).collect::<Vec<_>>();
        assert!(overlap.is_empty(), "{a_name}/{b_name} overlap: {overlap:?}");
    }
}
