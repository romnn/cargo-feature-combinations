use super::flags::FLAG_KEYS;
use color_eyre::eyre;

/// The [`FeatureMatrixPatch`] field names, listed in exactly one place. Any
/// scope that carries a feature-matrix patch accepts these in addition to its
/// own keys, so adding a field to `FeatureMatrixPatch` only requires updating
/// this const (the `validator_allowlists_match_serde_field_names` test enforces
/// it stays in sync with the struct).
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

/// Keys accepted at *every* scope (the `driver` build-driver override is `✓` in
/// every matrix cell), so — like [`FLAG_KEYS`] — they are checked once in
/// [`validate_keys`] instead of being repeated in each per-scope allowlist.
const UNIVERSAL_KEYS: &[&str] = &["driver"];

const PACKAGE_KEYS: &[&str] = &[
    "replace",
    "targets",
    "exclude_packages",
    "subcommands",
    "target",
    "skip_feature_sets",
    "denylist",
    "exact_combinations",
];

const PACKAGE_TARGET_KEYS: &[&str] = &["replace", "subcommands"];

/// Workspace base is the broadest layer, so `replace` (which resets everything
/// broader) is intentionally absent here — there is nothing for it to reset.
const WORKSPACE_KEYS: &[&str] = &["exclude_packages", "targets", "target", "subcommands"];

const WORKSPACE_TARGET_KEYS: &[&str] = &["replace", "exclude_packages", "subcommands"];

/// Package-scope subcommand tables also accept the feature-matrix keys (via
/// [`FEATURE_MATRIX_KEYS`]), so a subcommand may vary the feature matrix
/// (e.g. `subcommands.test.exclude_features`).
const PACKAGE_COMMAND_KEYS: &[&str] = &["replace", "expand_targets", "targets"];

/// Workspace-scope subcommand tables accept the package-selection key
/// `exclude_packages` (per-command, e.g. "skip `foo` when testing") but never
/// [`FEATURE_MATRIX_KEYS`], which are per-package.
const WORKSPACE_COMMAND_KEYS: &[&str] =
    &["replace", "expand_targets", "exclude_packages", "targets"];

/// Subcommand tables nested inside a `target.'cfg(...)'` section drop the
/// `targets` list: redefining the outer target axis from a section that was
/// already selected by a target match is circular (and nothing reads it there).
/// Otherwise they accept the same keys as their base-scope counterparts.
const PACKAGE_TARGET_COMMAND_KEYS: &[&str] = &["replace", "expand_targets"];

const WORKSPACE_TARGET_COMMAND_KEYS: &[&str] = &["replace", "expand_targets", "exclude_packages"];

pub(crate) fn validate_package_metadata(
    value: &serde_json::Value,
    section: &str,
) -> eyre::Result<()> {
    validate_keys(value, section, &[PACKAGE_KEYS, FEATURE_MATRIX_KEYS])?;
    validate_target_table(
        value,
        section,
        &[PACKAGE_TARGET_KEYS, FEATURE_MATRIX_KEYS],
        &[PACKAGE_TARGET_COMMAND_KEYS, FEATURE_MATRIX_KEYS],
    )?;
    validate_subcommands(value, section, &[PACKAGE_COMMAND_KEYS, FEATURE_MATRIX_KEYS])?;
    Ok(())
}

pub(crate) fn validate_workspace_metadata(
    value: &serde_json::Value,
    section: &str,
) -> eyre::Result<()> {
    validate_keys(value, section, &[WORKSPACE_KEYS])?;
    validate_target_table(
        value,
        section,
        &[WORKSPACE_TARGET_KEYS],
        &[WORKSPACE_TARGET_COMMAND_KEYS],
    )?;
    validate_subcommands(value, section, &[WORKSPACE_COMMAND_KEYS])?;
    Ok(())
}

fn validate_target_table(
    value: &serde_json::Value,
    section: &str,
    allowed: &[&[&str]],
    command_allowed: &[&[&str]],
) -> eyre::Result<()> {
    let Some(targets) = value.get("target").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    for (cfg_expr, target) in targets {
        let target_section = format!("{section}.target.'{cfg_expr}'");
        validate_keys(target, &target_section, allowed)?;
        validate_subcommands(target, &target_section, command_allowed)?;
    }
    Ok(())
}

fn validate_subcommands(
    value: &serde_json::Value,
    section: &str,
    command_allowed: &[&[&str]],
) -> eyre::Result<()> {
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
            command_allowed,
        )?;
    }
    Ok(())
}

