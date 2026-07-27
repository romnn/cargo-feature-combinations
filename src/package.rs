//! Package-level configuration discovery and the [`Package`] extension trait.

mod combinations;

pub use combinations::FeatureCombinationError;

use crate::config::patch::{FeatureSetVecPatch, StringSetPatch};
use crate::config::{Config, ResolvedFeatures, validate_package_metadata};
use crate::print_warning;
use crate::{DEFAULT_METADATA_KEY, find_metadata_value, pkg_metadata_section};
use color_eyre::eyre::{self, WrapErr};
use itertools::Itertools;
use std::collections::HashSet;

#[must_use]
pub(crate) fn has_lib_target(package: &cargo_metadata::Package) -> bool {
    package
        .targets
        .iter()
        .any(|target| target.kind.contains(&cargo_metadata::TargetKind::Lib))
}

/// Extension trait for [`cargo_metadata::Package`] used by this crate.
pub trait Package {
    /// Parse the configuration for this package if present.
    ///
    /// If the Cargo.toml manifest contains a configuration section,
    /// the latter is parsed.
    /// Otherwise, a default configuration is used.
    ///
    /// # Errors
    ///
    /// If the configuration in the manifest can not be parsed,
    /// an error is returned.
    ///
    fn config(&self) -> eyre::Result<Config>;
    /// Compute all feature combinations for this package based on the
    /// provided [`ResolvedFeatures`].
    ///
    /// # Errors
    ///
    /// Returns an error if the package declares too many features or its
    /// mutually exclusive feature groups are invalid.
    fn feature_combinations<'a>(
        &'a self,
        config: &ResolvedFeatures,
    ) -> eyre::Result<Vec<Vec<&'a String>>>;
    /// Convert [`Package::feature_combinations`] into a list of comma-separated
    /// feature strings suitable for passing to `cargo --features`.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Package::feature_combinations`] fails.
    fn feature_matrix(&self, config: &ResolvedFeatures) -> eyre::Result<Vec<String>>;
}

impl Package for cargo_metadata::Package {
    fn config(&self) -> eyre::Result<Config> {
        let (mut config, key) = match find_metadata_value(&self.metadata) {
            Some((value, key)) => {
                validate_package_metadata(value, &pkg_metadata_section(key))?;
                (
                    serde_json::from_value(value.clone()).wrap_err_with(|| {
                        format!(
                            "invalid [{}] configuration in package `{}`",
                            pkg_metadata_section(key),
                            self.name
                        )
                    })?,
                    key,
                )
            }
            None => (Config::default(), DEFAULT_METADATA_KEY),
        };

        let section = pkg_metadata_section(key);

        if !config.deprecated.skip_feature_sets.is_empty() {
            print_warning!(
                "[{section}].skip_feature_sets in package `{}` is deprecated; use exclude_feature_sets instead",
                self.name,
            );
        }

        if !config.deprecated.denylist.is_empty() {
            print_warning!(
                "[{section}].denylist in package `{}` is deprecated; use exclude_features instead",
                self.name,
            );
        }

        if !config.deprecated.exact_combinations.is_empty() {
            print_warning!(
                "[{section}].exact_combinations in package `{}` is deprecated; use include_feature_sets instead",
                self.name,
            );
        }

        fold_deprecated_feature_sets(
            &mut config.base.settings.features.exclude_feature_sets,
            std::mem::take(&mut config.deprecated.skip_feature_sets),
        );
        fold_deprecated_string_set(
            &mut config.base.settings.features.exclude_features,
            std::mem::take(&mut config.deprecated.denylist),
        );
        fold_deprecated_feature_sets(
            &mut config.base.settings.features.include_feature_sets,
            std::mem::take(&mut config.deprecated.exact_combinations),
        );

        // After folding, so names from deprecated spellings are checked too.
        crate::config::validate_feature_names(&config, &self.features, &self.name, &section)?;

        Ok(config)
    }

