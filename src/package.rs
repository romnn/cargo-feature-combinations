//! Package-level configuration, feature combination generation, and error types.

use crate::config::patch::{FeatureSetVecPatch, StringSetPatch};
use crate::config::{Config, ResolvedFeatures, validate_package_metadata};
use crate::print_warning;
use crate::{DEFAULT_METADATA_KEY, find_metadata_value, pkg_metadata_section};
use color_eyre::eyre::{self, WrapErr};
use itertools::Itertools;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;

const DEFAULT_MAX_FEATURE_COMBINATIONS: u128 = 100_000;

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
                    "too many configurations for package `{package}`: {num_features} feature(s) would produce {} combinations (limit: {limit})",
                    num_configurations
                        .map_or_else(|| "an unbounded number of".to_string(), |v| v.to_string()),
                )
            }
        }
    }
}

impl std::error::Error for FeatureCombinationError {}

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
        // Short-circuit: if an explicit allowlist of feature sets is configured,
        // interpret it as the complete matrix.
        //
        // This is intentionally *not* combined with the normal powerset-based
        // generation and its filters: the user is declaring the exact sets they
        // care about (e.g. SSR vs hydrate), and we should not implicitly add
        // `[]` or any other combinations.
        if !config.allow_feature_sets.is_empty() {
            let mut allowed: BTreeSet<BTreeSet<&'a String>> = config
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

        let effective_exclude_features = derive_effective_exclude_features(&self.features, config);

        validate_mutually_exclusive_features(
            &self.name,
            &self.features,
            &config.include_features,
            &config.mutually_exclusive_features,
        )?;

        // Generate the base powerset from
        // - all features
        // - or from isolated sets, minus excluded features
        let base_powerset = generate_base_powerset(
            &self.name,
            &self.features,
            &effective_exclude_features,
            config,
        )?;

        // Filter out feature sets that contain skip sets
        let mut filtered_powerset = base_powerset
            .into_iter()
            .filter(|feature_set| {
                !violates_mutually_exclusive_features(
                    feature_set,
                    &config.mutually_exclusive_features,
                ) && !config.exclude_feature_sets.iter().any(|skip_set| {
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
            let exact_combination: BTreeSet<&'a String> = proposed_exact_combination
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

fn derive_effective_exclude_features(
    package_features: &BTreeMap<String, Vec<String>>,
    config: &ResolvedFeatures,
) -> HashSet<String> {
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
        for (feature_name, implied) in package_features {
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

    effective_exclude_features
}

fn validate_mutually_exclusive_features(
    package_name: &str,
    package_features: &BTreeMap<String, Vec<String>>,
    include_features: &HashSet<String>,
    groups: &[HashSet<String>],
) -> eyre::Result<()> {
    for (left_index, left) in groups.iter().enumerate() {
        for right in groups.iter().skip(left_index + 1) {
            if let Some(shared) = left.intersection(right).sorted().next() {
                eyre::bail!(
                    "invalid mutually_exclusive_features for package `{package_name}`: groups {} and {} overlap at feature `{shared}`",
                    format_feature_group(left),
                    format_feature_group(right),
                );
            }
        }
    }

    for group in groups {
        // Any *known* included member counts as forced: `include_features` are
        // chained into every combination regardless of `exclude_features` /
        // `only_features`, so filtering by the varied universe here would let
        // two forced members slip through and silently empty the matrix.
        let forced = group
            .iter()
            .filter(|feature| package_features.contains_key(*feature))
            .filter(|feature| include_features.contains(*feature))
            .sorted()
            .take(2)
            .collect::<Vec<_>>();
        if let [first, second] = forced.as_slice() {
            eyre::bail!(
                "invalid mutually_exclusive_features for package `{package_name}`: group {} forces conflicting features `{first}` and `{second}` through include_features",
                format_feature_group(group),
            );
        }
    }

    Ok(())
}

fn format_feature_group(group: &HashSet<String>) -> String {
    format!("[{}]", group.iter().sorted().join(", "))
}

fn violates_mutually_exclusive_features(
    feature_set: &BTreeSet<&String>,
    groups: &[HashSet<String>],
) -> bool {
    groups.iter().any(|group| {
        group
            .iter()
            .filter(|feature| feature_set.contains(*feature))
            .take(2)
            .count()
            >= 2
    })
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

fn checked_num_combinations(num_features: usize) -> Option<u128> {
    if num_features >= u128::BITS as usize {
        return None;
    }
    let shift: u32 = num_features.try_into().ok()?;
    Some(1u128 << shift)
}

fn generate_base_powerset<'a>(
    package_name: &str,
    package_features: &'a BTreeMap<String, Vec<String>>,
    effective_exclude_features: &HashSet<String>,
    config: &ResolvedFeatures,
) -> Result<BTreeSet<BTreeSet<&'a String>>, FeatureCombinationError> {
    let max_combinations = config
        .max_combinations
        .unwrap_or(DEFAULT_MAX_FEATURE_COMBINATIONS);
    if !config.isolated_feature_sets.is_empty() {
        return generate_isolated_base_powerset(
            package_name,
            package_features,
            &config.isolated_feature_sets,
            effective_exclude_features,
            &config.include_features,
            &config.only_features,
            max_combinations,
        );
    }
    if !config.mutually_exclusive_features.is_empty() {
        return generate_mutually_exclusive_global_base_powerset(
            package_name,
            package_features,
            effective_exclude_features,
            &config.include_features,
            &config.only_features,
            &config.mutually_exclusive_features,
            max_combinations,
        );
    }
    generate_global_base_powerset(
        package_name,
        package_features,
        effective_exclude_features,
        &config.include_features,
        &config.only_features,
        max_combinations,
    )
}

fn ensure_within_combination_limit(
    package_name: &str,
    num_features: usize,
    limit: u128,
) -> Result<(), FeatureCombinationError> {
    let num_configurations = checked_num_combinations(num_features);
    let exceeds = match num_configurations {
        Some(n) => n > limit,
        None => true,
    };

    if exceeds {
        return Err(FeatureCombinationError::TooManyConfigurations {
            package: package_name.to_string(),
            num_features,
            num_configurations,
            limit,
        });
    }

    Ok(())
}

/// Known `include_features` as references into `package_features`.
///
/// These are chained into every generated combination, deliberately bypassing
/// `exclude_features` / `only_features`, which only shape the varied universe.
fn known_include_features<'a>(
    package_features: &'a BTreeMap<String, Vec<String>>,
    include_features: &HashSet<String>,
) -> BTreeSet<&'a String> {
    include_features
        .iter()
        .filter_map(|feature| package_features.get_key_value(feature).map(|(key, _)| key))
        .collect()
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
    include_features: &HashSet<String>,
    only_features: &HashSet<String>,
    max_combinations: u128,
) -> Result<BTreeSet<BTreeSet<&'a String>>, FeatureCombinationError> {
    let included = known_include_features(package_features, include_features);
    let features = package_features
        .keys()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|ft| !exclude_features.contains(*ft))
        .filter(|ft| only_features.is_empty() || only_features.contains(*ft))
        .collect::<BTreeSet<_>>();

    ensure_within_combination_limit(package_name, features.len(), max_combinations)?;

    Ok(features
        .into_iter()
        .powerset()
        .map(|combination| {
            combination
                .into_iter()
                .chain(included.iter().copied())
                .collect::<BTreeSet<&'a String>>()
        })
        .collect())
}

/// Generates the global matrix without materializing combinations that violate
/// a mutually exclusive feature group.
fn generate_mutually_exclusive_global_base_powerset<'a>(
    package_name: &str,
    package_features: &'a BTreeMap<String, Vec<String>>,
    exclude_features: &HashSet<String>,
    include_features: &HashSet<String>,
    only_features: &HashSet<String>,
    mutually_exclusive_features: &[HashSet<String>],
    max_combinations: u128,
) -> Result<BTreeSet<BTreeSet<&'a String>>, FeatureCombinationError> {
    let included = known_include_features(package_features, include_features);
    let features = package_features
        .keys()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|feature| !exclude_features.contains(*feature))
        .filter(|feature| only_features.is_empty() || only_features.contains(*feature))
        .collect::<BTreeSet<_>>();
    let groups = mutually_exclusive_features
        .iter()
        .map(|group| {
            group
                .iter()
                .filter_map(|feature| package_features.get_key_value(feature).map(|(key, _)| key))
                .filter(|feature| features.contains(feature))
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    let unconstrained_features = features
        .iter()
        .copied()
        .filter(|feature| groups.iter().all(|group| !group.contains(feature)))
        .collect::<BTreeSet<_>>();
    let group_choices = mutually_exclusive_features
        .iter()
        .zip(&groups)
        .map(|(raw_group, group)| {
            // A *known* included member collapses the group even when universe
            // filters drop it from the varied features: `include_features` are
            // chained into every combination regardless of those filters, so
            // any other member would immediately violate the group.
            let forces_known_member = raw_group.iter().any(|feature| {
                package_features.contains_key(feature) && include_features.contains(feature)
            });
            if forces_known_member {
                vec![None]
            } else {
                std::iter::once(None)
                    .chain(group.iter().copied().map(Some))
                    .collect::<Vec<_>>()
            }
        })
        .collect::<Vec<_>>();

    let num_configurations =
        checked_num_combinations(unconstrained_features.len()).and_then(|initial| {
            group_choices.iter().try_fold(initial, |total, choices| {
                total.checked_mul(u128::try_from(choices.len()).ok()?)
            })
        });
    if num_configurations.is_none_or(|count| count > max_combinations) {
        return Err(FeatureCombinationError::TooManyConfigurations {
            package: package_name.to_string(),
            num_features: features.len(),
            num_configurations,
            limit: max_combinations,
        });
    }

    let mut combinations = unconstrained_features
        .into_iter()
        .powerset()
        .map(|combination| combination.into_iter().collect::<BTreeSet<_>>())
        .collect::<BTreeSet<_>>();
    for choices in group_choices {
        combinations = combinations
            .into_iter()
            .flat_map(|combination| {
                choices.iter().map(move |choice| {
                    let mut combination = combination.clone();
                    if let Some(feature) = *choice {
                        combination.insert(feature);
                    }
                    combination
                })
            })
            .collect();
    }

    Ok(combinations
        .into_iter()
        .map(|combination| {
            combination
                .into_iter()
                .chain(included.iter().copied())
                .collect::<BTreeSet<_>>()
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
    include_features: &HashSet<String>,
    only_features: &HashSet<String>,
    max_combinations: u128,
) -> Result<BTreeSet<BTreeSet<&'a String>>, FeatureCombinationError> {
    // Collect known package features for easy querying
    let known_features = package_features.keys().collect::<HashSet<_>>();
    let included = known_include_features(package_features, include_features);

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
                limit: max_combinations,
            });
        };

        worst_case_total = worst_case_total.saturating_add(n);
        if worst_case_total > max_combinations {
            return Err(FeatureCombinationError::TooManyConfigurations {
                package: package_name.to_string(),
                num_features,
                num_configurations: Some(worst_case_total),
                limit: max_combinations,
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
                        .chain(included.iter().copied())
                        .collect::<BTreeSet<_>>()
                })
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod test {
    use super::{FeatureCombinationError, Package};
    use crate::config::{Config, ResolvedFeatures};
    use color_eyre::eyre;
    use itertools::Itertools;
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::collections::{BTreeSet, HashSet};

    static INIT: std::sync::Once = std::sync::Once::new();

    fn init() {
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

    fn naive_mutually_exclusive_combinations<'a>(
        package: &'a cargo_metadata::Package,
        config: &ResolvedFeatures,
    ) -> Vec<Vec<&'a String>> {
        let included = config
            .include_features
            .iter()
            .filter_map(|feature| package.features.get_key_value(feature).map(|(key, _)| key))
            .collect::<BTreeSet<_>>();
        let features = package
            .features
            .keys()
            .filter(|feature| !config.exclude_features.contains(*feature))
            .filter(|feature| {
                config.only_features.is_empty() || config.only_features.contains(*feature)
            })
            .collect::<BTreeSet<_>>();
        let forbidden_pairs = config
            .mutually_exclusive_features
            .iter()
            .flat_map(|group| group.iter().array_combinations::<2>())
            .collect::<Vec<_>>();
        let mut combinations = features
            .into_iter()
            .powerset()
            .map(|combination| {
                combination
                    .into_iter()
                    .chain(included.iter().copied())
                    .collect::<BTreeSet<_>>()
            })
            .filter(|combination| {
                forbidden_pairs.iter().all(|[left, right]| {
                    !(combination.contains(left) && combination.contains(right))
                })
            })
            .filter(|combination| {
                !config.exclude_feature_sets.iter().any(|excluded| {
                    if excluded.is_empty() {
                        combination.is_empty()
                    } else {
                        excluded.iter().all(|feature| combination.contains(feature))
                    }
                })
            })
            .collect::<BTreeSet<_>>();

        for included_set in &config.include_feature_sets {
            combinations.insert(
                included_set
                    .iter()
                    .filter_map(|feature| {
                        package.features.get_key_value(feature).map(|(key, _)| key)
                    })
                    .collect(),
            );
        }
        if config.no_empty_feature_set {
            combinations.retain(|combination| !combination.is_empty());
        }

        combinations
            .into_iter()
            .map(|combination| combination.into_iter().sorted().collect())
            .sorted()
            .collect()
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
        let have = package.feature_combinations(&ResolvedFeatures::from_config(&config))?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_only_features() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo", "bar", "baz"])?;
        let config = ResolvedFeatures {
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
    fn combinations_mutually_exclusive_preserve_open_world_powerset() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml", "webgpu", "tracing"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            ..ResolvedFeatures::default()
        };

        let have = package.feature_combinations(&config)?;

        assert_eq!(have.len(), 12);
        assert!(have.iter().any(Vec::is_empty));
        assert!(have.iter().any(|features| features == &["cuda"]));
        assert!(have.iter().any(|features| features == &["coreml"]));
        assert!(
            have.iter()
                .any(|features| { features == &["coreml", "tracing", "webgpu"] })
        );
        assert!(!have.iter().any(|features| {
            features.contains(&&"cuda".to_string()) && features.contains(&&"coreml".to_string())
        }));
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_ignore_unknown_and_degenerate_groups() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "tracing"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![
                HashSet::new(),
                HashSet::from(["cuda".to_string()]),
                HashSet::from(["unknown-a".to_string(), "unknown-b".to_string()]),
            ],
            ..ResolvedFeatures::default()
        };

        let want = vec![
            vec![],
            vec!["cuda"],
            vec!["cuda", "tracing"],
            vec!["tracing"],
        ];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_respect_universe_filters() -> eyre::Result<()> {
        init();
        let mut package = package_with_features(&["cuda", "coreml", "tracing"])?;
        package
            .features
            .insert("coreml".to_string(), vec!["dep:coreml".to_string()]);
        let group = HashSet::from(["cuda".to_string(), "coreml".to_string()]);

        for config in [
            ResolvedFeatures {
                mutually_exclusive_features: vec![group.clone()],
                exclude_features: HashSet::from(["coreml".to_string()]),
                ..ResolvedFeatures::default()
            },
            ResolvedFeatures {
                mutually_exclusive_features: vec![group.clone()],
                only_features: HashSet::from(["cuda".to_string(), "tracing".to_string()]),
                ..ResolvedFeatures::default()
            },
            ResolvedFeatures {
                mutually_exclusive_features: vec![group.clone()],
                skip_optional_dependencies: true,
                ..ResolvedFeatures::default()
            },
        ] {
            let have = package.feature_combinations(&config)?;
            let want = vec![
                vec![],
                vec!["cuda"],
                vec!["cuda", "tracing"],
                vec!["tracing"],
            ];
            sim_assert_eq!(have: have, want: want);
        }
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_collapse_around_included_member() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml", "tracing"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            include_features: HashSet::from(["cuda".to_string()]),
            ..ResolvedFeatures::default()
        };

        let want = vec![vec!["cuda"], vec!["cuda", "tracing"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_collapse_around_included_excluded_member() -> eyre::Result<()>
    {
        init();
        let package = package_with_features(&["cuda", "coreml", "tracing"])?;
        // `cuda` is excluded from the varied universe but still chained into
        // every combination through `include_features`, so the group must
        // collapse to it instead of offering `coreml`.
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            include_features: HashSet::from(["cuda".to_string()]),
            exclude_features: HashSet::from(["cuda".to_string()]),
            ..ResolvedFeatures::default()
        };

        let want = vec![vec!["cuda"], vec!["cuda", "tracing"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_reject_forced_conflict_with_excluded_member()
    -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml"])?;
        // Excluding `cuda` does not stop `include_features` from forcing it
        // into every combination, so this must still be a forced conflict
        // instead of a silently empty matrix.
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            include_features: HashSet::from(["cuda".to_string(), "coreml".to_string()]),
            exclude_features: HashSet::from(["cuda".to_string()]),
            ..ResolvedFeatures::default()
        };

        let err = package
            .feature_combinations(&config)
            .expect_err("forcing two mutually exclusive features should fail");
        let message = err.to_string();

        assert!(message.contains("cuda"), "{message}");
        assert!(message.contains("coreml"), "{message}");
        assert!(message.contains("include_features"), "{message}");
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_reject_forced_conflict() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            include_features: HashSet::from(["cuda".to_string(), "coreml".to_string()]),
            ..ResolvedFeatures::default()
        };

        let err = package
            .feature_combinations(&config)
            .expect_err("forcing two mutually exclusive features should fail");
        let message = err.to_string();

        assert!(message.contains("test"), "{message}");
        assert!(message.contains("cuda"), "{message}");
        assert!(message.contains("coreml"), "{message}");
        assert!(message.contains("include_features"), "{message}");
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_include_exact_set_is_escape_hatch() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            include_feature_sets: vec![HashSet::from(["cuda".to_string(), "coreml".to_string()])],
            ..ResolvedFeatures::default()
        };

        let want = vec![vec![], vec!["coreml"], vec!["coreml", "cuda"], vec!["cuda"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_allow_feature_sets_ignore_mutually_exclusive_groups() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![
                HashSet::from(["cuda".to_string(), "coreml".to_string()]),
                HashSet::from(["coreml".to_string(), "overlap".to_string()]),
            ],
            include_features: HashSet::from(["cuda".to_string(), "coreml".to_string()]),
            allow_feature_sets: vec![HashSet::from(["cuda".to_string(), "coreml".to_string()])],
            ..ResolvedFeatures::default()
        };

        let want = vec![vec!["coreml", "cuda"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_filter_isolated_powersets() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml", "tracing"])?;
        let config = ResolvedFeatures {
            isolated_feature_sets: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
                "tracing".to_string(),
            ])],
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            ..ResolvedFeatures::default()
        };

        let want = vec![
            vec![],
            vec!["coreml"],
            vec!["coreml", "tracing"],
            vec!["cuda"],
            vec!["cuda", "tracing"],
            vec!["tracing"],
        ];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_respect_no_empty_feature_set() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from([
                "cuda".to_string(),
                "coreml".to_string(),
            ])],
            no_empty_feature_set: true,
            ..ResolvedFeatures::default()
        };

        let want = vec![vec!["coreml"], vec!["cuda"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_use_named_features_not_implication_closures()
    -> eyre::Result<()> {
        init();
        let mut package = package_with_features(&["a", "b"])?;
        package
            .features
            .insert("b".to_string(), vec!["a".to_string()]);
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![HashSet::from(["a".to_string(), "b".to_string()])],
            ..ResolvedFeatures::default()
        };

        let want = vec![vec![], vec!["a"], vec!["b"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_mutually_exclusive_reject_overlapping_groups() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["cuda", "coreml"])?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![
                HashSet::from(["cuda".to_string(), "shared".to_string()]),
                HashSet::from(["coreml".to_string(), "shared".to_string()]),
            ],
            ..ResolvedFeatures::default()
        };

        let err = package
            .feature_combinations(&config)
            .expect_err("overlapping mutually exclusive groups should fail");
        let message = err.to_string();

        assert!(message.contains("test"), "{message}");
        assert!(message.contains("shared"), "{message}");
        assert!(message.contains("cuda"), "{message}");
        assert!(message.contains("coreml"), "{message}");
        Ok(())
    }

    #[test]
    fn mutually_exclusive_direct_generator_matches_pairwise_exclusion_oracle() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["a", "b", "c", "d", "e"])?;
        let group_families = [
            vec![],
            vec![HashSet::from(["a".to_string(), "b".to_string()])],
            vec![
                HashSet::from(["a".to_string(), "b".to_string()]),
                HashSet::from(["c".to_string(), "d".to_string()]),
            ],
        ];

        // Each bit independently enables one matrix-shaping interaction, so
        // every group family is checked against all 128 configurations. Bits 2
        // and 5 together include *and* exclude the group member `a` — the case
        // where direct generation and pairwise desugaring diverge most easily.
        for groups in group_families {
            for options in 0u8..128 {
                let config = ResolvedFeatures {
                    mutually_exclusive_features: groups.clone(),
                    exclude_features: {
                        let mut excluded = HashSet::new();
                        if options & 1 != 0 {
                            excluded.insert("e".to_string());
                        }
                        if options & 32 != 0 {
                            excluded.insert("a".to_string());
                        }
                        excluded
                    },
                    only_features: if options & 2 != 0 {
                        HashSet::from([
                            "a".to_string(),
                            "b".to_string(),
                            "c".to_string(),
                            "d".to_string(),
                        ])
                    } else {
                        HashSet::new()
                    },
                    include_features: if options & 4 != 0 {
                        HashSet::from(["a".to_string()])
                    } else {
                        HashSet::new()
                    },
                    include_feature_sets: if options & 8 != 0 {
                        vec![HashSet::from(["a".to_string(), "b".to_string()])]
                    } else {
                        Vec::new()
                    },
                    no_empty_feature_set: options & 16 != 0,
                    exclude_feature_sets: if options & 64 != 0 {
                        vec![HashSet::from(["a".to_string(), "c".to_string()])]
                    } else {
                        Vec::new()
                    },
                    ..ResolvedFeatures::default()
                };

                let have = package.feature_combinations(&config)?;
                let want = naive_mutually_exclusive_combinations(&package, &config);

                sim_assert_eq!(have: have, want: want, "groups={groups:?}, options={options:07b}");
            }
        }
        Ok(())
    }

    #[test]
    fn mutually_exclusive_limit_uses_constrained_count() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["a", "b", "c", "free-a", "free-b"])?;
        let group = HashSet::from(["a".to_string(), "b".to_string(), "c".to_string()]);
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![group.clone()],
            max_combinations: Some(16),
            ..ResolvedFeatures::default()
        };

        let combinations = package.feature_combinations(&config)?;

        assert_eq!(combinations.len(), 16);

        let err = package
            .feature_combinations(&ResolvedFeatures {
                mutually_exclusive_features: vec![group],
                max_combinations: Some(15),
                ..ResolvedFeatures::default()
            })
            .expect_err("the constrained 16-row matrix should exceed limit 15");
        assert!(matches!(
            err.downcast_ref::<FeatureCombinationError>(),
            Some(FeatureCombinationError::TooManyConfigurations {
                num_configurations: Some(16),
                limit: 15,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn mutually_exclusive_group_avoids_naive_powerset_overflow() -> eyre::Result<()> {
        init();
        let features = (0..128)
            .map(|index| format!("f{index}"))
            .collect::<Vec<_>>();
        let feature_refs = features.iter().map(String::as_str).collect::<Vec<_>>();
        let package = package_with_features(&feature_refs)?;
        let config = ResolvedFeatures {
            mutually_exclusive_features: vec![features.into_iter().collect()],
            max_combinations: Some(129),
            ..ResolvedFeatures::default()
        };

        let combinations = package.feature_combinations(&config)?;

        assert_eq!(combinations.len(), 129);
        Ok(())
    }

    #[test]
    fn combinations_respects_configured_max_combinations() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo", "bar"])?;
        let config = ResolvedFeatures {
            max_combinations: Some(3),
            ..ResolvedFeatures::default()
        };

        let err = package
            .feature_combinations(&config)
            .expect_err("2 features produce 4 combinations and should exceed limit 3");

        assert!(matches!(
            err.downcast_ref::<FeatureCombinationError>(),
            Some(FeatureCombinationError::TooManyConfigurations { limit: 3, .. })
        ));
        Ok(())
    }

    #[test]
    fn combinations_isolated() -> eyre::Result<()> {
        init();
        let package =
            package_with_features(&["foo-a", "foo-b", "bar-b", "bar-a", "car-b", "car-a"])?;
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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
    fn combinations_allow_feature_sets_normalize_unknown_features() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["hydrate", "ssr"])?;
        // Unknown names are rejected at config load; a `ResolvedFeatures`
        // built directly (library callers, tests) still normalizes them away
        // as defense in depth.
        let config = ResolvedFeatures {
            allow_feature_sets: vec![
                HashSet::from(["hydrate".to_string(), "unknown".to_string()]),
                HashSet::from(["ssr".to_string()]),
            ],
            ..Default::default()
        };

        let want = vec![vec!["hydrate"], vec!["ssr"]];
        let have = package.feature_combinations(&config)?;

        sim_assert_eq!(have: have, want: want);
        Ok(())
    }

    #[test]
    fn combinations_no_empty_feature_set_filters_generated_empty() -> eyre::Result<()> {
        init();
        let package = package_with_features(&["foo", "bar"])?;
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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
        let config = ResolvedFeatures {
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

        let config = ResolvedFeatures::default();
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
