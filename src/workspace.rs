//! Workspace-level configuration and package discovery.

use crate::config::{Config, WorkspaceConfig};
use crate::print_warning;
use crate::{
    DEFAULT_METADATA_KEY, METADATA_KEYS, find_metadata_value, pkg_metadata_section,
    ws_metadata_section,
};
use color_eyre::eyre;
use std::collections::HashSet;

/// Workspace-only metadata keys read solely from the workspace root.
const WORKSPACE_ONLY_KEYS: &[&str] = &[
    "exclude_packages",
    "targets",
    "install_missing_targets",
    "target",
    "subcommands",
    "driver",
];

/// Abstraction over a Cargo workspace used by this crate.
pub trait Workspace {
    /// Return the workspace configuration section for feature combinations.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace metadata configuration can not be
    /// deserialized.
    fn workspace_config(&self) -> eyre::Result<WorkspaceConfig>;

    /// Return the candidate packages for feature combinations **without**
    /// applying workspace package exclusions.
    ///
    /// This emits the deprecation and no-op warnings for misplaced workspace
    /// metadata once. Workspace `exclude_packages` (and its target-specific
    /// patches) are applied later, per target, by the planner.
    ///
    /// # Errors
    ///
    /// Returns an error if metadata can not be parsed.
    fn candidate_packages_for_fc(&self) -> eyre::Result<Vec<&cargo_metadata::Package>>;

    /// Return the base, target-independent workspace exclude set.
    ///
    /// This is the union of `[workspace.metadata.*].exclude_packages` and the
    /// deprecated root-package `exclude_packages`. Target-specific workspace
    /// overrides patch this set per target during planning. This method emits
    /// no warnings.
    ///
    /// # Errors
    ///
    /// Returns an error if workspace metadata can not be parsed.
    fn base_workspace_exclude_packages(&self) -> eyre::Result<HashSet<String>>;

    /// Return the packages that should be considered for feature combinations.
    ///
    /// This is the backward-compatible single path: candidate discovery plus
    /// the base (target-independent) workspace exclude set.
    ///
    /// # Errors
    ///
    /// Returns an error if per-package configuration can not be parsed.
    fn packages_for_fc(&self) -> eyre::Result<Vec<&cargo_metadata::Package>>;
}

impl Workspace for cargo_metadata::Metadata {
    fn workspace_config(&self) -> eyre::Result<WorkspaceConfig> {
        let config: WorkspaceConfig = match find_metadata_value(&self.workspace_metadata) {
            Some((value, _key)) => serde_json::from_value(value.clone())?,
            None => WorkspaceConfig::default(),
        };
        Ok(config)
    }

    fn candidate_packages_for_fc(&self) -> eyre::Result<Vec<&cargo_metadata::Package>> {
        warn_workspace_metadata_misuse(self);
        Ok(self.workspace_packages())
    }

    fn base_workspace_exclude_packages(&self) -> eyre::Result<HashSet<String>> {
        let mut exclude = self.workspace_config()?.exclude_packages;

        // Fold in the deprecated root-package exclude_packages without emitting
        // warnings here (warnings are emitted once in candidate discovery).
        if let Some(root_package) = self.root_package()
            && let Some((value, _key)) = find_metadata_value(&root_package.metadata)
            && let Ok(config) = serde_json::from_value::<Config>(value.clone())
        {
            exclude.extend(config.exclude_packages);
        }

        Ok(exclude)
    }

    fn packages_for_fc(&self) -> eyre::Result<Vec<&cargo_metadata::Package>> {
        let mut packages = self.candidate_packages_for_fc()?;
        let exclude = self.base_workspace_exclude_packages()?;
        packages.retain(|p| !exclude.contains(p.name.as_str()));
        Ok(packages)
    }
}

/// Emit deprecation and no-op warnings for misplaced workspace metadata.
///
/// Warnings are intentionally side effects of candidate discovery so they fire
/// once per invocation regardless of how many targets are later planned.
fn warn_workspace_metadata_misuse(metadata: &cargo_metadata::Metadata) {
    let Some(root_package) = metadata.root_package() else {
        return;
    };

    let root_key =
        find_metadata_value(&root_package.metadata).map_or(DEFAULT_METADATA_KEY, |(_, key)| key);

    // Root-package exclude_packages is deprecated in favor of workspace metadata.
    if let Some((value, _key)) = find_metadata_value(&root_package.metadata)
        && let Ok(config) = serde_json::from_value::<Config>(value.clone())
        && !config.exclude_packages.is_empty()
    {
        print_warning!(
            "[{}].exclude_packages in the workspace root package is deprecated; use [{}].exclude_packages instead",
            pkg_metadata_section(root_key),
            ws_metadata_section(root_key),
        );
    }

    let root_id = &root_package.id;
    for package in &metadata.packages {
        if &package.id == root_id {
            continue;
        }

        // [package.metadata.<alias>].exclude_packages in a non-root member is a no-op.
        if let Some((raw, key)) = find_metadata_value(&package.metadata)
            && let Ok(config) = serde_json::from_value::<Config>(raw.clone())
            && !config.exclude_packages.is_empty()
        {
            print_warning!(
                "[{}].exclude_packages in package `{}` has no effect; this field is only read from the workspace root Cargo.toml",
                pkg_metadata_section(key),
                package.name,
            );
        }

        // [workspace.metadata.<alias>].<key> specified in non-root manifests is
        // also a no-op. Detect the JSON shape produced by cargo metadata and
        // warn for any workspace-only key that carries values.
        if let Some(workspace) = package.metadata.get("workspace")
            && let Some((key, tool)) = METADATA_KEYS
                .iter()
                .find_map(|&key| workspace.get(key).map(|tool| (key, tool)))
        {
            for ws_key in WORKSPACE_ONLY_KEYS {
                if json_has_values(tool.get(*ws_key)) {
                    print_warning!(
                        "[{}].{} in package `{}` has no effect; workspace metadata is only read from the workspace root Cargo.toml",
                        ws_metadata_section(key),
                        ws_key,
                        package.name,
                    );
                }
            }
        }
    }
}

