use super::env::{validate_name, validate_value};
use super::flags::FLAG_KEYS;
use super::patch::{FeatureSetVecPatch, StringSetPatch};
use super::schema::{RootConfig, ScopeConfig, WorkspaceConfig};
use super::scope::ScopeId;
use color_eyre::eyre;
use itertools::Itertools;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// The [`FeatureMatrixPatch`] field names, listed in exactly one place.
///
/// [`FeatureMatrixPatch`]: super::schema::FeatureMatrixPatch
const FEATURE_MATRIX_KEYS: &[&str] = &[
    "isolated_feature_sets",
    "mutually_exclusive_features",
    "exclude_features",
    "include_features",
    "only_features",
    "skip_optional_dependencies",
    "exclude_feature_sets",
    "include_feature_sets",
    "allow_feature_sets",
    "no_empty_feature_set",
    "matrix",
    "max_combinations",
];

const DEPRECATED_FEATURE_KEYS: &[&str] = &["skip_feature_sets", "denylist", "exact_combinations"];

const PATCH_TYPED_KEYS: &[&str] = &[
    "targets",
    "exclude_packages",
    "isolated_feature_sets",
    "mutually_exclusive_features",
    "exclude_features",
    "include_features",
    "only_features",
    "exclude_feature_sets",
    "include_feature_sets",
    "allow_feature_sets",
];

const PATCH_OP_KEYS: &[&str] = &["override", "add", "remove"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingKind {
    Flag,
    FeatureMatrix,
    DeprecatedFeature,
    ExcludePackages,
    TargetsList,
    ExpandTargets,
    Driver,
    Env,
    Inherit,
    TargetTable,
    SubcommandsTable,
}

pub(crate) fn validate_package_metadata(
    value: &serde_json::Value,
    section: &str,
) -> eyre::Result<()> {
    validate_scope(value, section, ScopeId::PackageBase)?;
    validate_target_table(value, section, ScopeId::PackageTarget)?;
    validate_subcommands(value, section, ScopeId::PackageCommand)?;
    Ok(())
}

pub(crate) fn validate_workspace_metadata(
    value: &serde_json::Value,
    section: &str,
) -> eyre::Result<()> {
    validate_scope(value, section, ScopeId::WorkspaceBase)?;
    validate_target_table(value, section, ScopeId::WorkspaceTarget)?;
    validate_subcommands(value, section, ScopeId::WorkspaceCommand)?;
    Ok(())
}

fn validate_target_table(
    value: &serde_json::Value,
    section: &str,
    target_scope: ScopeId,
) -> eyre::Result<()> {
    let Some(targets) = value.get("target").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    for (cfg_expr, target) in targets {
        let target_section = format!("{section}.target.'{cfg_expr}'");
        validate_scope(target, &target_section, target_scope)?;
        validate_subcommands(target, &target_section, target_command_scope(target_scope))?;
    }
    Ok(())
}

fn validate_subcommands(
    value: &serde_json::Value,
    section: &str,
    command_scope: ScopeId,
) -> eyre::Result<()> {
    let Some(subcommands) = value
        .get("subcommands")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(());
    };
    for (name, command) in subcommands {
        validate_scope(
            command,
            &format!("{section}.subcommands.{name}"),
            command_scope,
        )?;
    }
    Ok(())
}

fn target_command_scope(scope: ScopeId) -> ScopeId {
    match scope {
        ScopeId::WorkspaceTarget => ScopeId::WorkspaceTargetCommand,
        ScopeId::PackageTarget => ScopeId::PackageTargetCommand,
        _ => scope,
    }
}

fn validate_scope(value: &serde_json::Value, section: &str, scope: ScopeId) -> eyre::Result<()> {
    let Some(map) = value.as_object() else {
        return Ok(());
    };
    for (key, value) in map {
        let Some(kind) = setting_kind(key) else {
            return bail_unknown(key, section);
        };
        if matches!(
            kind,
            SettingKind::TargetTable | SettingKind::SubcommandsTable
        ) && valid_in(kind, scope).is_err()
        {
            bail_unknown(key, section)?;
        }
        if let Err(reason) = valid_in(kind, scope) {
            eyre::bail!("`{key}` is not valid in [{section}]: {reason}");
        }
        validate_value_shape(key, value, kind, section)?;
    }
    if scope == ScopeId::WorkspaceBase && !scope_should_inherit(map) {
        eyre::bail!(
            "`inherit = false` in [{section}] discards everything broader in the precedence chain, but the workspace base is the broadest scope, so there is nothing broader to discard"
        );
    }
    validate_non_inheriting_patch_ops(map, section)?;
    Ok(())
}

/// Whether this scope inherits everything broader in the precedence chain.
///
/// Reads the `inherit` / `replace` spelling off the raw TOML map (validation
/// runs before deserialization) and defers to the shared
/// [`super::schema::should_inherit`] rule that resolution uses, so the two never
/// disagree.
fn scope_should_inherit(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    let inherit = map.get("inherit").and_then(serde_json::Value::as_bool);
    let replace = map.get("replace") == Some(&serde_json::Value::Bool(true));
    super::schema::should_inherit(inherit, replace)
}

