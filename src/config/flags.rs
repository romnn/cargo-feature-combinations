use serde::{Deserialize, Serialize};

macro_rules! for_each_flag_config_field {
    ($callback:ident) => {
        $callback!(
            summary_only,
            diagnostics_only,
            dedupe,
            verbose,
            pedantic,
            errors_only,
            packages_only,
            fail_fast,
            no_prune_implied,
            prune_implied,
            show_pruned,
            aggregate_targets,
            no_targets,
            install_missing_targets,
            only_packages_with_lib_target,
        );
    };
}

// Fields copied by the default `unwrap_or(false)` rule. Diagnostics and
// normalized pruning are resolved explicitly because they have implications.
macro_rules! for_each_simple_resolved_flag_field {
    ($callback:ident) => {
        $callback!(
            summary_only,
            verbose,
            pedantic,
            errors_only,
            packages_only,
            fail_fast,
            show_pruned,
            aggregate_targets,
            no_targets,
            install_missing_targets,
            only_packages_with_lib_target,
        );
    };
}

macro_rules! define_flag_keys {
    ($($field:ident),+ $(,)?) => {
        pub(crate) const FLAG_KEYS: &[&str] = &[$(stringify!($field),)+ "dedup"];
    };
}
for_each_flag_config_field!(define_flag_keys);

/// Raw configurable cargo-fc flag defaults.
///
/// Each field is tri-state: absent means "inherit from the next broader
/// scope", while `true`/`false` explicitly override broader config. CLI flags
/// are converted into this same shape with `true` entries and overlaid last.
#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlagConfig {
    /// Whether to hide cargo output and only print summaries.
    #[serde(default)]
    pub summary_only: Option<bool>,
    /// Whether to show only rendered compiler diagnostics.
    #[serde(default)]
    pub diagnostics_only: Option<bool>,
    /// Whether to deduplicate rendered diagnostics across combinations.
    ///
    /// Accept `dedup` as a spelling alias for users who mirror the CLI alias.
    #[serde(default, alias = "dedup")]
    pub dedupe: Option<bool>,
    /// Whether to print cargo-fc's verbose command header details.
    #[serde(default)]
    pub verbose: Option<bool>,
    /// Whether warnings should fail summaries and fail-fast checks.
    #[serde(default)]
    pub pedantic: Option<bool>,
    /// Whether to allow warnings and show errors only.
    #[serde(default)]
    pub errors_only: Option<bool>,
    /// Whether matrix output should emit one row per package-target.
    #[serde(default)]
    pub packages_only: Option<bool>,
    /// Whether execution should stop at the first bad combination.
    #[serde(default)]
    pub fail_fast: Option<bool>,
    /// Whether to disable automatic implied-feature pruning.
    #[serde(default)]
    pub no_prune_implied: Option<bool>,
    /// Positive spelling kept for existing config and readability.
    #[serde(default)]
    pub prune_implied: Option<bool>,
    /// Whether pruned combinations should be shown in the summary.
    #[serde(default)]
    pub show_pruned: Option<bool>,
    /// Whether compatible target invocations may be aggregated.
    #[serde(default)]
    pub aggregate_targets: Option<bool>,
    /// Whether configured target lists should be ignored.
    #[serde(default)]
    pub no_targets: Option<bool>,
    /// Whether missing Rust target components should be installed with rustup.
    #[serde(default)]
    pub install_missing_targets: Option<bool>,
    /// Whether packages without a library target should be skipped.
    #[serde(default)]
    pub only_packages_with_lib_target: Option<bool>,
}

impl FlagConfig {
    /// Validate contradictions that can be expressed inside one raw scope.
    pub(crate) fn validate(self) -> color_eyre::eyre::Result<()> {
        if self.no_prune_implied.is_some() && self.prune_implied.is_some() {
            color_eyre::eyre::bail!(
                "`no_prune_implied` and `prune_implied` are contradictory; configure only one spelling in the same scope"
            );
        }
        if self.dedupe == Some(true) && self.diagnostics_only == Some(false) {
            color_eyre::eyre::bail!(
                "`dedupe = true` requires diagnostics-only output; do not set `diagnostics_only = false` in the same scope"
            );
        }
        Ok(())
    }

