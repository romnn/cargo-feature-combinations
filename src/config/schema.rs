use super::flags::FlagConfig;
use super::patch::{FeatureSetVecPatch, StringSetPatch};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

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
    #[serde(default, rename = "targets")]
    pub package_targets: Option<Vec<String>>,
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
    /// By default this is `false` to preserve the existing feature matrix.
    #[serde(default)]
    pub skip_optional_dependencies: bool,
    /// Deprecated TOML key. Prefer
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
    /// Arbitrary user-defined matrix values forwarded to the runner.
    #[serde(default)]
    pub matrix: serde_json::Map<String, serde_json::Value>,
    /// Package-level cargo-fc flag defaults.
    #[serde(default, flatten)]
    pub flags: FlagConfig,
    /// Per-subcommand package-level cargo-fc flag defaults.
    #[serde(default, rename = "subcommands")]
    pub subcommand_overrides: BTreeMap<String, CommandCapabilities>,

    /// Target-specific package configuration overrides.
    ///
    /// This is read from `[package.metadata.cargo-fc.target.'cfg(...)']`.
    #[serde(default, rename = "target")]
    pub target_overrides: BTreeMap<String, TargetOverride>,
    /// Deprecated TOML keys accepted during config parsing.
    #[serde(flatten)]
    pub(crate) deprecated: DeprecatedTomlKeys,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            package_targets: None,
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
            matrix: serde_json::Map::new(),
            flags: FlagConfig::default(),
            subcommand_overrides: BTreeMap::new(),
            target_overrides: BTreeMap::new(),
            deprecated: DeprecatedTomlKeys::default(),
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
    /// Merge override for [`Config::matrix`].
    #[serde(default)]
    pub matrix: Option<serde_json::Map<String, serde_json::Value>>,
    /// Target-specific cargo-fc flag defaults.
    #[serde(default, flatten)]
    pub flags: FlagConfig,
    /// Per-subcommand target-specific cargo-fc flag defaults.
    #[serde(default, rename = "subcommands")]
    pub subcommand_overrides: BTreeMap<String, CommandCapabilities>,
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
    #[serde(default, rename = "targets")]
    pub workspace_targets: Vec<String>,
    /// Target-specific workspace overrides keyed by Cargo-style cfg expressions.
    ///
    /// These select workspace packages and cargo-fc flags for one
    /// already-selected target.
    #[serde(default, rename = "target")]
    pub target_overrides: BTreeMap<String, WorkspaceTargetOverride>,
    /// Per-subcommand capability and flag overrides.
    ///
    /// Built-in Cargo subcommands default to their code-provided capabilities.
    /// Unresolved aliases and custom subcommands default to denied. Entries in
    /// this table override target capability and command-local cargo-fc flags.
    #[serde(default, rename = "subcommands")]
    pub subcommand_overrides: BTreeMap<String, CommandCapabilities>,
    /// Build driver to invoke in place of `cargo` for each combination.
    ///
    /// When unset, cargo-fc uses plain `cargo` for host-only runs and defaults
    /// to `cargo-zigbuild` when any non-host target is planned (so native-C
    /// dependencies cross-compile via zig). Set it to a wrapper such as
    /// `cargo-zigbuild`, `cross`, or back to `cargo` to force plain cargo. The
    /// `--driver` CLI flag overrides this.
    #[serde(default)]
    pub driver: Option<String>,
    /// Workspace cargo-fc flag defaults.
    #[serde(default, flatten)]
    pub flags: FlagConfig,
}

/// Target-specific workspace override.
///
/// Keyed by Cargo-style cfg expressions, e.g. `cfg(target_arch = "wasm32")`.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct WorkspaceTargetOverride {
    /// Patch operations for [`WorkspaceConfig::exclude_packages`].
    #[serde(default)]
    pub exclude_packages: Option<StringSetPatch>,
    /// Target-specific workspace cargo-fc flag defaults.
    #[serde(default, flatten)]
    pub flags: FlagConfig,
    /// Per-subcommand target-specific workspace cargo-fc flag defaults.
    #[serde(default, rename = "subcommands")]
    pub subcommand_overrides: BTreeMap<String, CommandCapabilities>,
}

/// Workspace-level capability and flag overrides for a single command token.
///
/// Unresolved aliases and custom subcommands default to deny capabilities,
/// while built-ins default according to cargo-fc's registry.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct CommandCapabilities {
    /// When `true`, cargo-fc may expand configured target lists and inject
    /// `--target <triple>` for this command.
    #[serde(default)]
    pub targets: Option<bool>,
    /// Per-command cargo-fc flag defaults.
    #[serde(default, flatten)]
    pub flags: FlagConfig,
}

/// Deprecated TOML keys kept as accepted input spellings.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub(crate) struct DeprecatedTomlKeys {
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