fn validate_value_shape(
    key: &str,
    value: &serde_json::Value,
    kind: SettingKind,
    section: &str,
) -> eyre::Result<()> {
    match value_shape(key, kind) {
        ValueShape::Bool if !value.is_boolean() => {
            eyre::bail!("`{key}` in [{section}] must be a boolean");
        }
        ValueShape::String => {
            let Some(driver) = value.as_str() else {
                eyre::bail!("`{key}` in [{section}] must be a string");
            };
            if driver.trim().is_empty() {
                eyre::bail!("`driver` must not be empty in [{section}]");
            }
        }
        ValueShape::Patch => validate_patch_shape(key, value, section)?,
        ValueShape::EnvPatch => validate_env_patch_shape(value, section)?,
        ValueShape::Array if !value.is_array() => {
            eyre::bail!("`{key}` in [{section}] must be an array");
        }
        ValueShape::Object if !value.is_object() => {
            eyre::bail!("`{key}` in [{section}] must be a table/object");
        }
        ValueShape::PositiveInteger => {
            let Some(value) = value.as_u64() else {
                eyre::bail!("`{key}` in [{section}] must be a positive integer");
            };
            if value == 0 {
                eyre::bail!("`{key}` in [{section}] must be greater than zero");
            }
        }
        ValueShape::Bool | ValueShape::Array | ValueShape::Object => {}
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ValueShape {
    Bool,
    String,
    Patch,
    EnvPatch,
    Array,
    Object,
    PositiveInteger,
}

fn value_shape(key: &str, kind: SettingKind) -> ValueShape {
    match kind {
        SettingKind::Flag | SettingKind::ExpandTargets | SettingKind::Inherit => ValueShape::Bool,
        SettingKind::Driver => ValueShape::String,
        SettingKind::Env => ValueShape::EnvPatch,
        SettingKind::FeatureMatrix => match key {
            "skip_optional_dependencies" | "no_empty_feature_set" => ValueShape::Bool,
            "matrix" => ValueShape::Object,
            "max_combinations" => ValueShape::PositiveInteger,
            _ => ValueShape::Patch,
        },
        SettingKind::DeprecatedFeature => ValueShape::Array,
        SettingKind::ExcludePackages | SettingKind::TargetsList => ValueShape::Patch,
        SettingKind::TargetTable | SettingKind::SubcommandsTable => ValueShape::Object,
    }
}

fn validate_env_patch_shape(value: &serde_json::Value, section: &str) -> eyre::Result<()> {
    let Some(map) = value.as_object() else {
        eyre::bail!("`env` in [{section}] must be a table/object");
    };

    for (op, op_value) in map {
        if !PATCH_OP_KEYS.contains(&op.as_str()) {
            eyre::bail!(
                "`env.{op}` is not an env operation in [{section}]; use `env.add.{op} = \"...\"`"
            );
        }

        if op == "remove" {
            validate_env_remove(op_value, section)?;
        } else {
            validate_env_map(op, op_value, section)?;
        }
    }

    Ok(())
}

fn validate_env_map(operation: &str, value: &serde_json::Value, section: &str) -> eyre::Result<()> {
    let Some(map) = value.as_object() else {
        eyre::bail!("`env.{operation}` in [{section}] must be a table mapping names to strings");
    };
    for (name, value) in map {
        validate_env_name(name, operation, section)?;
        let Some(value) = value.as_str() else {
            eyre::bail!("`env.{operation}.{name}` in [{section}] must be a string");
        };
        if let Err(reason) = validate_value(value) {
            eyre::bail!("`env.{operation}.{name}` in [{section}] {reason}");
        }
    }
    Ok(())
}

fn validate_env_remove(value: &serde_json::Value, section: &str) -> eyre::Result<()> {
    let Some(names) = value.as_array() else {
        eyre::bail!("`env.remove` in [{section}] must be an array of strings");
    };
    for (index, name) in names.iter().enumerate() {
        let Some(name) = name.as_str() else {
            eyre::bail!("`env.remove[{index}]` in [{section}] must be a string");
        };
        validate_env_name(name, "remove", section)?;
    }
    Ok(())
}

fn validate_env_name(name: &str, operation: &str, section: &str) -> eyre::Result<()> {
    if let Err(reason) = validate_name(name) {
        eyre::bail!("environment variable name in `env.{operation}` in [{section}] {reason}");
    }
    Ok(())
}

fn validate_patch_shape(key: &str, value: &serde_json::Value, section: &str) -> eyre::Result<()> {
    if value.is_array() {
        return Ok(());
    }

    let Some(map) = value.as_object() else {
        eyre::bail!(
            "`{key}` in [{section}] must be an array or a patch object with override/add/remove arrays"
        );
    };

    for (op, op_value) in map {
        if !PATCH_OP_KEYS.contains(&op.as_str()) {
            eyre::bail!(
                "`{key}` in [{section}] has unknown patch operation `{op}`; expected one of override, add, remove"
            );
        }
        if !op_value.is_array() {
            eyre::bail!("`{key}.{op}` in [{section}] must be an array");
        }
    }

    Ok(())
}

fn validate_non_inheriting_patch_ops(
    map: &serde_json::Map<String, serde_json::Value>,
    section: &str,
) -> eyre::Result<()> {
    if scope_should_inherit(map) {
        return Ok(());
    }

    let invalid = PATCH_TYPED_KEYS
        .iter()
        .copied()
        .filter(|key| {
            map.get(*key)
                .and_then(serde_json::Value::as_object)
                .is_some_and(patch_object_has_add_or_remove)
        })
        .collect::<Vec<_>>();

    if invalid.is_empty() {
        return Ok(());
    }

    eyre::bail!(
        "`{}` use add/remove patch operations in [{section}] with inherit = false",
        invalid.join(", ")
    );
}

fn patch_object_has_add_or_remove(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["add", "remove"].into_iter().any(|key| {
        map.get(key)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|values| !values.is_empty())
    })
}

/// Every scope of a config that may carry settings, with TOML-style locations.
fn scopes_with_locations(config: &RootConfig) -> Vec<(String, &ScopeConfig)> {
    let mut scopes = vec![(String::new(), &config.base.settings)];
    for (name, command) in &config.base.subcommands {
        scopes.push((format!("subcommands.{name}"), command));
    }
    for (expr, section) in &config.targets {
        scopes.push((format!("target.'{expr}'"), &section.settings));
        for (name, command) in &section.subcommands {
            scopes.push((format!("target.'{expr}'.subcommands.{name}"), command));
        }
    }
    scopes
}

fn string_patch_names(patch: &StringSetPatch) -> impl Iterator<Item = &String> + '_ {
    patch
        .override_value()
        .into_iter()
        .flatten()
        .chain(patch.add_values())
        .chain(patch.remove_values())
}

