/// Patch types for set-like configuration fields.
pub mod patch;
/// Configuration resolution logic (merge base config with target overrides).
pub mod resolve;

use self::patch::{FeatureSetVecPatch, StringSetPatch};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Per-package configuration for `cargo-feature-combinations`.
///
/// This is read from `[package.metadata.cargo-feature-combinations]` in a
/// package's `Cargo.toml`. For workspace-wide options such as
/// `exclude_packages`, prefer using [`WorkspaceConfig`] via
/// `[workspace.metadata.cargo-feature-combinations]` instead.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct Config {
    /// Feature sets that must be tested in isolation.
    #[serde(default)]
    pub isolated_feature_sets: Vec<HashSet<String>>,
    /// Formerly named `denylist`
    #[serde(default)]
    pub exclude_features: HashSet<String>,
    /// Include these features in every generated feature combination.
    ///
    /// This does not restrict which features are varied for the combinatorial
    /// matrix. To restrict the matrix to a specific allowlist of features, use
    /// [`Config::only_features`].
    #[serde(default)]
    pub include_features: HashSet<String>,
    /// Only consider these features when generating the combinatorial matrix.
    ///
    /// When empty, all package features are considered. Non-existent features
    /// are ignored.
    #[serde(default)]
    pub only_features: HashSet<String>,
    /// When enabled, exclude implicit features that correspond to optional
    /// dependencies from the feature combination matrix.
    ///
    /// This mirrors `cargo-all-features`: only the implicit features that
    /// Cargo generates for optional dependencies (of the form
    /// `foo = ["dep:foo"]` in the feature graph) are skipped. Other
    /// user-defined features that happen to enable optional dependencies via
    /// `dep:NAME` remain part of the matrix.
    ///
    /// By default this is `false` to preserve backwards-compatible behavior.
    #[serde(default)]
    pub skip_optional_dependencies: bool,
    /// Deprecated: kept for backwards compatibility. Prefer
    /// [`WorkspaceConfig::exclude_packages`] via
    /// `[workspace.metadata.cargo-feature-combinations].exclude_packages`.
    #[serde(default)]
    pub exclude_packages: HashSet<String>,
    /// Formerly named `skip_feature_sets`
    #[serde(default)]
    pub exclude_feature_sets: Vec<HashSet<String>>,
    /// Formerly named `exact_combinations`
    #[serde(default)]
    pub include_feature_sets: Vec<HashSet<String>>,
    /// Explicitly allowed feature sets.
    #[serde(default)]
    pub allow_feature_sets: Vec<HashSet<String>>,
    /// When enabled, disallow generating the empty feature set.
    #[serde(default)]
    pub no_empty_feature_set: bool,
    /// Arbitrary user-defined matrix values forwarded to the runner.
    #[serde(default)]
    pub matrix: HashMap<String, serde_json::Value>,

    /// Target-specific configuration overrides.
    ///
    /// This is read from `[package.metadata.cargo-feature-combinations.target.'cfg(...)']`.
    #[serde(default)]
    pub target: BTreeMap<String, TargetOverride>,
    /// Deprecated configuration keys (kept for backwards compatibility).
    #[serde(flatten)]
    pub deprecated: DeprecatedConfig,
}

/// Target-specific configuration override.
///
/// These sections are keyed by Cargo-style cfg expressions, e.g.
/// `cfg(target_os = "linux")`.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct TargetOverride {
    /// When enabled, start from a fresh default configuration instead of
    /// inheriting values from the base config.
    #[serde(default)]
    pub replace: bool,

    /// Patch operations for [`Config::isolated_feature_sets`].
    #[serde(default)]
    pub isolated_feature_sets: Option<FeatureSetVecPatch>,
    /// Patch operations for [`Config::exclude_features`].
    #[serde(default)]
    pub exclude_features: Option<StringSetPatch>,
    /// Patch operations for [`Config::include_features`].
    #[serde(default)]
    pub include_features: Option<StringSetPatch>,
    /// Patch operations for [`Config::only_features`].
    #[serde(default)]
    pub only_features: Option<StringSetPatch>,
    /// Override for [`Config::skip_optional_dependencies`].
    #[serde(default)]
    pub skip_optional_dependencies: Option<bool>,
    /// Patch operations for [`Config::exclude_feature_sets`].
    #[serde(default)]
    pub exclude_feature_sets: Option<FeatureSetVecPatch>,
    /// Patch operations for [`Config::include_feature_sets`].
    #[serde(default)]
    pub include_feature_sets: Option<FeatureSetVecPatch>,
    /// Patch operations for [`Config::allow_feature_sets`].
    #[serde(default)]
    pub allow_feature_sets: Option<FeatureSetVecPatch>,
    /// Override for [`Config::no_empty_feature_set`].
    #[serde(default)]
    pub no_empty_feature_set: Option<bool>,
    /// Merge override for [`Config::matrix`].
    #[serde(default)]
    pub matrix: Option<HashMap<String, serde_json::Value>>,
}

/// Workspace-wide configuration for `cargo-feature-combinations`.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct WorkspaceConfig {
    /// List of package names to exclude from the workspace analysis.
    #[serde(default)]
    pub exclude_packages: HashSet<String>,
}

/// Deprecated configuration keys kept for backwards compatibility.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct DeprecatedConfig {
    /// Former name of [`Config::exclude_feature_sets`].
    #[serde(default)]
    pub skip_feature_sets: Vec<HashSet<String>>,
    /// Former name of [`Config::exclude_features`].
    #[serde(default)]
    pub denylist: HashSet<String>,
    /// Former name of [`Config::include_feature_sets`].
    #[serde(default)]
    pub exact_combinations: Vec<HashSet<String>>,
}