    /// Overlay explicitly configured values from `other`.
    pub fn overlay(&mut self, other: Self) {
        macro_rules! overlay_fields {
            ($($field:ident),+ $(,)?) => {
                $(overlay_bool(&mut self.$field, other.$field);)+
            };
        }
        for_each_flag_config_field!(overlay_fields);
        if other.diagnostics_only == Some(false) && other.dedupe != Some(true) {
            self.dedupe = Some(false);
        }
        if other.dedupe == Some(true)
            && other.diagnostics_only.is_none()
            && self.diagnostics_only == Some(false)
        {
            self.diagnostics_only = None;
        }
        if let Some(value) = other.no_prune_implied {
            self.no_prune_implied = Some(value);
            self.prune_implied = None;
        }
        if let Some(value) = other.prune_implied {
            self.prune_implied = Some(value);
            self.no_prune_implied = None;
        }
    }

    /// Whether this config explicitly asks cargo-fc to enable diagnostics mode.
    #[must_use]
    pub fn requests_diagnostics(self) -> bool {
        self.diagnostics_only == Some(true) || self.dedupe == Some(true)
    }

    /// Whether this config explicitly mentions diagnostics mode.
    #[must_use]
    pub fn mentions_diagnostics(self) -> bool {
        self.diagnostics_only.is_some() || self.dedupe.is_some()
    }
}

fn overlay_bool(target: &mut Option<bool>, source: Option<bool>) {
    if source.is_some() {
        *target = source;
    }
}

/// Fully resolved cargo-fc flags for one phase or package-target execution.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedFlags {
    /// Hide cargo output and print only summaries.
    pub summary_only: bool,
    /// Request JSON diagnostics and render only compiler diagnostics.
    pub diagnostics_only: bool,
    /// Deduplicate rendered diagnostics across invocations.
    pub dedupe: bool,
    /// Print full spawned commands in the per-combination header.
    pub verbose: bool,
    /// Treat warnings like failures in summaries and fail-fast checks.
    pub pedantic: bool,
    /// Suppress warnings from rustc and show errors only.
    pub errors_only: bool,
    /// Emit one matrix row per package-target instead of every feature combo.
    pub packages_only: bool,
    /// Stop execution after the first failing combination.
    pub fail_fast: bool,
    /// Disable implied-feature pruning.
    pub no_prune_implied: bool,
    /// Show pruned combinations in the final summary.
    pub show_pruned: bool,
    /// Use aggregate target execution when the whole run can do so.
    pub aggregate_targets: bool,
    /// Ignore configured target lists during target selection.
    pub no_targets: bool,
    /// Install missing Rust target components with rustup before execution.
    pub install_missing_targets: bool,
    /// Skip packages that do not expose a library target.
    pub only_packages_with_lib_target: bool,
}

impl ResolvedFlags {
    /// Convert an overlaid raw config into concrete flags.
    #[must_use]
    pub fn from_config(config: FlagConfig) -> Self {
        let mut out = Self::default();
        macro_rules! set_defaulted_fields {
            ($($field:ident),+ $(,)?) => {
                $(out.$field = config.$field.unwrap_or(false);)+
            };
        }
        for_each_simple_resolved_flag_field!(set_defaulted_fields);

        let no_prune_implied = config
            .no_prune_implied
            .or_else(|| config.prune_implied.map(|enabled| !enabled))
            .unwrap_or(false);
        let dedupe = config.dedupe.unwrap_or(false);
        out.diagnostics_only = config.diagnostics_only.unwrap_or(false) || dedupe;
        out.dedupe = dedupe;
        out.no_prune_implied = no_prune_implied;
        out
    }

