//! Package-level configuration, feature combination generation, and error types.

use crate::METADATA_KEY;
use crate::config::Config;
use color_eyre::eyre;
use itertools::Itertools;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;

const MAX_FEATURE_COMBINATIONS: u128 = 100_000;

/// Errors that can occur while generating feature combinations.
#[derive(Debug)]
pub enum FeatureCombinationError {
    /// The package declares too many features, which would result in more
    /// combinations than this tool is willing to generate.
    TooManyConfigurations {
        /// Package name from Cargo metadata.
        package: String,
        /// Number of features considered for combination generation.
        num_features: usize,
        /// Total number of configurations implied by `num_features`, if bounded.
        num_configurations: Option<u128>,
        /// Maximum number of configurations allowed before failing.
        limit: u128,
    },
}

impl fmt::Display for FeatureCombinationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyConfigurations {
                package,
                num_features,
                num_configurations,
                limit,
            } => {
                write!(
                    f,
                    "too many configurations for package `{}`: {} feature(s) would produce {} combinations (limit: {})",
                    package,
                    num_features,
                    num_configurations
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "an unbounded number of".to_string()),
                    limit
                )
            }
        }
    }
}

impl std::error::Error for FeatureCombinationError {}

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
    /// provided [`Config`].
    ///
    /// # Errors
    ///
    /// Returns an error if feature combinations can not be computed, e.g. when
    /// the package declares too many features.
    fn feature_combinations<'a>(&'a self, config: &'a Config)
    -> eyre::Result<Vec<Vec<&'a String>>>;
    /// Convert [`Package::feature_combinations`] into a list of comma-separated
    /// feature strings suitable for passing to `cargo --features`.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Package::feature_combinations`] fails.
    fn feature_matrix(&self, config: &Config) -> eyre::Result<Vec<String>>;
}

impl Package for cargo_metadata::Package {
    fn config(&self) -> eyre::Result<Config> {
        let mut config: Config = match self.metadata.get(METADATA_KEY) {
            Some(config) => serde_json::from_value(config.clone())?,
            None => Config::default(),
        };

        if !config.deprecated.skip_feature_sets.is_empty() {
            eprintln!(
                "warning: [package.metadata.cargo-feature-combinations].skip_feature_sets in package `{}` is deprecated; use exclude_feature_sets instead",
                self.name,
            );
        }

        if !config.deprecated.denylist.is_empty() {
            eprintln!(
                "warning: [package.metadata.cargo-feature-combinations].denylist in package `{}` is deprecated; use exclude_features instead",
                self.name,
            );
        }

        if !config.deprecated.exact_combinations.is_empty() {
            eprintln!(
                "warning: [package.metadata.cargo-feature-combinations].exact_combinations in package `{}` is deprecated; use include_feature_sets instead",
                self.name,
            );
        }

        // Handle deprecated config values
        config
            .exclude_feature_sets
            .append(&mut config.deprecated.skip_feature_sets);
        config
            .exclude_features
            .extend(config.deprecated.denylist.drain());
        config
            .include_feature_sets
            .append(&mut config.deprecated.exact_combinations);

        Ok(config)
    }