/// Explain why a *known* cargo-fc key is invalid in the scope that rejected it.
///
/// Each of these keys is valid in exactly one family of scopes, so the reason a
/// rejection happened is determined by the key alone — no scope argument needed.
/// Returns `None` for keys that are either valid everywhere (so a rejection means
/// a genuine typo) or not a recognized cargo-fc key at all.
fn misplaced_key_reason(key: &str) -> Option<&'static str> {
    // Feature-matrix keys (and their deprecated spellings) shape one crate's
    // feature combinations, so they only exist in package scopes.
    if FEATURE_MATRIX_KEYS.contains(&key)
        || matches!(key, "skip_feature_sets" | "denylist" | "exact_combinations")
    {
        return Some(
            "feature-matrix settings are per-package and are not valid in workspace scope",
        );
    }
    match key {
        "exclude_packages" => Some(
            "`exclude_packages` selects which workspace members run and is only valid in workspace scope",
        ),
        "targets" => Some(
            "a `targets` list is not valid anywhere inside a `target.'cfg(...)'` section (that section was already selected by a target match); set it at a base scope or a base (non-target-nested) `subcommands.<cmd>` table instead",
        ),
        "replace" => Some(
            "`replace` resets everything broader in the precedence chain, but the workspace base is the broadest scope, so there is nothing for it to reset",
        ),
        "expand_targets" => Some(
            "`expand_targets` is a per-subcommand capability; set it inside a `subcommands.<cmd>` table",
        ),
        _ => None,
    }
}