fn feature_set_patch_names(patch: &FeatureSetVecPatch) -> impl Iterator<Item = &String> + '_ {
    patch
        .override_value()
        .into_iter()
        .flatten()
        .chain(patch.add_values())
        .chain(patch.remove_values())
        .flatten()
}

/// Reject configured feature names that do not exist in the package.
///
/// This covers every scope statically — including `target.'cfg(...)'` sections
/// that do not match the current run — because a package's feature list does
/// not depend on the resolution target. Silent tolerance would let a typo or a
/// stale entry after a feature rename change the matrix without any signal;
/// failing at config load keeps the matrix honest and matches Cargo's own
/// strictness for `--features` (which rejects even `default` when the package
/// declares no such feature).
pub(crate) fn validate_feature_names(
    config: &RootConfig,
    package_features: &BTreeMap<String, Vec<String>>,
    package_name: &str,
    section: &str,
) -> eyre::Result<()> {
    let unknown = unknown_feature_names(config, package_features);
    if unknown.is_empty() {
        return Ok(());
    }
    let details = unknown
        .iter()
        .map(|(name, places)| format!("`{name}` ({})", places.iter().join(", ")))
        .join(", ");
    let known = if package_features.is_empty() {
        "none".to_string()
    } else {
        package_features.keys().join(", ")
    };
    eyre::bail!(
        "unknown features in [{section}] for package `{package_name}`: {details}; package features are: {known}"
    );
}

/// Configured feature names not declared by the package, keyed by name with
/// every `location.key` place each one appears.
fn unknown_feature_names<'cfg>(
    config: &'cfg RootConfig,
    package_features: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<&'cfg String, BTreeSet<String>> {
    let mut unknown: BTreeMap<&'cfg String, BTreeSet<String>> = BTreeMap::new();
    for (location, scope) in scopes_with_locations(config) {
        let mut record = |key: &str, names: &mut dyn Iterator<Item = &'cfg String>| {
            for name in names.filter(|name| !package_features.contains_key(*name)) {
                let place = if location.is_empty() {
                    key.to_string()
                } else {
                    format!("{location}.{key}")
                };
                unknown.entry(name).or_default().insert(place);
            }
        };

        macro_rules! check_string_patch {
            ($field:ident) => {
                if let Some(patch) = &scope.features.$field {
                    record(stringify!($field), &mut string_patch_names(patch));
                }
            };
        }
        macro_rules! check_feature_set_patch {
            ($field:ident) => {
                if let Some(patch) = &scope.features.$field {
                    record(stringify!($field), &mut feature_set_patch_names(patch));
                }
            };
        }
        check_string_patch!(exclude_features);
        check_string_patch!(include_features);
        check_string_patch!(only_features);
        check_feature_set_patch!(isolated_feature_sets);
        check_feature_set_patch!(mutually_exclusive_features);
        check_feature_set_patch!(exclude_feature_sets);
        check_feature_set_patch!(include_feature_sets);
        check_feature_set_patch!(allow_feature_sets);
    }
    unknown
}

/// Reject configured `exclude_packages` names that are not workspace members.
///
/// `base_exclude` carries the resolved workspace base set (including the
/// deprecated root-package spelling); the scope walk additionally covers
/// target- and command-scoped patches that are not part of the base set.
pub(crate) fn validate_exclude_package_names(
    ws_config: &WorkspaceConfig,
    base_exclude: &HashSet<String>,
    workspace_members: &BTreeSet<&str>,
    section: &str,
) -> eyre::Result<()> {
    let mut names: BTreeSet<&String> = base_exclude.iter().collect();
    for (_location, scope) in scopes_with_locations(ws_config) {
        if let Some(patch) = &scope.exclude_packages {
            names.extend(string_patch_names(patch));
        }
    }
    let unknown = names
        .into_iter()
        .filter(|name| !workspace_members.contains(name.as_str()))
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    eyre::bail!(
        "unknown packages in [{section}] exclude_packages: {}; workspace members are: {}",
        unknown.iter().map(|name| format!("`{name}`")).join(", "),
        workspace_members.iter().join(", "),
    );
}

