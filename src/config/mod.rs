mod flags;
/// Patch types for set-like configuration fields.
pub mod patch;
/// Configuration resolution logic (merge base config with target overrides).
pub(crate) mod resolve;
mod schema;
mod validate;

pub(crate) use flags::{
    FLAG_KEYS, ResolveCommandConfigArgs, ResolvedCommandConfig, combine_bool,
    combine_command_capability_maps, combine_driver, combine_flag_configs, resolve_command_config,
};
pub use flags::{FlagConfig, ResolvedFlags};
pub use schema::{
    CommandCapabilities, Config, FeatureMatrixPatch, TargetOverride, WorkspaceConfig,
    WorkspaceTargetOverride,
};
pub(crate) use validate::{validate_package_metadata, validate_workspace_metadata};