    fn feature_combinations<'a>(
        &'a self,
        config: &'a Config,
    ) -> eyre::Result<Vec<Vec<&'a String>>> {
        // Short-circuit: if an explicit allowlist of feature sets is configured,
        // interpret it as the complete matrix.
        //
        // This is intentionally *not* combined with the normal powerset-based
        // generation and its filters: the user is declaring the exact sets they
        // care about (e.g. SSR vs hydrate), and we should not implicitly add
        // `[]` or any other combinations.
        if !config.allow_feature_sets.is_empty() {
            let mut allowed = config
                .allow_feature_sets
                .iter()
                .map(|proposed_allowed_set| {
                    // Normalize to this package by dropping unknown feature
                    // names and switching to references into `self.features`.
                    proposed_allowed_set
                        .iter()
                        .filter_map(|maybe_feature| {
                            self.features.get_key_value(maybe_feature).map(|(k, _v)| k)
                        })
                        .collect::<BTreeSet<_>>()
                })
                .collect::<BTreeSet<_>>();

            if config.no_empty_feature_set {
                // In exact-matrix mode, `[]` is only included if explicitly
                // listed. This option makes it easy to forbid `[]` entirely.
                allowed.retain(|set| !set.is_empty());
            }

            return Ok(allowed
                .into_iter()
                .map(|set| set.into_iter().sorted().collect::<Vec<_>>())
                .sorted()
                .collect::<Vec<_>>());
        }

        // Derive the effective exclude set for this package.
        //
        // When `skip_optional_dependencies` is enabled, extend the configured
        // `exclude_features` with implicit features that correspond to optional
        // dependencies for this package.
        //
        // This mirrors the behaviour in `cargo-all-features`: only the
        // *implicit* features generated by Cargo for optional dependencies are
        // skipped, i.e. features of the form
        //
        //   foo = ["dep:foo"]
        //
        // that are not also referenced via `dep:foo` in any other feature.
        let mut effective_exclude_features = config.exclude_features.clone();

        if config.skip_optional_dependencies {
            let mut implicit_features: HashSet<String> = HashSet::new();
            let mut optional_dep_used_with_dep_syntax_outside: HashSet<String> = HashSet::new();

            // Classify implicit optional-dependency features and track optional
            // dependencies that are referenced via `dep:NAME` in other
            // features, following the logic from cargo-all-features'
            // features_finder.rs.
            for (feature_name, implied) in &self.features {
                for value in implied.iter().filter(|v| v.starts_with("dep:")) {
                    let dep_name = value.trim_start_matches("dep:");
                    if implied.len() == 1 && dep_name == feature_name {
                        // Feature of the shape `foo = ["dep:foo"]`.
                        implicit_features.insert(feature_name.clone());
                    } else {
                        // The dep is used with `dep:` syntax in another
                        // feature, so Cargo will not generate an implicit
                        // feature for it.
                        optional_dep_used_with_dep_syntax_outside.insert(dep_name.to_string());
                    }
                }
            }

            // If the dep is used with `dep:` syntax in another feature, it is
            // not an implicit feature and should not be skipped purely because
            // it is an optional dependency.
            for dep_name in &optional_dep_used_with_dep_syntax_outside {
                implicit_features.remove(dep_name);
            }

            // Extend the effective exclude list with the remaining implicit
            // optional-dependency features.
            effective_exclude_features.extend(implicit_features);
        }

        // Generate the base powerset from
        // - all features
        // - or from isolated sets, minus excluded features
        let base_powerset = if config.isolated_feature_sets.is_empty() {
            generate_global_base_powerset(
                &self.name,
                &self.features,
                &effective_exclude_features,
                &config.include_features,
                &config.only_features,
            )?
        } else {
            generate_isolated_base_powerset(
                &self.name,
                &self.features,
                &config.isolated_feature_sets,
                &effective_exclude_features,
                &config.include_features,
                &config.only_features,
            )?
        };

        // Filter out feature sets that contain skip sets
        let mut filtered_powerset = base_powerset
            .into_iter()
            .filter(|feature_set| {
                !config.exclude_feature_sets.iter().any(|skip_set| {
                    if skip_set.is_empty() {
                        // Special-case: an empty skip set means "exclude only the empty
                        // feature set".
                        //
                        // Without this, the usual "all()" subset test would treat an empty
                        // set as contained in every feature set (vacuously true), and thus
                        // exclude *everything*.
                        feature_set.is_empty()
                    } else {
                        // Remove feature sets containing any of the skip sets
                        skip_set
                            .iter()
                            // Skip set is contained when all its features are contained
                            .all(|skip_feature| feature_set.contains(skip_feature))
                    }
                })
            })
            .collect::<BTreeSet<_>>();

        // Add back exact combinations
        for proposed_exact_combination in &config.include_feature_sets {
            // Remove non-existent features and switch reference to that pointing to `self`
            let exact_combination = proposed_exact_combination
                .iter()
                .filter_map(|maybe_feature| {
                    self.features.get_key_value(maybe_feature).map(|(k, _v)| k)
                })
                .collect::<BTreeSet<_>>();

            // This exact combination may now be empty, but empty combination is always added anyway
            filtered_powerset.insert(exact_combination);
        }

        if config.no_empty_feature_set {
            // When enabled, drop the empty feature set (`[]`) from the final matrix.
            filtered_powerset.retain(|set| !set.is_empty());
        }

        // Re-collect everything into a vector of vectors
        Ok(filtered_powerset
            .into_iter()
            .map(|set| set.into_iter().sorted().collect::<Vec<_>>())
            .sorted()
            .collect::<Vec<_>>())
    }

    fn feature_matrix(&self, config: &Config) -> eyre::Result<Vec<String>> {
        Ok(self
            .feature_combinations(config)?
            .into_iter()
            .map(|features| features.iter().join(","))
            .collect())
    }
}