fn bail_unknown(key: &str, section: &str) -> eyre::Result<()> {
    let hint = if key.contains('-') {
        "; cargo-fc config keys use `_`, not `-`"
    } else {
        ""
    };
    eyre::bail!("unknown cargo-fc config key `{key}` in [{section}]{hint}");
}

fn setting_kind(key: &str) -> Option<SettingKind> {
    if FLAG_KEYS.contains(&key) {
        return Some(SettingKind::Flag);
    }
    if FEATURE_MATRIX_KEYS.contains(&key) {
        return Some(SettingKind::FeatureMatrix);
    }
    if DEPRECATED_FEATURE_KEYS.contains(&key) {
        return Some(SettingKind::DeprecatedFeature);
    }
    match key {
        "exclude_packages" => Some(SettingKind::ExcludePackages),
        "targets" => Some(SettingKind::TargetsList),
        "expand_targets" => Some(SettingKind::ExpandTargets),
        "driver" => Some(SettingKind::Driver),
        "env" => Some(SettingKind::Env),
        "inherit" | "replace" => Some(SettingKind::Inherit),
        "target" => Some(SettingKind::TargetTable),
        "subcommands" => Some(SettingKind::SubcommandsTable),
        _ => None,
    }
}

fn valid_in(kind: SettingKind, scope: ScopeId) -> Result<(), &'static str> {
    match kind {
        SettingKind::Flag | SettingKind::Driver | SettingKind::Env | SettingKind::Inherit => Ok(()),
        SettingKind::ExpandTargets if scope.is_command() => Ok(()),
        SettingKind::ExpandTargets => Err(
            "`expand_targets` is a per-subcommand capability; set it inside a `subcommands.<cmd>` table",
        ),
        SettingKind::TargetsList
            if matches!(
                scope,
                ScopeId::WorkspaceBase
                    | ScopeId::WorkspaceCommand
                    | ScopeId::PackageBase
                    | ScopeId::PackageCommand
            ) =>
        {
            Ok(())
        }
        SettingKind::TargetsList => Err(
            "a `targets` list is not valid anywhere inside a `target.'cfg(...)'` section (that section was already selected by a target match); set it at a base scope or a base (non-target-nested) `subcommands.<cmd>` table instead",
        ),
        SettingKind::ExcludePackages
            if matches!(
                scope,
                ScopeId::WorkspaceBase
                    | ScopeId::WorkspaceCommand
                    | ScopeId::WorkspaceTarget
                    | ScopeId::WorkspaceTargetCommand
                    | ScopeId::PackageBase
            ) =>
        {
            Ok(())
        }
        SettingKind::ExcludePackages => Err(
            "`exclude_packages` selects which workspace members run and is only valid in workspace scope",
        ),
        SettingKind::FeatureMatrix
            if matches!(
                scope,
                ScopeId::PackageBase
                    | ScopeId::PackageCommand
                    | ScopeId::PackageTarget
                    | ScopeId::PackageTargetCommand
            ) =>
        {
            Ok(())
        }
        SettingKind::DeprecatedFeature if scope == ScopeId::PackageBase => Ok(()),
        SettingKind::DeprecatedFeature => Err(
            "deprecated feature-matrix keys are accepted only at package base scope; use `exclude_feature_sets`, `exclude_features`, or `include_feature_sets` in target/subcommand scopes",
        ),
        SettingKind::FeatureMatrix => {
            Err("feature-matrix settings are per-package and are not valid in workspace scope")
        }
        SettingKind::TargetTable
            if matches!(scope, ScopeId::WorkspaceBase | ScopeId::PackageBase) =>
        {
            Ok(())
        }
        SettingKind::SubcommandsTable
            if matches!(
                scope,
                ScopeId::WorkspaceBase
                    | ScopeId::WorkspaceTarget
                    | ScopeId::PackageBase
                    | ScopeId::PackageTarget
            ) =>
        {
            Ok(())
        }
        SettingKind::TargetTable | SettingKind::SubcommandsTable => Err(""),
    }
}

#[cfg(test)]
mod tests {
    use super::{SettingKind, setting_kind, valid_in};
    use crate::config::scope::ScopeId;
    use serde::Serialize;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    fn features(names: &[&str]) -> BTreeMap<String, Vec<String>> {
        names
            .iter()
            .map(|name| ((*name).to_string(), Vec::new()))
            .collect()
    }

    #[test]
    fn feature_names_accept_known_names_in_all_scopes() {
        let config: crate::config::Config = serde_json::from_value(serde_json::json!({
            "exclude_features": ["default"],
            "mutually_exclusive_features": [["cuda", "coreml"]],
            "subcommands": {
                "test": { "only_features": { "add": ["cuda"] } },
            },
            "target": {
                "cfg(unix)": {
                    "include_features": { "remove": ["coreml"] },
                    "subcommands": {
                        "check": { "exclude_feature_sets": [["cuda", "coreml"]] },
                    },
                },
            },
        }))
        .expect("deserialize config");

        super::validate_feature_names(
            &config,
            &features(&["default", "cuda", "coreml"]),
            "pkg",
            "package.metadata.cargo-fc",
        )
        .expect("all referenced names are declared features");
    }