    fn feature_combinations<'a>(
        &'a self,
        config: &ResolvedFeatures,
    ) -> eyre::Result<Vec<Vec<&'a String>>> {
        combinations::feature_combinations(self, config)
    }

    fn feature_matrix(&self, config: &ResolvedFeatures) -> eyre::Result<Vec<String>> {
        Ok(self
            .feature_combinations(config)?
            .into_iter()
            .map(|features| features.iter().join(","))
            .collect())
    }
}

fn fold_deprecated_string_set(target: &mut Option<StringSetPatch>, values: HashSet<String>) {
    if values.is_empty() {
        return;
    }
    match target {
        Some(
            StringSetPatch::Override(current)
            | StringSetPatch::Patch {
                r#override: Some(current),
                ..
            },
        ) => current.extend(values),
        Some(StringSetPatch::Patch { add, .. }) => add.extend(values),
        None => {
            *target = Some(StringSetPatch::Patch {
                r#override: None,
                add: values,
                remove: HashSet::new(),
            });
        }
    }
}

fn fold_deprecated_feature_sets(
    target: &mut Option<FeatureSetVecPatch>,
    mut values: Vec<HashSet<String>>,
) {
    if values.is_empty() {
        return;
    }
    match target {
        Some(
            FeatureSetVecPatch::Override(current)
            | FeatureSetVecPatch::Patch {
                r#override: Some(current),
                ..
            },
        ) => current.append(&mut values),
        Some(FeatureSetVecPatch::Patch { add, .. }) => add.append(&mut values),
        None => {
            *target = Some(FeatureSetVecPatch::Patch {
                r#override: None,
                add: values,
                remove: Vec::new(),
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::Package;
    use crate::config::ResolvedFeatures;
    use color_eyre::eyre;

    static INIT: std::sync::Once = std::sync::Once::new();

    pub(crate) fn init() {
        INIT.call_once(|| {
            color_eyre::install().ok();
        });
    }

    pub(crate) fn package(name: &str) -> eyre::Result<cargo_metadata::Package> {
        package_with_manifest_path(name, "")
    }

    pub(crate) fn package_with_manifest_path(
        name: &str,
        manifest_path: &str,
    ) -> eyre::Result<cargo_metadata::Package> {
        use cargo_metadata::{PackageBuilder, PackageId, PackageName};
        use semver::Version;
        use std::str::FromStr as _;

        Ok(PackageBuilder::new(
            PackageName::from_str(name)?,
            Version::parse("0.1.0")?,
            PackageId {
                repr: name.to_string(),
            },
            manifest_path,
        )
        .build()?)
    }

    pub(crate) fn effective_target(triple: &str) -> crate::target::EffectiveTarget {
        crate::target::EffectiveTarget {
            triple: crate::target::TargetTriple(triple.to_string()),
            source: crate::target::TargetSource::WorkspaceConfig,
        }
    }

    pub(crate) fn package_with_features(
        features: &[&str],
    ) -> eyre::Result<cargo_metadata::Package> {
        let mut package = package("test")?;
        package.features = features
            .iter()
            .map(|feature| ((*feature).to_string(), vec![]))
            .collect();
        Ok(package)
    }

    /// Build a package whose metadata contains a config under the given alias.
    pub(crate) fn package_with_metadata(
        features: &[&str],
        metadata_key: &str,
        config: &serde_json::Value,
    ) -> eyre::Result<cargo_metadata::Package> {
        let mut package = package_with_features(features)?;
        package.metadata = serde_json::json!({ metadata_key: config });
        Ok(package)
    }

    #[test]
    fn config_from_cargo_fc_alias() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["foo", "bar"],
            "cargo-fc",
            &serde_json::json!({ "exclude_features": ["foo"] }),
        )?;
        let config = package.config()?;
        let resolved = ResolvedFeatures::from_config(&config);
        assert!(resolved.exclude_features.contains("foo"));
        assert!(!resolved.exclude_features.contains("bar"));
        Ok(())
    }

    #[test]
    fn config_from_fc_alias() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["foo", "bar"],
            "fc",
            &serde_json::json!({ "exclude_features": ["bar"] }),
        )?;
        let config = package.config()?;
        let resolved = ResolvedFeatures::from_config(&config);
        assert!(resolved.exclude_features.contains("bar"));
        assert!(!resolved.exclude_features.contains("foo"));
        Ok(())
    }

    #[test]
    fn config_from_feature_combinations_alias() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["a", "b"],
            "feature-combinations",
            &serde_json::json!({ "no_empty_feature_set": true }),
        )?;
        let config = package.config()?;
        assert!(ResolvedFeatures::from_config(&config).no_empty_feature_set);
        Ok(())
    }

    #[test]
    fn config_from_cargo_feature_combinations_alias() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["a", "b"],
            "cargo-feature-combinations",
            &serde_json::json!({ "exclude_features": ["a"] }),
        )?;
        let config = package.config()?;
        assert!(
            ResolvedFeatures::from_config(&config)
                .exclude_features
                .contains("a")
        );
        Ok(())
    }

    #[test]
    fn config_rejects_unknown_feature_names() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["foo"],
            "cargo-fc",
            &serde_json::json!({ "exclude_features": ["bar-typo"] }),
        )?;

        let err = package
            .config()
            .expect_err("unknown feature names should fail config load");

        assert!(err.to_string().contains("bar-typo"), "{err}");
        Ok(())
    }

    #[test]
    fn config_rejects_unknown_feature_in_unmatched_target_section() -> eyre::Result<()> {
        init();
        // The feature list does not depend on the resolution target, so even a
        // section whose cfg never matches this host must reference real names.
        let package = package_with_metadata(
            &["foo"],
            "cargo-fc",
            &serde_json::json!({
                "target": {
                    "cfg(target_os = \"none\")": {
                        "mutually_exclusive_features": [["foo", "ghost"]],
                    },
                },
            }),
        )?;

        let err = package
            .config()
            .expect_err("unknown names in any target section should fail config load");

        assert!(err.to_string().contains("ghost"), "{err}");
        Ok(())
    }

    #[test]
    fn config_rejects_undeclared_default() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["foo"],
            "cargo-fc",
            &serde_json::json!({ "exclude_features": ["default"] }),
        )?;

        let err = package
            .config()
            .expect_err("excluding an undeclared default feature should fail config load");

        assert!(err.to_string().contains("`default`"), "{err}");
        Ok(())
    }

    #[test]
    fn config_validates_names_from_deprecated_keys() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["foo"],
            "cargo-fc",
            &serde_json::json!({ "denylist": ["typo"] }),
        )?;

        let err = package
            .config()
            .expect_err("deprecated spellings fold into checked keys");

        assert!(err.to_string().contains("typo"), "{err}");
        Ok(())
    }

    #[test]
    fn config_default_when_no_metadata() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo"])?;
        let config = package.config()?;
        let resolved = ResolvedFeatures::from_config(&config);
        assert!(resolved.exclude_features.is_empty());
        assert!(!resolved.no_empty_feature_set);
        Ok(())
    }

    #[test]
    fn config_alias_affects_feature_matrix() -> eyre::Result<()> {
        init();
        let package = package_with_metadata(
            &["foo", "bar"],
            "cargo-fc",
            &serde_json::json!({ "exclude_features": ["foo"] }),
        )?;
        let config = package.config()?;
        let matrix = package.feature_combinations(&ResolvedFeatures::from_config(&config))?;

        // "foo" is excluded, so no combination should contain it
        assert!(
            !matrix.iter().any(|combo| combo.iter().any(|f| *f == "foo")),
            "expected no combination to contain 'foo', got: {matrix:?}"
        );
        // "bar" should still appear
        assert!(
            matrix.iter().any(|combo| combo.iter().any(|f| *f == "bar")),
            "expected 'bar' in at least one combination, got: {matrix:?}"
        );
        Ok(())
    }
}