fn checked_num_combinations(num_features: usize) -> Option<u128> {
    if num_features >= u128::BITS as usize {
        return None;
    }
    let shift: u32 = num_features.try_into().ok()?;
    Some(1u128 << shift)
}

fn ensure_within_combination_limit(
    package_name: &str,
    num_features: usize,
) -> Result<(), FeatureCombinationError> {
    let num_configurations = checked_num_combinations(num_features);
    let exceeds = match num_configurations {
        Some(n) => n > MAX_FEATURE_COMBINATIONS,
        None => true,
    };

    if exceeds {
        return Err(FeatureCombinationError::TooManyConfigurations {
            package: package_name.to_string(),
            num_features,
            num_configurations,
            limit: MAX_FEATURE_COMBINATIONS,
        });
    }

    Ok(())
}

/// Generates the **global** base [powerset](Itertools::powerset) of features.
/// Global features are all features that are defined in the package, except the
/// features from the provided denylist.
///
/// The returned powerset is a two-level [`BTreeSet`], with the strings pointing
/// back to the `package_features`.
fn generate_global_base_powerset<'a>(
    package_name: &str,
    package_features: &'a BTreeMap<String, Vec<String>>,
    exclude_features: &HashSet<String>,
    include_features: &'a HashSet<String>,
    only_features: &HashSet<String>,
) -> Result<BTreeSet<BTreeSet<&'a String>>, FeatureCombinationError> {
    let features = package_features
        .keys()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|ft| !exclude_features.contains(*ft))
        .filter(|ft| only_features.is_empty() || only_features.contains(*ft))
        .collect::<BTreeSet<_>>();

    ensure_within_combination_limit(package_name, features.len())?;

    Ok(features
        .into_iter()
        .powerset()
        .map(|combination| {
            combination
                .into_iter()
                .chain(include_features)
                .collect::<BTreeSet<&'a String>>()
        })
        .collect())
}