    #[test]
    fn feature_names_reject_unknown_names_with_locations() {
        let config: crate::config::Config = serde_json::from_value(serde_json::json!({
            "exclude_features": ["typo-a"],
            "target": {
                "cfg(windows)": {
                    "mutually_exclusive_features": [["cuda", "typo-b"]],
                },
            },
        }))
        .expect("deserialize config");

        let err = super::validate_feature_names(
            &config,
            &features(&["cuda"]),
            "pkg",
            "package.metadata.cargo-fc",
        )
        .expect_err("unknown names should fail, even in non-matching target sections");
        let message = err.to_string();

        assert!(message.contains("`typo-a` (exclude_features)"), "{message}");
        assert!(
            message.contains("`typo-b` (target.'cfg(windows)'.mutually_exclusive_features)"),
            "{message}"
        );
        assert!(message.contains("package features are: cuda"), "{message}");
    }

    #[test]
    fn feature_names_reject_undeclared_default() {
        // Cargo itself rejects `--features default` when the package declares
        // no `default` feature, so cargo-fc is deliberately just as strict.
        let config: crate::config::Config = serde_json::from_value(serde_json::json!({
            "exclude_features": ["default"],
        }))
        .expect("deserialize config");

        let err = super::validate_feature_names(
            &config,
            &features(&["cuda"]),
            "pkg",
            "package.metadata.cargo-fc",
        )
        .expect_err("undeclared default is not exempt");

        assert!(err.to_string().contains("`default`"), "{err}");
    }

    #[test]
    fn feature_name_validation_covers_every_patch_typed_feature_key() {
        // Guards `unknown_feature_names()` against drift: a key registered in
        // the schema key lists but missing from the walk would silently skip
        // validation for that key.
        for key in super::FEATURE_MATRIX_KEYS
            .iter()
            .filter(|key| super::PATCH_TYPED_KEYS.contains(*key))
        {
            // A key holds either feature names or feature sets; exactly one of
            // the two shapes deserializes.
            let config = [serde_json::json!(["ghost"]), serde_json::json!([["ghost"]])]
                .into_iter()
                .find_map(|shape| {
                    let mut map = serde_json::Map::new();
                    map.insert((*key).to_string(), shape);
                    serde_json::from_value::<crate::config::Config>(serde_json::Value::Object(map))
                        .ok()
                })
                .unwrap_or_else(|| panic!("`{key}` accepts neither name-list shape"));

            let Err(err) = super::validate_feature_names(
                &config,
                &features(&["real"]),
                "pkg",
                "package.metadata.cargo-fc",
            ) else {
                panic!(
                    "`{key}` referencing unknown feature `ghost` passed validation; add the key to unknown_feature_names()"
                );
            };
            let message = err.to_string();
            assert!(message.contains(&format!("({key})")), "{key}: {message}");
        }
    }

    #[test]
    fn feature_names_check_remove_operations() {
        let config: crate::config::Config = serde_json::from_value(serde_json::json!({
            "exclude_features": { "remove": ["typo"] },
        }))
        .expect("deserialize config");

        let err = super::validate_feature_names(
            &config,
            &features(&["cuda"]),
            "pkg",
            "package.metadata.cargo-fc",
        )
        .expect_err("remove operations reference feature names too");

        assert!(err.to_string().contains("`typo`"), "{err}");
    }

    #[test]
    fn exclude_package_names_reject_unknown_members() {
        let ws_config: crate::config::WorkspaceConfig = serde_json::from_value(serde_json::json!({
            "target": {
                "cfg(unix)": { "exclude_packages": { "add": ["ghost"] } },
            },
        }))
        .expect("deserialize workspace config");
        let base_exclude = HashSet::from(["ghost-base".to_string()]);
        let members = BTreeSet::from(["member-a"]);

        let err = super::validate_exclude_package_names(
            &ws_config,
            &base_exclude,
            &members,
            "workspace.metadata.cargo-fc",
        )
        .expect_err("unknown packages should fail");
        let message = err.to_string();

        assert!(message.contains("`ghost`"), "{message}");
        assert!(message.contains("`ghost-base`"), "{message}");
        assert!(
            message.contains("workspace members are: member-a"),
            "{message}"
        );
    }

    #[test]
    fn exclude_package_names_accept_members() {
        let ws_config: crate::config::WorkspaceConfig = serde_json::from_value(serde_json::json!({
            "exclude_packages": ["member-b"],
            "target": {
                "cfg(unix)": { "exclude_packages": { "remove": ["member-b"] } },
            },
        }))
        .expect("deserialize workspace config");
        let base_exclude = HashSet::from(["member-b".to_string()]);
        let members = BTreeSet::from(["member-a", "member-b"]);

        super::validate_exclude_package_names(
            &ws_config,
            &base_exclude,
            &members,
            "workspace.metadata.cargo-fc",
        )
        .expect("workspace members are valid excludes");
    }

    fn scope_ids() -> [ScopeId; 8] {
        [
            ScopeId::WorkspaceBase,
            ScopeId::WorkspaceCommand,
            ScopeId::WorkspaceTarget,
            ScopeId::WorkspaceTargetCommand,
            ScopeId::PackageBase,
            ScopeId::PackageCommand,
            ScopeId::PackageTarget,
            ScopeId::PackageTargetCommand,
        ]
    }

