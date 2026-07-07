mod flags;
/// Patch types for set-like configuration fields.
pub mod patch;
/// Configuration resolution logic (merge base config with target overrides).
pub(crate) mod resolve;
mod schema;
pub(crate) mod scope;
mod validate;

pub(crate) use flags::{FLAG_KEYS, combine_bool, combine_driver, combine_flag_configs};
pub use flags::{FlagConfig, ResolvedFlags};
pub use resolve::ResolvedFeatures;
pub use schema::{
    CommandCapabilities, Config, FeatureMatrixPatch, RootConfig, ScopeConfig, SectionConfig,
    TargetOverride, WorkspaceConfig, WorkspaceTargetOverride,
};
pub(crate) use scope::Chain;
pub(crate) use validate::{validate_package_metadata, validate_workspace_metadata};
