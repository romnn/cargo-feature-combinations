use super::flags::FLAG_KEYS;
use color_eyre::eyre;

const PACKAGE_KEYS: &[&str] = &[
    "targets",
    "isolated_feature_sets",
    "exclude_features",
    "include_features",
    "only_features",
    "skip_optional_dependencies",
    "exclude_packages",
    "exclude_feature_sets",
    "include_feature_sets",
    "allow_feature_sets",
    "no_empty_feature_set",
    "matrix",
    "subcommands",
    "target",
    "skip_feature_sets",
    "denylist",
    "exact_combinations",
];

const PACKAGE_TARGET_KEYS: &[&str] = &[
    "replace",
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
    "subcommands",
];

const WORKSPACE_KEYS: &[&str] = &[
    "exclude_packages",
    "targets",
    "target",
    "subcommands",
    "driver",
];

const WORKSPACE_TARGET_KEYS: &[&str] = &["exclude_packages", "subcommands"];

const COMMAND_KEYS: &[&str] = &["targets"];

pub(crate) fn validate_package_metadata(
    value: &serde_json::Value,
    section: &str,
) -> eyre::Result<()> {
    validate_keys(value, section, PACKAGE_KEYS, FLAG_KEYS)?;
    validate_target_table(value, section, PACKAGE_TARGET_KEYS)?;
    validate_subcommands(value, section)?;
    Ok(())
}

pub(crate) fn validate_workspace_metadata(
    value: &serde_json::Value,
    section: &str,
) -> eyre::Result<()> {
    validate_keys(value, section, WORKSPACE_KEYS, FLAG_KEYS)?;
    validate_target_table(value, section, WORKSPACE_TARGET_KEYS)?;
    validate_subcommands(value, section)?;
    Ok(())
}

fn validate_target_table(
    value: &serde_json::Value,
    section: &str,
    allowed: &[&str],
) -> eyre::Result<()> {
    let Some(targets) = value.get("target").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    for (cfg_expr, target) in targets {
        let target_section = format!("{section}.target.'{cfg_expr}'");
        validate_keys(target, &target_section, allowed, FLAG_KEYS)?;
        validate_subcommands(target, &target_section)?;
    }
    Ok(())
}

fn validate_subcommands(value: &serde_json::Value, section: &str) -> eyre::Result<()> {
    let Some(subcommands) = value
        .get("subcommands")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(());
    };
    for (name, command) in subcommands {
        validate_keys(
            command,
            &format!("{section}.subcommands.{name}"),
            COMMAND_KEYS,
            FLAG_KEYS,
        )?;
    }
    Ok(())
}

fn validate_keys(
    value: &serde_json::Value,
    section: &str,
    allowed: &[&str],
    allowed_flags: &[&str],
) -> eyre::Result<()> {
    let Some(map) = value.as_object() else {
        return Ok(());
    };
    for key in map.keys() {
        if allowed.contains(&key.as_str()) || allowed_flags.contains(&key.as_str()) {
            continue;
        }
        let hint = if key.contains('-') {
            "; cargo-fc config keys use `_`, not `-`"
        } else {
            ""
        };
        eyre::bail!("unknown cargo-fc config key `{key}` in [{section}]{hint}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_KEYS, FLAG_KEYS, PACKAGE_KEYS, PACKAGE_TARGET_KEYS, WORKSPACE_KEYS,
        WORKSPACE_TARGET_KEYS,
    };
    use crate::config::{
        CommandCapabilities, Config, TargetOverride, WorkspaceConfig, WorkspaceTargetOverride,
    };
    use std::collections::BTreeSet;

    fn serialized_keys<T: Default + serde::Serialize>() -> BTreeSet<String> {
        serde_json::to_value(T::default())
            .expect("default config should serialize")
            .as_object()
            .expect("config should serialize to an object")
            .keys()
            .cloned()
            .collect()
    }

    fn allowed_keys(primary: &[&str]) -> BTreeSet<String> {
        primary
            .iter()
            .chain(FLAG_KEYS)
            .copied()
            .map(String::from)
            .collect()
    }

    fn assert_allowlist_matches_serialized_keys<T: Default + serde::Serialize>(
        name: &str,
        primary: &[&str],
    ) {
        let actual = serialized_keys::<T>();
        let allowed = allowed_keys(primary);
        let missing: Vec<_> = actual.difference(&allowed).cloned().collect();
        let extra: Vec<_> = allowed
            .difference(&actual)
            .filter(|key| key.as_str() != "dedup")
            .cloned()
            .collect();

        assert!(
            missing.is_empty(),
            "{name} serialized keys missing from validator allowlist: {missing:?}",
        );
        assert!(
            extra.is_empty(),
            "{name} validator allowlist accepts keys not produced by serde: {extra:?}",
        );
    }

    #[test]
    fn validator_allowlists_match_serde_field_names() {
        assert_allowlist_matches_serialized_keys::<Config>("package config", PACKAGE_KEYS);
        assert_allowlist_matches_serialized_keys::<TargetOverride>(
            "package target override",
            PACKAGE_TARGET_KEYS,
        );
        assert_allowlist_matches_serialized_keys::<WorkspaceConfig>(
            "workspace config",
            WORKSPACE_KEYS,
        );
        assert_allowlist_matches_serialized_keys::<WorkspaceTargetOverride>(
            "workspace target override",
            WORKSPACE_TARGET_KEYS,
        );
        assert_allowlist_matches_serialized_keys::<CommandCapabilities>(
            "command capabilities",
            COMMAND_KEYS,
        );
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
}