    #[test]
    fn every_known_key_has_a_setting_kind() {
        for key in crate::config::FLAG_KEYS
            .iter()
            .chain(super::FEATURE_MATRIX_KEYS)
            .chain(super::DEPRECATED_FEATURE_KEYS)
            .chain(
                [
                    "exclude_packages",
                    "targets",
                    "expand_targets",
                    "driver",
                    "env",
                    "inherit",
                    "replace",
                    "target",
                    "subcommands",
                ]
                .iter(),
            )
        {
            assert!(setting_kind(key).is_some(), "missing kind for {key}");
        }
    }

    #[test]
    fn feature_matrix_key_list_matches_schema() {
        let schema_keys = keys_for(crate::config::FeatureMatrixPatch::default());
        let validation_keys = super::FEATURE_MATRIX_KEYS
            .iter()
            .map(|key| (*key).to_string())
            .collect::<BTreeSet<_>>();

        assert_eq!(validation_keys, schema_keys);
    }

    #[test]
    fn every_setting_kind_is_valid_somewhere() {
        for kind in [
            SettingKind::Flag,
            SettingKind::FeatureMatrix,
            SettingKind::DeprecatedFeature,
            SettingKind::ExcludePackages,
            SettingKind::TargetsList,
            SettingKind::ExpandTargets,
            SettingKind::Driver,
            SettingKind::Env,
            SettingKind::Inherit,
            SettingKind::TargetTable,
            SettingKind::SubcommandsTable,
        ] {
            assert!(
                scope_ids()
                    .into_iter()
                    .any(|scope| valid_in(kind, scope).is_ok()),
                "{kind:?} is never valid",
            );
        }
    }

    #[test]
    fn package_metadata_rejects_unknown_flag_keys() {
        let err = super::validate_package_metadata(
            &serde_json::json!({ "fail-fast": true }),
            "package.metadata.cargo-fc",
        )
        .expect_err("hyphenated cargo-fc flag should fail");

        assert!(err.to_string().contains("fail-fast"));
        assert!(err.to_string().contains("use `_`, not `-`"));
    }

