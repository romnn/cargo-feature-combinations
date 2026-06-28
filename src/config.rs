/// Patch types for set-like configuration fields.
pub mod patch;
/// Configuration resolution logic (merge base config with target overrides).
pub mod resolve;

use self::patch::{FeatureSetVecPatch, StringSetPatch};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

fn default_true() -> bool {
    true
}

/// Per-package configuration for `cargo-fc`.
///
/// This is read from `[package.metadata.cargo-fc]` (or any supported alias)
/// in a package's `Cargo.toml`. For workspace-wide options such as
/// `exclude_packages`, prefer using [`WorkspaceConfig`] via
/// `[workspace.metadata.cargo-fc]` instead.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Package-level target triples to check by default.
    ///
    /// This is a target *selection* field, not a feature-matrix field:
    ///
    /// - `None` (key absent): inherit the workspace target list.
    /// - `Some([])`: explicit opt-out of the workspace target list; use the
    ///   single effective target (`CARGO_BUILD_TARGET` or host) instead.
    /// - `Some([..])`: this package's own target list, overriding the workspace
    ///   list.
    ///
    /// `targets` is never read by feature-combination generation. Target
    /// override sections (`target.'cfg(...)'`) must not change it.
    #[serde(default)]
    pub targets: Option<Vec<String>>,
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
    /// `[workspace.metadata.cargo-fc].exclude_packages`.
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
    /// When enabled, automatically detect and skip redundant feature
    /// combinations whose resolved feature set (after Cargo's feature
    /// unification) is identical to a smaller combination.
    ///
    /// Defaults to `true`.
    #[serde(default = "default_true")]
    pub prune_implied: bool,
    /// When enabled, include pruned feature combinations in the summary
    /// output. Pruned combinations are hidden by default.
    #[serde(default)]
    pub show_pruned: bool,
    /// Arbitrary user-defined matrix values forwarded to the runner.
    #[serde(default)]
    pub matrix: HashMap<String, serde_json::Value>,

    /// Target-specific configuration overrides.
    ///
    /// This is read from `[package.metadata.cargo-fc.target.'cfg(...)']`.
    #[serde(default)]
    pub target: BTreeMap<String, TargetOverride>,
    /// Deprecated configuration keys (kept for backwards compatibility).
    #[serde(flatten)]
    pub deprecated: DeprecatedConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            targets: None,
            isolated_feature_sets: Vec::new(),
            exclude_features: HashSet::new(),
            include_features: HashSet::new(),
            only_features: HashSet::new(),
            skip_optional_dependencies: false,
            exclude_packages: HashSet::new(),
            exclude_feature_sets: Vec::new(),
            include_feature_sets: Vec::new(),
            allow_feature_sets: Vec::new(),
            no_empty_feature_set: false,
            prune_implied: true,
            show_pruned: false,
            matrix: HashMap::new(),
            target: BTreeMap::new(),
            deprecated: DeprecatedConfig::default(),
        }
    }
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
    /// Override for [`Config::prune_implied`].
    #[serde(default)]
    pub prune_implied: Option<bool>,
    /// Override for [`Config::show_pruned`].
    #[serde(default)]
    pub show_pruned: Option<bool>,
    /// Merge override for [`Config::matrix`].
    #[serde(default)]
    pub matrix: Option<HashMap<String, serde_json::Value>>,
}

/// Workspace-wide configuration for `cargo-fc`.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct WorkspaceConfig {
    /// List of package names to exclude from the workspace analysis.
    #[serde(default)]
    pub exclude_packages: HashSet<String>,
    /// Target triples checked by default for the whole workspace.
    ///
    /// An empty list means "no configured target list"; behavior falls back to
    /// the existing single effective target detection path. Package-level
    /// `targets` override (do not merge with) this list.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Target-specific workspace overrides keyed by Cargo-style cfg expressions.
    ///
    /// These may patch `exclude_packages` only; they select which workspace
    /// packages participate for one already-selected target.
    #[serde(default)]
    pub target: BTreeMap<String, WorkspaceTargetOverride>,
    /// Per-subcommand configured-target policy.
    ///
    /// Built-in Cargo subcommands default to their code-provided capability.
    /// Unknown aliases (e.g. `lint`) default to denied. Entries in this table
    /// override either default with `targets = true` or `targets = false`.
    #[serde(default)]
    pub subcommands: BTreeMap<String, CommandTargetCapability>,
}

/// Target-specific workspace override.
///
/// Keyed by Cargo-style cfg expressions, e.g. `cfg(target_arch = "wasm32")`.
/// Workspace target overrides may patch `exclude_packages` only.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct WorkspaceTargetOverride {
    /// Patch operations for [`WorkspaceConfig::exclude_packages`].
    #[serde(default)]
    pub exclude_packages: Option<StringSetPatch>,
}

/// Workspace-level configured-target policy for a single command token.
///
/// A plain `bool` is enough in v1. Unknown aliases default to deny, while
/// built-ins default according to cargo-fc's registry. A present
/// `targets = false` entry is therefore an explicit command-level opt-out.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct CommandTargetCapability {
    /// When `true`, cargo-fc may expand configured target lists and inject
    /// `--target <triple>` for this command.
    #[serde(default)]
    pub targets: bool,
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
