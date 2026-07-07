use super::flags::FlagConfig;
use super::patch::{FeatureSetVecPatch, StringSetPatch, TargetListPatch};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// What one precedence-chain scope may say.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct ScopeConfig {
    /// When enabled, discard everything broader in the precedence chain.
    #[serde(default)]
    pub replace: bool,
    /// Build driver override.
    #[serde(default)]
    pub driver: Option<String>,
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
}

/// Deprecated package-level feature keys kept as accepted input spellings.
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
    use serde::Serialize;
    use serde_json::Value;
    use std::collections::BTreeSet;

    #[test]
    fn scope_config_splits_flattened_feature_and_flag_keys() {
        let value = serde_json::json!({
            "replace": true,
            "exclude_features": ["gpu"],
            "only_features": { "add": ["core"] },
            "skip_optional_dependencies": true,
            "matrix": { "kind": "ci" },
            "pedantic": true,
        });
        let scope: ScopeConfig = serde_json::from_value(value).expect("deserialize ScopeConfig");
        assert!(scope.replace);
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
    }

    #[test]
    fn section_config_splits_subcommands_from_settings() {
        let value = serde_json::json!({
            "targets": ["wasm", "linux"],
            "subcommands": {
                "test": { "expand_targets": false, "verbose": true },
            },
        });
        let section: SectionConfig =
            serde_json::from_value(value).expect("deserialize SectionConfig");

        assert!(matches!(
            section.settings.targets,
            Some(TargetListPatch::Override(_))
        ));
        let command = section.subcommands.get("test").expect("test command");
        assert_eq!(command.expand_targets, Some(false));
        assert_eq!(command.flags.verbose, Some(true));
    }

    #[test]
    fn absent_keys_default_to_none() {
        let scope: ScopeConfig =
            serde_json::from_value(serde_json::json!({})).expect("deserialize empty");
        assert!(scope.expand_targets.is_none());
        assert!(scope.features.exclude_features.is_none());
        assert!(scope.features.skip_optional_dependencies.is_none());
    }

    #[test]
    fn empty_override_array_is_a_replace_not_absent() {
        let scope: ScopeConfig = serde_json::from_value(serde_json::json!({ "only_features": [] }))
            .expect("deserialize empty override");
        match scope.features.only_features {
            Some(StringSetPatch::Override(set)) => assert!(set.is_empty()),
            other => panic!("expected empty override, got {other:?}"),
        }
    }

    #[test]
    fn flattened_schema_key_sets_are_disjoint() {
        let scope = [
            "replace",
            "driver",
            "expand_targets",
            "targets",
            "exclude_packages",
        ]
        .into_iter()
        .map(String::from)
        .collect::<BTreeSet<_>>();
        let features = keys_for(FeatureMatrixPatch::default());
        let flags = keys_for(FlagConfig::default());

        assert_disjoint("scope", &scope, "features", &features);
        assert_disjoint("scope", &scope, "flags", &flags);
        assert_disjoint("features", &features, "flags", &flags);
    }

    fn keys_for<T: Serialize>(value: T) -> BTreeSet<String> {
        match serde_json::to_value(value).expect("serialize default") {
            Value::Object(map) => map.keys().cloned().collect(),
            other => panic!("expected object, got {other:?}"),
        }
    }

    fn assert_disjoint(a_name: &str, a: &BTreeSet<String>, b_name: &str, b: &BTreeSet<String>) {
        let overlap = a.intersection(b).collect::<Vec<_>>();
        assert!(overlap.is_empty(), "{a_name}/{b_name} overlap: {overlap:?}");
    }
}