    #[test]
    fn package_subcommand_accepts_feature_matrix_keys() {
        super::validate_package_metadata(
            &serde_json::json!({
                "subcommands": { "test": { "exclude_features": ["gpu"] } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("package subcommand should accept feature-matrix keys");
    }

    #[test]
    fn package_target_subcommand_accepts_feature_matrix_keys() {
        super::validate_package_metadata(
            &serde_json::json!({
                "target": {
                    "cfg(unix)": {
                        "subcommands": { "test": { "only_features": ["a"] } },
                    },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("package target subcommand should accept feature-matrix keys");
    }

    #[test]
    fn mutually_exclusive_features_accepts_patch_shape_in_package_scopes() {
        super::validate_package_metadata(
            &serde_json::json!({
                "mutually_exclusive_features": [["cuda", "coreml"]],
                "target": {
                    "cfg(unix)": {
                        "mutually_exclusive_features": {
                            "add": [["openssl", "rustls"]],
                        },
                    },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("feature groups should accept feature-set patch syntax");
    }

    #[test]
    fn mutually_exclusive_features_rejects_invalid_patch_shape() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "mutually_exclusive_features": { "add": "cuda" },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("feature group patch operations must contain arrays");

        assert!(err.to_string().contains("mutually_exclusive_features"));
        assert!(err.to_string().contains("must be an array"));
    }

    #[test]
    fn workspace_rejects_mutually_exclusive_features() {
        let err = super::validate_workspace_metadata(
            &serde_json::json!({
                "mutually_exclusive_features": [["cuda", "coreml"]],
            }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("feature groups are package-scoped");

        assert!(err.to_string().contains("per-package"));
    }

    #[test]
    fn workspace_base_rejects_inherit_false() {
        let err = super::validate_workspace_metadata(
            &serde_json::json!({ "inherit": false }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace base should reject inherit = false");
        assert!(err.to_string().contains("nothing broader to discard"));

        // The deprecated `replace = true` alias hits the same rule.
        let err = super::validate_workspace_metadata(
            &serde_json::json!({ "replace": true }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace base should reject the legacy replace alias");
        assert!(err.to_string().contains("nothing broader to discard"));
    }

    #[test]
    fn workspace_base_accepts_redundant_inherit_true() {
        super::validate_workspace_metadata(
            &serde_json::json!({ "inherit": true }),
            "workspace.metadata.cargo-fc",
        )
        .expect("inherit = true at the workspace base is a harmless no-op");
    }

    #[test]
    fn package_base_and_subcommands_accept_inherit_false() {
        super::validate_package_metadata(
            &serde_json::json!({
                "inherit": false,
                "subcommands": { "test": { "inherit": false } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("package base and subcommands should accept inherit = false");

        // The deprecated `replace = true` alias is still parsed and accepted.
        super::validate_package_metadata(
            &serde_json::json!({
                "replace": true,
                "subcommands": { "test": { "replace": true } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("package base and subcommands should accept the legacy replace alias");
    }

    #[test]
    fn workspace_subcommand_accepts_exclude_packages() {
        super::validate_workspace_metadata(
            &serde_json::json!({
                "subcommands": { "test": { "exclude_packages": { "add": ["foo"] } } },
            }),
            "workspace.metadata.cargo-fc",
        )
        .expect("workspace subcommand should accept exclude_packages");
    }

    #[test]
    fn package_subcommand_rejects_exclude_packages() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "subcommands": { "test": { "exclude_packages": ["foo"] } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("package subcommand should reject exclude_packages");
        assert!(err.to_string().contains("workspace scope"));
    }

    #[test]
    fn workspace_subcommand_rejects_feature_matrix_keys() {
        let err = super::validate_workspace_metadata(
            &serde_json::json!({
                "subcommands": { "test": { "exclude_features": ["gpu"] } },
            }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace subcommand should reject per-package feature-matrix keys");

        assert!(err.to_string().contains("per-package"));
    }

    #[test]
    fn package_metadata_rejects_wrong_value_shapes_with_key_context() {
        let err = super::validate_package_metadata(
            &serde_json::json!({ "exclude_features": "gpu" }),
            "package.metadata.cargo-fc",
        )
        .expect_err("patch field should reject string value");
        assert!(err.to_string().contains("exclude_features"), "{err}");
        assert!(err.to_string().contains("must be an array"), "{err}");

        let err = super::validate_package_metadata(
            &serde_json::json!({ "pedantic": "true" }),
            "package.metadata.cargo-fc",
        )
        .expect_err("flag field should reject string value");
        assert!(err.to_string().contains("pedantic"), "{err}");
        assert!(err.to_string().contains("boolean"), "{err}");

        let err = super::validate_package_metadata(
            &serde_json::json!({ "exclude_features": { "append": ["gpu"] } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("patch field should reject unknown operation");
        assert!(err.to_string().contains("append"), "{err}");
    }

    #[test]
    fn deprecated_feature_keys_have_specific_scope_error() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "target": { "cfg(unix)": { "denylist": ["gpu"] } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("deprecated feature key should fail outside package base");

        let message = err.to_string();
        assert!(message.contains("deprecated"), "{message}");
        assert!(message.contains("exclude_features"), "{message}");
    }

    #[test]
    fn misplaced_known_keys_get_scope_aware_reasons() {
        let err = super::validate_workspace_metadata(
            &serde_json::json!({ "exclude_features": ["gpu"] }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace base should reject feature-matrix keys");
        assert!(err.to_string().contains("per-package"), "{err}");

        let err = super::validate_package_metadata(
            &serde_json::json!({ "subcommands": { "test": { "exclude_packages": ["foo"] } } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("package subcommand should reject exclude_packages");
        assert!(err.to_string().contains("workspace scope"), "{err}");

        let err = super::validate_package_metadata(
            &serde_json::json!({ "target": { "cfg(unix)": { "targets": ["x"] } } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("target section should reject a targets list");
        assert!(
            err.to_string().contains("not valid anywhere inside"),
            "{err}"
        );

        let err = super::validate_package_metadata(
            &serde_json::json!({ "expand_targets": true }),
            "package.metadata.cargo-fc",
        )
        .expect_err("package base should reject expand_targets");
        assert!(
            err.to_string().contains("per-subcommand capability"),
            "{err}"
        );
    }

    #[test]
    fn targets_list_rejected_in_target_nested_subcommand() {
        super::validate_package_metadata(
            &serde_json::json!({ "subcommands": { "test": { "targets": ["x"] } } }),
            "package.metadata.cargo-fc",
        )
        .expect("base subcommand accepts a targets list");

        let err = super::validate_package_metadata(
            &serde_json::json!({
                "target": { "cfg(unix)": { "subcommands": { "test": { "targets": ["x"] } } } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("target-nested subcommand should reject a targets list");
        assert!(err.to_string().contains("targets"), "{err}");

        super::validate_workspace_metadata(
            &serde_json::json!({ "subcommands": { "test": { "targets": ["x"] } } }),
            "workspace.metadata.cargo-fc",
        )
        .expect("workspace base subcommand accepts a targets list");
        let err = super::validate_workspace_metadata(
            &serde_json::json!({
                "target": { "cfg(unix)": { "subcommands": { "test": { "targets": ["x"] } } } },
            }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace target-nested subcommand should reject a targets list");
        assert!(err.to_string().contains("targets"), "{err}");
    }

    #[test]
    fn driver_is_accepted_in_every_scope() {
        super::validate_workspace_metadata(
            &serde_json::json!({
                "driver": "cargo-zigbuild",
                "target": {
                    "cfg(unix)": {
                        "driver": "cross",
                        "subcommands": { "test": { "driver": "cargo" } },
                    },
                },
                "subcommands": { "test": { "driver": "cargo" } },
            }),
            "workspace.metadata.cargo-fc",
        )
        .expect("workspace scopes should accept driver everywhere");

        super::validate_package_metadata(
            &serde_json::json!({
                "driver": "cargo-zigbuild",
                "target": {
                    "cfg(unix)": {
                        "driver": "cross",
                        "subcommands": { "test": { "driver": "cargo" } },
                    },
                },
                "subcommands": { "test": { "driver": "cargo" } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("package scopes should accept driver everywhere");
    }

    #[test]
    fn env_is_accepted_in_every_scope() {
        let config = serde_json::json!({
            "env": { "add": { "BASE": "base" } },
            "target": {
                "cfg(unix)": {
                    "env": { "remove": ["TARGET"] },
                    "subcommands": {
                        "test": { "env": { "override": { "TARGET_COMMAND": "set" } } },
                    },
                },
            },
            "subcommands": {
                "test": { "env": { "add": { "COMMAND": "set" } } },
            },
        });

        super::validate_workspace_metadata(&config, "workspace.metadata.cargo-fc")
            .expect("workspace scopes should accept env everywhere");
        super::validate_package_metadata(&config, "package.metadata.cargo-fc")
            .expect("package scopes should accept env everywhere");
    }

    #[test]
    fn env_rejects_bare_map_with_operation_hint() {
        let err = super::validate_package_metadata(
            &serde_json::json!({ "env": { "RUST_BACKTRACE": "1" } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("a bare environment map is ambiguous");
        let message = err.to_string();

        assert!(
            message.contains("`env.RUST_BACKTRACE` is not an env operation"),
            "{message}"
        );
        assert!(
            message.contains("`env.add.RUST_BACKTRACE = \"...\"`"),
            "{message}"
        );
    }

    #[test]
    fn env_rejects_non_string_values_and_remove_entries() {
        let add_err = super::validate_package_metadata(
            &serde_json::json!({ "env": { "add": { "COUNT": 3 } } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("environment additions must contain strings");
        assert!(
            add_err.to_string().contains("`env.add.COUNT`")
                && add_err.to_string().contains("must be a string"),
            "{add_err}"
        );

        let remove_err = super::validate_package_metadata(
            &serde_json::json!({ "env": { "remove": [true] } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("environment removals must contain strings");
        assert!(
            remove_err.to_string().contains("`env.remove[0]`")
                && remove_err.to_string().contains("must be a string"),
            "{remove_err}"
        );
    }

    #[test]
    fn env_rejects_invalid_names_and_nul_values() {
        for name in ["", "BAD=NAME", "BAD\0NAME"] {
            let err = super::validate_package_metadata(
                &serde_json::json!({ "env": { "add": { name: "value" } } }),
                "package.metadata.cargo-fc",
            )
            .expect_err("invalid environment variable name should fail");
            assert!(
                err.to_string().contains("environment variable name"),
                "{err}"
            );
        }

        let err = super::validate_package_metadata(
            &serde_json::json!({ "env": { "add": { "VALID": "bad\0value" } } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("NUL in environment variable value should fail");
        assert!(err.to_string().contains("`env.add.VALID`"), "{err}");
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    #[test]
    fn empty_driver_is_rejected_in_every_scope() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "target": { "cfg(unix)": { "subcommands": { "test": { "driver": " " } } } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("empty driver should fail");
        assert!(err.to_string().contains("driver"));
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn inherit_false_rejects_add_remove_in_same_package_section() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "inherit": false,
                "exclude_features": { "add": ["gpu"], "remove": ["cpu"] },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("inherit = false with add/remove in the same section should fail");

        let message = err.to_string();
        assert!(message.contains("inherit = false"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("exclude_features"), "{message}");
    }

    #[test]
    fn legacy_replace_true_rejects_add_remove_in_same_workspace_section() {
        let err = super::validate_workspace_metadata(
            &serde_json::json!({
                "subcommands": {
                    "test": {
                        "replace": true,
                        "exclude_packages": { "remove": ["native"] },
                    },
                },
            }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace command replace with add/remove should fail");

        let message = err.to_string();
        assert!(message.contains("inherit = false"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("exclude_packages"), "{message}");
        assert!(
            message.contains("workspace.metadata.cargo-fc.subcommands.test"),
            "{message}"
        );
    }

    #[test]
    fn broader_non_inheriting_add_remove_errors_even_when_narrower_also_opts_out() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "inherit": false,
                "exclude_features": { "add": ["base"] },
                "target": {
                    "cfg(unix)": { "inherit": false },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("broader invalid non-inheriting section should fail at load time");

        let message = err.to_string();
        assert!(message.contains("inherit = false"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("package.metadata.cargo-fc"), "{message}");
    }

    #[test]
    fn unmatched_target_non_inheriting_add_remove_errors_at_load_time() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "target": {
                    "cfg(target_os = \"definitely-not-this-target\")": {
                        "inherit": false,
                        "exclude_features": { "add": ["gpu"] },
                    },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("invalid target section should fail without cfg evaluation");

        let message = err.to_string();
        assert!(message.contains("inherit = false"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("target.'cfg(target_os = \"definitely-not-this-target\")'"));
    }

    #[test]
    fn non_inheriting_rule_is_section_local_for_nested_subcommands() {
        super::validate_package_metadata(
            &serde_json::json!({
                "inherit": false,
                "subcommands": {
                    "test": { "exclude_features": { "add": ["gpu"] } },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("parent non-inheriting section should not constrain nested subcommand patches");

        super::validate_package_metadata(
            &serde_json::json!({
                "exclude_features": { "add": ["gpu"] },
                "subcommands": {
                    "test": { "inherit": false },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("subcommand non-inheriting section should not constrain parent patches");
    }

    fn keys_for<T: Serialize>(value: T) -> BTreeSet<String> {
        match serde_json::to_value(value).expect("serialize default") {
            Value::Object(map) => map.keys().cloned().collect(),
            other => panic!("expected object, got {other:?}"),
        }
    }
}
