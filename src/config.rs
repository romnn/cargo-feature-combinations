use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Per-package configuration for `cargo-feature-combinations`.
///
/// This is read from `[package.metadata.cargo-feature-combinations]` in a
/// package's `Cargo.toml`. For workspace-wide options such as
/// `exclude_packages`, prefer using [`WorkspaceConfig`] via
/// `[workspace.metadata.cargo-feature-combinations]` instead.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
    #[serde(default)]
    pub isolated_feature_sets: Vec<HashSet<String>>,
    /// Formerly named `denylist`
    #[serde(default)]
    pub exclude_features: HashSet<String>,
    #[serde(default)]
    pub include_features: HashSet<String>,
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
    #[serde(default)]
    pub matrix: HashMap<String, serde_json::Value>,
    #[serde(flatten)]
    pub deprecated: DeprecatedConfig,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct WorkspaceConfig {
    /// List of package names to exclude from the workspace analysis.
    #[serde(default)]
    pub exclude_packages: HashSet<String>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct DeprecatedConfig {
    #[serde(default)]
    pub skip_feature_sets: Vec<HashSet<String>>,
    #[serde(default)]
    pub denylist: HashSet<String>,
    #[serde(default)]
    pub exact_combinations: Vec<HashSet<String>>,
}