/// Reject any key that is not in one of `allowlists` or the always-accepted
/// [`FLAG_KEYS`] / [`UNIVERSAL_KEYS`].
fn validate_keys(
    value: &serde_json::Value,
    section: &str,
    allowlists: &[&[&str]],
) -> eyre::Result<()> {
    let Some(map) = value.as_object() else {
        return Ok(());
    };
    for key in map.keys() {
        let allowed = FLAG_KEYS.contains(&key.as_str())
            || UNIVERSAL_KEYS.contains(&key.as_str())
            || allowlists.iter().any(|list| list.contains(&key.as_str()));
        if allowed {
            continue;
        }
        // A recognized-but-misplaced key gets a scope-aware explanation instead
        // of the generic "unknown key" error.
        if let Some(reason) = misplaced_key_reason(key) {
            eyre::bail!("`{key}` is not valid in [{section}]: {reason}");
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
        FEATURE_MATRIX_KEYS, FLAG_KEYS, PACKAGE_COMMAND_KEYS, PACKAGE_KEYS,
        PACKAGE_TARGET_COMMAND_KEYS, PACKAGE_TARGET_KEYS, UNIVERSAL_KEYS, WORKSPACE_COMMAND_KEYS,
        WORKSPACE_KEYS, WORKSPACE_TARGET_COMMAND_KEYS, WORKSPACE_TARGET_KEYS,
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

    fn allowed_keys(allowlists: &[&[&str]]) -> BTreeSet<String> {
        allowlists
            .iter()
            .flat_map(|list| list.iter())
            .chain(FLAG_KEYS)
            .chain(UNIVERSAL_KEYS)
            .copied()
            .map(String::from)
            .collect()
    }

    fn assert_allowlist_matches_serialized_keys<T: Default + serde::Serialize>(
        name: &str,
        allowlists: &[&[&str]],
    ) {
        let actual = serialized_keys::<T>();
        let allowed = allowed_keys(allowlists);
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
        assert_allowlist_matches_serialized_keys::<Config>(
            "package config",
            &[PACKAGE_KEYS, FEATURE_MATRIX_KEYS],
        );
        assert_allowlist_matches_serialized_keys::<TargetOverride>(
            "package target override",
            &[PACKAGE_TARGET_KEYS, FEATURE_MATRIX_KEYS],
        );
        assert_allowlist_matches_serialized_keys::<WorkspaceConfig>(
            "workspace config",
            &[WORKSPACE_KEYS],
        );
        assert_allowlist_matches_serialized_keys::<WorkspaceTargetOverride>(
            "workspace target override",
            &[WORKSPACE_TARGET_KEYS],
        );
        // `CommandCapabilities` is shared by package and workspace subcommand
        // scopes, so its serde fields must be the union of both scopes'
        // allowlists (each scope accepts a subset, gated by validation).
        assert_allowlist_matches_serialized_keys::<CommandCapabilities>(
            "command capabilities",
            &[
                PACKAGE_COMMAND_KEYS,
                WORKSPACE_COMMAND_KEYS,
                FEATURE_MATRIX_KEYS,
            ],
        );
    }

    #[test]
    fn workspace_command_keys_are_real_command_capability_fields() {
        // Workspace subcommand tables deliberately accept a narrower set than the
        // shared `CommandCapabilities` type exposes (they reject per-package
        // feature keys). Whatever they DO accept must still be a real field on
        // the type, or a typo/drift here would silently reject a valid workspace
        // config with no other test catching it.
        let capability_keys: BTreeSet<String> = serialized_keys::<CommandCapabilities>()
            .into_iter()
            .chain(FLAG_KEYS.iter().map(|k| (*k).to_string()))
            .collect();
        for key in WORKSPACE_COMMAND_KEYS {
            assert!(
                capability_keys.contains(*key),
                "WORKSPACE_COMMAND_KEYS references `{key}`, which is not a CommandCapabilities field",
            );
        }
    }

    #[test]
    fn target_command_keys_are_base_command_keys_without_targets() {
        // The subcommand allowlists nested under a `target.'cfg(...)'` section are
        // exactly their base-scope counterparts with `targets` removed (a
        // `targets` list is `—` at the target×subcommand scope). Deriving the rule
        // in a test keeps the two literal consts from drifting — a typo or stale
        // entry would otherwise silently reject a valid config.
        fn without_targets(base: &[&'static str]) -> Vec<&'static str> {
            base.iter()
                .copied()
                .filter(|key| *key != "targets")
                .collect()
        }
        assert_eq!(
            PACKAGE_TARGET_COMMAND_KEYS,
            without_targets(PACKAGE_COMMAND_KEYS).as_slice(),
        );
        assert_eq!(
            WORKSPACE_TARGET_COMMAND_KEYS,
            without_targets(WORKSPACE_COMMAND_KEYS).as_slice(),
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
        // `replace` resets everything broader; the workspace base is the
        // broadest layer, so it has nothing to reset and rejects the key.
        let err = super::validate_workspace_metadata(
            &serde_json::json!({ "replace": true }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace base should reject replace");
        assert!(err.to_string().contains("replace"));
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
        // A package can't exclude sibling packages — that is a workspace concern.
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "subcommands": { "test": { "exclude_packages": ["foo"] } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("package subcommand should reject exclude_packages");
        assert!(err.to_string().contains("exclude_packages"));
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

        assert!(err.to_string().contains("exclude_features"));
    }

    #[test]
    fn misplaced_known_keys_get_scope_aware_reasons() {
        // Feature-matrix key in workspace scope.
        let err = super::validate_workspace_metadata(
            &serde_json::json!({ "exclude_features": ["gpu"] }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace base should reject feature-matrix keys");
        assert!(err.to_string().contains("per-package"), "{err}");

        // `exclude_packages` in a package subcommand scope (the package base
        // still accepts the deprecated flat key for backwards compatibility).
        let err = super::validate_package_metadata(
            &serde_json::json!({ "subcommands": { "test": { "exclude_packages": ["foo"] } } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("package subcommand should reject exclude_packages");
        assert!(err.to_string().contains("workspace scope"), "{err}");

        // `targets` list inside a `target.'cfg(...)'` section.
        let err = super::validate_package_metadata(
            &serde_json::json!({ "target": { "cfg(unix)": { "targets": ["x"] } } }),
            "package.metadata.cargo-fc",
        )
        .expect_err("target section should reject a targets list");
        assert!(
            err.to_string().contains("not valid anywhere inside"),
            "{err}"
        );

        // `replace` at the workspace base.
        let err = super::validate_workspace_metadata(
            &serde_json::json!({ "replace": true }),
            "workspace.metadata.cargo-fc",
        )
        .expect_err("workspace base should reject replace");
        assert!(err.to_string().contains("nothing for it to reset"), "{err}");

        // `expand_targets` outside a subcommand table.
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
        // A `targets` list is valid in a BASE subcommand table...
        super::validate_package_metadata(
            &serde_json::json!({ "subcommands": { "test": { "targets": ["x"] } } }),
            "package.metadata.cargo-fc",
        )
        .expect("base subcommand accepts a targets list");

        // ...but not in a subcommand table nested under a target section (that
        // scope is `—` in the matrix, and nothing reads it there).
        let err = super::validate_package_metadata(
            &serde_json::json!({
                "target": { "cfg(unix)": { "subcommands": { "test": { "targets": ["x"] } } } },
            }),
            "package.metadata.cargo-fc",
        )
        .expect_err("target-nested subcommand should reject a targets list");
        assert!(err.to_string().contains("targets"), "{err}");

        // Same for the workspace scope.
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
        // The `driver` scalar is overridable at every scope in the matrix.
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
}