    /// Convert an overlaid raw config into concrete flags.
    ///
    /// # Errors
    ///
    /// Returns an error when the merged config explicitly enables `dedupe`
    /// while explicitly disabling `diagnostics_only`. Dedupe consumes the
    /// diagnostics-only JSON stream, so those settings are contradictory.
    pub(crate) fn try_from_config(config: FlagConfig) -> color_eyre::eyre::Result<Self> {
        if config.dedupe == Some(true) && config.diagnostics_only == Some(false) {
            color_eyre::eyre::bail!(
                "`dedupe = true` requires `diagnostics_only = true`; remove `diagnostics_only = false` or set `dedupe = false`"
            );
        }
        Ok(Self::from_config(config))
    }
}

pub(crate) fn gated_plain_diagnostics(
    mut flags: FlagConfig,
    ignored_diagnostics_config: &mut bool,
) -> FlagConfig {
    if flags.requests_diagnostics() {
        *ignored_diagnostics_config = true;
    } else if flags.mentions_diagnostics() {
        *ignored_diagnostics_config = false;
    }

    if flags.diagnostics_only == Some(true) {
        flags.diagnostics_only = None;
    }
    if flags.dedupe == Some(true) {
        flags.dedupe = None;
    }

    flags
}

pub(crate) fn combine_flag_configs<'a>(
    field_prefix: Option<&str>,
    source_kind: &str,
    entries: impl IntoIterator<Item = (&'a str, FlagConfig)>,
) -> color_eyre::eyre::Result<FlagConfig> {
    let entries: Vec<_> = entries.into_iter().collect();
    let mut out = FlagConfig::default();
    macro_rules! combine {
        ($($field:ident),+ $(,)?) => {
            $(
                let field_name = if let Some(prefix) = field_prefix {
                    format!("{prefix}.{}", stringify!($field))
                } else {
                    stringify!($field).to_string()
                };
                if let Some(value) = combine_bool(
                    &field_name,
                    source_kind,
                    &entries,
                    |flags| flags.$field,
                )? {
                    out.$field = Some(value);
                }
            )+
        };
    }
    for_each_flag_config_field!(combine);
    out.validate()?;
    Ok(out)
}