/// Whether a JSON value carries meaningful (non-empty) configuration.
fn json_has_values(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Array(values)) => !values.is_empty(),
        Some(serde_json::Value::Object(map)) => !map.is_empty(),
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::Null) | None => false,
        Some(_) => true,
    }
}

#[cfg(test)]
mod test {
    use super::{Workspace, json_has_values};
    use color_eyre::eyre;
    use serde_json::json;

    static INIT: std::sync::Once = std::sync::Once::new();

    fn init() {
        INIT.call_once(|| {
            color_eyre::install().ok();
        });
    }

    #[test]
    fn workspace_with_package() -> eyre::Result<()> {
        init();

        let package = crate::package::test::package_with_features(&[])?;
        let metadata = workspace_builder()
            .packages(vec![package.clone()])
            .workspace_members(vec![package.id.clone()])
            .build()?;

        let have = metadata.packages_for_fc()?;
        similar_asserts::assert_eq!(have: have, want: vec![&package]);
        Ok(())
    }

    #[test]
    fn workspace_with_excluded_package() -> eyre::Result<()> {
        init();

        let package = crate::package::test::package_with_features(&[])?;
        let metadata = workspace_builder()
            .packages(vec![package.clone()])
            .workspace_members(vec![package.id.clone()])
            .workspace_metadata(json!({
                "cargo-feature-combinations": {
                    "exclude_packages": [package.name]
                }
            }))
            .build()?;

        let have = metadata.packages_for_fc()?;
        assert!(have.is_empty(), "expected no packages after exclusion");
        Ok(())
    }

    #[test]
    fn workspace_with_excluded_package_cargo_fc_alias() -> eyre::Result<()> {
        init();

        let package = crate::package::test::package_with_features(&[])?;
        let metadata = workspace_builder()
            .packages(vec![package.clone()])
            .workspace_members(vec![package.id.clone()])
            .workspace_metadata(json!({
                "cargo-fc": {
                    "exclude_packages": [package.name]
                }
            }))
            .build()?;

        let have = metadata.packages_for_fc()?;
        assert!(
            have.is_empty(),
            "expected no packages after exclusion via cargo-fc alias"
        );
        Ok(())
    }

    #[test]
    fn workspace_with_excluded_package_fc_alias() -> eyre::Result<()> {
        init();

        let package = crate::package::test::package_with_features(&[])?;
        let metadata = workspace_builder()
            .packages(vec![package.clone()])
            .workspace_members(vec![package.id.clone()])
            .workspace_metadata(json!({
                "fc": {
                    "exclude_packages": [package.name]
                }
            }))
            .build()?;

        let have = metadata.packages_for_fc()?;
        assert!(
            have.is_empty(),
            "expected no packages after exclusion via fc alias"
        );
        Ok(())
    }

    #[test]
    fn workspace_with_excluded_package_feature_combinations_alias() -> eyre::Result<()> {
        init();

        let package = crate::package::test::package_with_features(&[])?;
        let metadata = workspace_builder()
            .packages(vec![package.clone()])
            .workspace_members(vec![package.id.clone()])
            .workspace_metadata(json!({
                "feature-combinations": {
                    "exclude_packages": [package.name]
                }
            }))
            .build()?;

        let have = metadata.packages_for_fc()?;
        assert!(
            have.is_empty(),
            "expected no packages after exclusion via feature-combinations alias"
        );
        Ok(())
    }

    #[test]
    fn workspace_config_reads_install_missing_targets() -> eyre::Result<()> {
        init();

        let package = crate::package::test::package_with_features(&[])?;
        let metadata = workspace_builder()
            .packages(vec![package.clone()])
            .workspace_members(vec![package.id.clone()])
            .workspace_metadata(json!({
                "cargo-fc": {
                    "install_missing_targets": true
                }
            }))
            .build()?;

        let config = metadata.workspace_config()?;

        assert!(config.install_missing_targets);
        Ok(())
    }

    #[test]
    fn json_has_values_treats_false_as_default_empty_value() {
        assert!(!json_has_values(Some(&json!(false))));
        assert!(json_has_values(Some(&json!(true))));
    }

    fn workspace_builder() -> cargo_metadata::MetadataBuilder {
        use cargo_metadata::{MetadataBuilder, WorkspaceDefaultMembers};

        MetadataBuilder::default()
            .version(1u8)
            .workspace_default_members(WorkspaceDefaultMembers::default())
            .resolve(None)
            .workspace_root("")
            .workspace_metadata(json!({}))
            .build_directory(None)
            .target_directory("")
    }
}