/// Generates the **isolated** base [powerset](Itertools::powerset) of features.
/// Isolated features are features from the provided isolated feature sets,
/// except non-existent features and except the features from the provided
/// denylist.
///
/// The returned powerset is a two-level [`BTreeSet`], with the strings pointing
/// back to the `package_features`.
fn generate_isolated_base_powerset<'a>(
    package_name: &str,
    package_features: &'a BTreeMap<String, Vec<String>>,
    isolated_feature_sets: &[HashSet<String>],
    exclude_features: &HashSet<String>,
    include_features: &'a HashSet<String>,
    only_features: &HashSet<String>,
) -> Result<BTreeSet<BTreeSet<&'a String>>, FeatureCombinationError> {
    // Collect known package features for easy querying
    let known_features = package_features.keys().collect::<HashSet<_>>();

    let mut worst_case_total: u128 = 0;
    for isolated_feature_set in isolated_feature_sets {
        let num_features = isolated_feature_set
            .iter()
            .filter(|ft| known_features.contains(*ft))
            .filter(|ft| !exclude_features.contains(*ft))
            .filter(|ft| only_features.is_empty() || only_features.contains(*ft))
            .count();

        let Some(n) = checked_num_combinations(num_features) else {
            return Err(FeatureCombinationError::TooManyConfigurations {
                package: package_name.to_string(),
                num_features,
                num_configurations: None,
                limit: MAX_FEATURE_COMBINATIONS,
            });
        };

        worst_case_total = worst_case_total.saturating_add(n);
        if worst_case_total > MAX_FEATURE_COMBINATIONS {
            return Err(FeatureCombinationError::TooManyConfigurations {
                package: package_name.to_string(),
                num_features,
                num_configurations: Some(worst_case_total),
                limit: MAX_FEATURE_COMBINATIONS,
            });
        }
    }

    Ok(isolated_feature_sets
        .iter()
        .flat_map(|isolated_feature_set| {
            isolated_feature_set
                .iter()
                .filter(|ft| known_features.contains(*ft)) // remove non-existent features
                .filter(|ft| !exclude_features.contains(*ft)) // remove features from denylist
                .filter(|ft| only_features.is_empty() || only_features.contains(*ft))
                .powerset()
                .map(|combination| {
                    combination
                        .into_iter()
                        .filter_map(|feature| known_features.get(feature).copied())
                        .chain(include_features)
                        .collect::<BTreeSet<_>>()
                })
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod test {
    use super::{FeatureCombinationError, Package};
    use crate::config::Config;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::collections::HashSet;

    static INIT: std::sync::Once = std::sync::Once::new();

    fn init() {
        INIT.call_once(|| {
            color_eyre::install().ok();
        });
    }

    pub(crate) fn package_with_features(
        features: &[&str],
    ) -> eyre::Result<cargo_metadata::Package> {
        use cargo_metadata::{PackageBuilder, PackageId, PackageName};
        use semver::Version;
        use std::str::FromStr as _;

        let mut package = PackageBuilder::new(
            PackageName::from_str("test")?,
            Version::parse("0.1.0")?,
            PackageId {
                repr: "test".to_string(),
            },
            "",
        )
        .build()?;
        package.features = features
            .iter()
            .map(|feature| ((*feature).to_string(), vec![]))
            .collect();
        Ok(package)
    }

    #[test]
    fn combinations() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo-c", "foo-a", "foo-b"])?;
        let config = Config::default();
        let want = vec![
            vec![],
            vec!["foo-a"],
            vec!["foo-a", "foo-b"],
            vec!["foo-a", "foo-b", "foo-c"],
            vec!["foo-a", "foo-c"],
            vec!["foo-b"],
            vec!["foo-b", "foo-c"],
            vec!["foo-c"],
        ];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_only_features() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo", "bar", "baz"])?;
        let config = Config {
            exclude_features: HashSet::from(["default".to_string()]),
            only_features: HashSet::from(["foo".to_string(), "bar".to_string()]),
            ..Default::default()
        };

        let want = vec![vec![], vec!["bar"], vec!["bar", "foo"], vec!["foo"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_isolated() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-b", "bar-a", "car-b", "car-a"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "foo-b".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            ..Default::default()
        };
        let want = vec![
            vec![],
            vec!["bar-a"],
            vec!["bar-a", "bar-b"],
            vec!["bar-b"],
            vec!["foo-a"],
            vec!["foo-a", "foo-b"],
            vec!["foo-b"],
        ];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_isolated_non_existent() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-a", "bar-b", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "non-existent".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            ..Default::default()
        };
        let want = vec![
            vec![],
            vec!["bar-a"],
            vec!["bar-a", "bar-b"],
            vec!["bar-b"],
            vec!["foo-a"],
        ];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_isolated_denylist() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-b", "bar-a", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "foo-b".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            exclude_features: HashSet::from(["bar-a".to_string()]),
            ..Default::default()
        };
        let want = vec![
            vec![],
            vec!["bar-b"],
            vec!["foo-a"],
            vec!["foo-a", "foo-b"],
            vec!["foo-b"],
        ];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_isolated_non_existent_denylist() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-b", "foo-a", "bar-a", "bar-b", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "non-existent".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            exclude_features: HashSet::from(["bar-a".to_string()]),
            ..Default::default()
        };
        let want = vec![vec![], vec!["bar-b"], vec!["foo-a"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_isolated_non_existent_denylist_exact() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-a", "bar-b", "car-a", "car-b"])?;
        let config = Config {
            isolated_feature_sets: vec![
                HashSet::from(["foo-a".to_string(), "non-existent".to_string()]),
                HashSet::from(["bar-a".to_string(), "bar-b".to_string()]),
            ],
            exclude_features: HashSet::from(["bar-a".to_string()]),
            include_feature_sets: vec![HashSet::from([
                "car-a".to_string(),
                "bar-a".to_string(),
                "non-existent".to_string(),
            ])],
            ..Default::default()
        };
        let want = vec![vec![], vec!["bar-a", "car-a"], vec!["bar-b"], vec!["foo-a"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_allow_feature_sets_exact() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["hydrate", "ssr", "other"])?;
        let config = Config {
            allow_feature_sets: vec![
                HashSet::from(["ssr".to_string()]),
                HashSet::from(["hydrate".to_string()]),
            ],
            ..Default::default()
        };

        let want = vec![vec!["hydrate"], vec!["ssr"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_allow_feature_sets_ignores_other_options() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["hydrate", "ssr"])?;
        let config = Config {
            allow_feature_sets: vec![HashSet::from(["hydrate".to_string()])],
            exclude_features: HashSet::from(["hydrate".to_string()]),
            exclude_feature_sets: vec![HashSet::from(["hydrate".to_string()])],
            include_feature_sets: vec![HashSet::from(["ssr".to_string()])],
            only_features: HashSet::from(["ssr".to_string()]),
            ..Default::default()
        };

        let want = vec![vec!["hydrate"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_no_empty_feature_set_filters_generated_empty() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo", "bar"])?;
        let config = Config {
            no_empty_feature_set: true,
            ..Default::default()
        };

        let want = vec![vec!["bar"], vec!["bar", "foo"], vec!["foo"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_no_empty_feature_set_filters_included_empty() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo"])?;
        let config = Config {
            include_feature_sets: vec![HashSet::new()],
            no_empty_feature_set: true,
            ..Default::default()
        };

        let want = vec![vec!["foo"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_exclude_empty_feature_set_only() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo", "bar"])?;
        let config = Config {
            exclude_feature_sets: vec![HashSet::new()],
            ..Default::default()
        };

        let want = vec![vec!["bar"], vec!["bar", "foo"], vec!["foo"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn too_many_feature_configurations() -> eyre::Result<()> {
        init();
        let features: Vec<String> = (0..25).map(|i| format!("f{i}")).collect();
        let feature_refs: Vec<&str> = features.iter().map(String::as_str).collect();
        let package = package_with_features(&feature_refs)?;

        let config = Config::default();
        let Err(err) = package.feature_combinations(&config) else {
            eyre::bail!("expected too-many-configurations error");
        };
        let Some(err) = err.downcast_ref::<FeatureCombinationError>() else {
            eyre::bail!("expected FeatureCombinationError");
        };
        assert!(
            err.to_string().contains("too many configurations"),
            "expected 'too many configurations' error, got: {err}"
        );
        Ok(())
    }
}
