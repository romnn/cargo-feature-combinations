//! Workspace-level configuration and package discovery.

use crate::config::{Config, WorkspaceConfig};
use crate::package::Package;
use crate::{METADATA_KEY, PKG_METADATA_SECTION, WS_METADATA_SECTION};
use color_eyre::eyre;

/// Abstraction over a Cargo workspace used by this crate.
pub trait Workspace {
    /// Return the workspace configuration section for feature combinations.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace metadata configuration can not be
    /// deserialized.
    fn workspace_config(&self) -> eyre::Result<WorkspaceConfig>;

    /// Return the packages that should be considered for feature combinations.
    ///
    /// # Errors
    ///
    /// Returns an error if per-package configuration can not be parsed.
    fn packages_for_fc(&self) -> eyre::Result<Vec<&cargo_metadata::Package>>;
}

impl Workspace for cargo_metadata::Metadata {
    fn workspace_config(&self) -> eyre::Result<WorkspaceConfig> {
        let config: WorkspaceConfig = match self.workspace_metadata.get(METADATA_KEY) {
            Some(config) => serde_json::from_value(config.clone())?,
            None => WorkspaceConfig::default(),
        };
        Ok(config)
    }

    fn packages_for_fc(&self) -> eyre::Result<Vec<&cargo_metadata::Package>> {
        let mut packages = self.workspace_packages();

        let workspace_config = self.workspace_config()?;

        // Determine the workspace root package (if any) and load its config so we can both
        // apply filtering and emit deprecation warnings for legacy configuration.
        let mut root_config: Option<Config> = None;
        let mut root_id: Option<cargo_metadata::PackageId> = None;

        if let Some(root_package) = self.root_package() {
            let config = root_package.config()?;

            if !config.exclude_packages.is_empty() {
                eprintln!(
                    "warning: {PKG_METADATA_SECTION}.exclude_packages in the workspace root package is deprecated; use {WS_METADATA_SECTION}.exclude_packages instead",
                );
            }

            root_id = Some(root_package.id.clone());
            root_config = Some(config);
        }

        // For non-root workspace members, using exclude_packages is a no-op. Emit warnings for
        // such configurations so users are aware that these fields are ignored.
        if root_id.is_some() {
            for package in &self.packages {
                if Some(&package.id) == root_id.as_ref() {
                    continue;
                }

                // [package.metadata.cargo-feature-combinations].exclude_packages
                if let Some(raw) = package.metadata.get(METADATA_KEY)
                    && let Ok(config) = serde_json::from_value::<Config>(raw.clone())
                    && !config.exclude_packages.is_empty()
                {
                    eprintln!(
                        "warning: {PKG_METADATA_SECTION}.exclude_packages in package `{}` has no effect; this field is only read from the workspace root Cargo.toml",
                        package.name,
                    );
                }

                // [workspace.metadata.cargo-feature-combinations].exclude_packages specified in
                // non-root manifests is also a no-op. Detect the likely JSON shape produced by
                // cargo metadata and warn if present.
                if let Some(workspace) = package.metadata.get("workspace")
                    && let Some(tool) = workspace.get(METADATA_KEY)
                    && let Some(exclude_packages) = tool.get("exclude_packages")
                {
                    let has_values = match exclude_packages {
                        serde_json::Value::Array(values) => !values.is_empty(),
                        serde_json::Value::Null => false,
                        _ => true,
                    };

                    if has_values {
                        eprintln!(
                            "warning: {WS_METADATA_SECTION}.exclude_packages in package `{}` has no effect; workspace metadata is only read from the workspace root Cargo.toml",
                            package.name,
                        );
                    }
                }
            }
        }

        // Filter packages based on workspace metadata configuration
        packages.retain(|p| !workspace_config.exclude_packages.contains(p.name.as_str()));

        if let Some(config) = root_config {
            // Filter packages based on root package Cargo.toml configuration
            packages.retain(|p| !config.exclude_packages.contains(p.name.as_str()));
        }

        Ok(packages)
    }
}

#[cfg(test)]
mod test {
    use super::Workspace;
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
