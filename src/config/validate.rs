use super::flags::FLAG_KEYS;
use super::scope::ScopeId;
use color_eyre::eyre;

/// The [`FeatureMatrixPatch`] field names, listed in exactly one place.
///
/// [`FeatureMatrixPatch`]: super::schema::FeatureMatrixPatch
const FEATURE_MATRIX_KEYS: &[&str] = &[
    "isolated_feature_sets",
    "exclude_features",
    "include_features",
    "only_features",
    "skip_optional_dependencies",
    "exclude_feature_sets",
    "include_feature_sets",
    "allow_feature_sets",
    "no_empty_feature_set",
    "matrix",
];

const DEPRECATED_FEATURE_KEYS: &[&str] = &["skip_feature_sets", "denylist", "exact_combinations"];

const PATCH_TYPED_KEYS: &[&str] = &[
    "targets",
    "exclude_packages",
    "isolated_feature_sets",
    "exclude_features",
    "include_features",
    "only_features",
    "exclude_feature_sets",
    "include_feature_sets",
    "allow_feature_sets",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingKind {
    Flag,
    FeatureMatrix,
    DeprecatedFeature,
    ExcludePackages,
    TargetsList,
    ExpandTargets,
    Driver,
    Replace,
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
            bail_unknown(key, section)?;
            continue;
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
        if kind == SettingKind::Driver
            && value
                .as_str()
                .is_some_and(|driver| driver.trim().is_empty())
        {
            eyre::bail!("`driver` must not be empty in [{section}]");
        }
    }
    validate_replace_patch_ops(map, section)?;
    Ok(())
}

fn validate_replace_patch_ops(
    map: &serde_json::Map<String, serde_json::Value>,
    section: &str,
) -> eyre::Result<()> {
    if map.get("replace") != Some(&serde_json::Value::Bool(true)) {
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
        "`{}` use add/remove patch operations in [{section}] with replace = true",
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
        "replace" => Some(SettingKind::Replace),
        "target" => Some(SettingKind::TargetTable),
        "subcommands" => Some(SettingKind::SubcommandsTable),
        _ => None,
    }
}

fn valid_in(kind: SettingKind, scope: ScopeId) -> Result<(), &'static str> {
    match kind {
        SettingKind::Flag | SettingKind::Driver => Ok(()),
        SettingKind::Replace if scope != ScopeId::WorkspaceBase => Ok(()),
        SettingKind::Replace => Err(
            "`replace` resets everything broader in the precedence chain, but the workspace base is the broadest scope, so there is nothing for it to reset",
        ),
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
        SettingKind::FeatureMatrix | SettingKind::DeprecatedFeature => {
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
    fn every_setting_kind_is_valid_somewhere() {
        for kind in [
            SettingKind::Flag,
            SettingKind::FeatureMatrix,
            SettingKind::DeprecatedFeature,
            SettingKind::ExcludePackages,
            SettingKind::TargetsList,
            SettingKind::ExpandTargets,
            SettingKind::Driver,
            SettingKind::Replace,
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
    fn workspace_base_rejects_replace() {
        let err = super::validate_workspace_metadata(
            &serde_json::json!({ "replace": true }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace base should reject replace");
        assert!(err.to_string().contains("nothing for it to reset"));
    }

    #[test]
    fn package_base_and_subcommands_accept_replace() {
        super::validate_package_metadata(
            &serde_json::json!({
                "replace": true,
                "subcommands": { "test": { "replace": true } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("package base and subcommands should accept replace");
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
    fn replace_rejects_add_remove_in_same_package_section() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "replace": true,
                "exclude_features": { "add": ["gpu"], "remove": ["cpu"] },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("replace with add/remove in the same section should fail");

        let message = err.to_string();
        assert!(message.contains("replace"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("exclude_features"), "{message}");
    }

    #[test]
    fn replace_rejects_add_remove_in_same_workspace_section() {
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
        assert!(message.contains("replace"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("exclude_packages"), "{message}");
        assert!(
            message.contains("workspace.metadata.cargo-fc.subcommands.test"),
            "{message}"
        );
    }

    #[test]
    fn broader_replace_add_remove_errors_even_when_narrower_reset_exists() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "replace": true,
                "exclude_features": { "add": ["base"] },
                "target": {
                    "cfg(unix)": { "replace": true },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("broader invalid reset should fail at load time");

        let message = err.to_string();
        assert!(message.contains("replace"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("package.metadata.cargo-fc"), "{message}");
    }

    #[test]
    fn unmatched_target_replace_add_remove_errors_at_load_time() {
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "target": {
                    "cfg(target_os = \"definitely-not-this-target\")": {
                        "replace": true,
                        "exclude_features": { "add": ["gpu"] },
                    },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("invalid target section should fail without cfg evaluation");

        let message = err.to_string();
        assert!(message.contains("replace"), "{message}");
        assert!(message.contains("add/remove"), "{message}");
        assert!(message.contains("target.'cfg(target_os = \"definitely-not-this-target\")'"));
    }

    #[test]
    fn replace_rule_is_section_local_for_nested_subcommands() {
        super::validate_package_metadata(
            &serde_json::json!({
                "replace": true,
                "subcommands": {
                    "test": { "exclude_features": { "add": ["gpu"] } },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("parent replace should not constrain nested subcommand patches");

        super::validate_package_metadata(
            &serde_json::json!({
                "exclude_features": { "add": ["gpu"] },
                "subcommands": {
                    "test": { "replace": true },
                },
            }),
            "package.metadata.cargo-fc",
        )
        .expect("subcommand replace should not constrain parent patches");
    }
}