/// Fold a scalar field across the sibling entries that make up one precedence
/// layer (e.g. several `target.'cfg(...)'` sections matching one triple),
/// erroring if two siblings set differing values. Returns `None` when no sibling
/// set the field.
fn combine_scalar<'a, T, V: PartialEq>(
    name: &str,
    source_kind: &str,
    entries: &'a [(&'a str, T)],
    get: impl Fn(&'a T) -> Option<V>,
) -> color_eyre::eyre::Result<Option<V>> {
    let mut out: Option<V> = None;
    for (expr, item) in entries {
        if let Some(value) = get(item) {
            match &out {
                None => out = Some(value),
                Some(existing) if *existing == value => {}
                Some(_) => {
                    color_eyre::eyre::bail!(
                        "conflicting values for `{name}` in {source_kind} `{expr}`"
                    );
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn combine_bool<T>(
    name: &str,
    source_kind: &str,
    entries: &[(&str, T)],
    get: impl Fn(&T) -> Option<bool>,
) -> color_eyre::eyre::Result<Option<bool>> {
    combine_scalar(name, source_kind, entries, get)
}

/// Combine the scalar `driver` across sibling entries that match one scope.
/// Values are trimmed before comparison, so two siblings differing only in
/// surrounding whitespace name the same program and do not conflict. An empty
/// (or whitespace-only) driver is rejected here — matching `normalize_driver` at
/// the other scopes — so `driver = ""` fails the same way at every scope rather
/// than being silently dropped. Differing non-empty values conflict, mirroring
/// [`combine_bool`].
pub(crate) fn combine_driver<T>(
    name: &str,
    source_kind: &str,
    entries: &[(&str, T)],
    get: impl Fn(&T) -> Option<&str>,
) -> color_eyre::eyre::Result<Option<String>> {
    for (expr, item) in entries {
        if get(item).is_some_and(|value| value.trim().is_empty()) {
            color_eyre::eyre::bail!("`{name}` must not be empty in {source_kind} `{expr}`");
        }
    }
    let combined = combine_scalar(name, source_kind, entries, |item| get(item).map(str::trim))?;
    Ok(combined.map(ToString::to_string))
}

#[cfg(test)]
mod tests {
    use super::{FlagConfig, ResolvedFlags};
    use std::collections::BTreeSet;

    #[test]
    fn flag_subset_macros_match_documented_special_cases() {
        macro_rules! define_master_fields {
            ($($field:ident),+ $(,)?) => {
                fn master_fields() -> BTreeSet<&'static str> {
                    [$(stringify!($field),)+].into_iter().collect()
                }
            };
        }
        macro_rules! define_simple_resolved_fields {
            ($($field:ident),+ $(,)?) => {
                fn simple_resolved_fields() -> BTreeSet<&'static str> {
                    [$(stringify!($field),)+].into_iter().collect()
                }
            };
        }

        for_each_flag_config_field!(define_master_fields);
        for_each_simple_resolved_flag_field!(define_simple_resolved_fields);

        let master = master_fields();
        let simple_resolved = simple_resolved_fields();

        let mut expected_simple_resolved = master.clone();
        expected_simple_resolved.remove("prune_implied");
        expected_simple_resolved.remove("diagnostics_only");
        expected_simple_resolved.remove("dedupe");
        expected_simple_resolved.remove("no_prune_implied");
        assert_eq!(simple_resolved, expected_simple_resolved);
    }

    #[test]
    fn flag_overlay_preserves_diagnostics_couplings() -> color_eyre::eyre::Result<()> {
        // A narrower dedupe = true rescues a broader diagnostics_only = false.
        let mut flags = FlagConfig {
            diagnostics_only: Some(false),
            ..FlagConfig::default()
        };
        flags.overlay(FlagConfig {
            dedupe: Some(true),
            ..FlagConfig::default()
        });
        let resolved = ResolvedFlags::try_from_config(flags)?;
        assert!(resolved.diagnostics_only);
        assert!(resolved.dedupe);

        // A narrower diagnostics_only = false also turns off a broader
        // dedupe = true (dedupe consumes the diagnostics-only stream).
        let mut flags = FlagConfig {
            dedupe: Some(true),
            ..FlagConfig::default()
        };
        flags.overlay(FlagConfig {
            diagnostics_only: Some(false),
            ..FlagConfig::default()
        });
        let resolved = ResolvedFlags::try_from_config(flags)?;
        assert!(!resolved.diagnostics_only);
        assert!(!resolved.dedupe);
        Ok(())
    }

    #[test]
    fn contradictory_prune_spellings_in_one_scope_error() {
        let err = FlagConfig {
            no_prune_implied: Some(true),
            prune_implied: Some(true),
            ..FlagConfig::default()
        }
        .validate()
        .expect_err("contradictory prune spelling should fail");

        assert!(err.to_string().contains("no_prune_implied"));
    }

    #[test]
    fn combine_driver_trims_whitespace_and_conflicts() -> color_eyre::eyre::Result<()> {
        let entries = [("cfg(a)", Some("cross")), ("cfg(b)", Some("cross "))];
        let out = super::combine_driver("driver", "target override", &entries, |value| *value)?;
        assert_eq!(out.as_deref(), Some("cross"));

        let entries = [
            ("cfg(a)", Some("cross")),
            ("cfg(b)", Some("cargo-zigbuild")),
        ];
        assert!(
            super::combine_driver("driver", "target override", &entries, |value| *value).is_err()
        );
        Ok(())
    }
}
