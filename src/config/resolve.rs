use crate::cfg_eval::CfgEvaluator;
use crate::target::TargetTriple;
use color_eyre::eyre::{self, WrapErr};
use serde_json_merge::{iter::dfs::Dfs, merge::Merge};
use std::collections::{BTreeSet, HashMap, HashSet};

use super::patch::{FeatureSetVecPatch, StringSetPatch};
use super::{Config, DeprecatedConfig, TargetOverride};

/// Resolve a target-specific effective [`Config`] for the given base config.
///
/// The base config is read from `[package.metadata.cargo-fc]` (or any supported alias).
/// Target overrides are read from
/// `[package.metadata.cargo-fc.target.'cfg(...)']`.
///
/// This function:
///
/// - determines which cfg expressions match the given target
/// - validates and applies `replace = true`
/// - merges override patches deterministically
/// - returns an effective config with `target` metadata removed
///
/// # Errors
///
/// Returns an error if cfg evaluation fails or if overrides conflict.
pub fn resolve_config<E: CfgEvaluator>(
    base: &Config,
    target: &TargetTriple,
    evaluator: &mut E,
) -> eyre::Result<Config> {
    let matched = matching_overrides(base, target, evaluator)?;

    // Fast path: no matching overrides.
    if matched.is_empty() {
        let mut out = base.clone();
        out.target.clear();
        return Ok(out);
    }

    let replace_exprs: Vec<&str> = matched
        .iter()
        .filter_map(|(expr, ov)| if ov.replace { Some(*expr) } else { None })
        .collect();

    if replace_exprs.len() > 1 {
        eyre::bail!(
            "multiple matching target overrides have replace = true: {}",
            replace_exprs.join(", ")
        );
    }

    let replace_mode = replace_exprs.len() == 1;

    if replace_mode {
        // When replace is enabled, disallow add/remove operations, which are
        // confusing (users might think they add to the base config rather than
        // the fresh default config).
        for (expr, ov) in &matched {
            validate_replace_override(expr, ov)?;
        }
    }

    let mut out = if replace_mode {
        Config::default()
    } else {
        base.clone()
    };

    // Apply matching overrides.
    apply_overrides(&mut out, &matched)?;

    // Remove target metadata from the resolved config.
    out.target.clear();
    out.deprecated = DeprecatedConfig::default();

    Ok(out)
}

fn matching_overrides<'a, E: CfgEvaluator>(
    base: &'a Config,
    target: &TargetTriple,
    evaluator: &mut E,
) -> eyre::Result<Vec<(&'a str, &'a TargetOverride)>> {
    let mut matched = Vec::new();
    for (expr, ov) in &base.target {
        let is_match = evaluator
            .matches(expr, target)
            .wrap_err_with(|| format!("failed to evaluate cfg expression `{expr}`"))?;
        if is_match {
            matched.push((expr.as_str(), ov));
        }
    }
    Ok(matched)
}

fn validate_replace_override(expr: &str, ov: &TargetOverride) -> eyre::Result<()> {
    let mut invalid_fields = Vec::new();

    let check_string_set = |name: &str, p: &Option<StringSetPatch>, invalid: &mut Vec<String>| {
        if let Some(p) = p
            && p.has_add_or_remove()
        {
            invalid.push(name.to_string());
        }
    };

    let check_feature_sets =
        |name: &str, p: &Option<FeatureSetVecPatch>, invalid: &mut Vec<String>| {
            if let Some(p) = p
                && p.has_add_or_remove()
            {
                invalid.push(name.to_string());
            }
        };

    check_feature_sets(
        "isolated_feature_sets",
        &ov.isolated_feature_sets,
        &mut invalid_fields,
    );
    check_string_set(
        "exclude_features",
        &ov.exclude_features,
        &mut invalid_fields,
    );
    check_string_set(
        "include_features",
        &ov.include_features,
        &mut invalid_fields,
    );
    check_string_set("only_features", &ov.only_features, &mut invalid_fields);
    check_feature_sets(
        "exclude_feature_sets",
        &ov.exclude_feature_sets,
        &mut invalid_fields,
    );
    check_feature_sets(
        "include_feature_sets",
        &ov.include_feature_sets,
        &mut invalid_fields,
    );
    check_feature_sets(
        "allow_feature_sets",
        &ov.allow_feature_sets,
        &mut invalid_fields,
    );

    if !invalid_fields.is_empty() {
        eyre::bail!(
            "target override `{expr}` uses add/remove patch operations while replace=true: {}",
            invalid_fields.join(", ")
        );
    }

    Ok(())
}

fn apply_overrides(out: &mut Config, matched: &[(&str, &TargetOverride)]) -> eyre::Result<()> {
    // Booleans
    if let Some(v) = combine_bool("skip_optional_dependencies", matched, |o| {
        o.skip_optional_dependencies
    })? {
        out.skip_optional_dependencies = v;
    }
    if let Some(v) = combine_bool("no_empty_feature_set", matched, |o| o.no_empty_feature_set)? {
        out.no_empty_feature_set = v;
    }

    // Set-like fields
    if let Some(ops) =
        combine_string_set_patch("exclude_features", matched, |o| o.exclude_features.as_ref())?
    {
        apply_string_set_patch(&mut out.exclude_features, &ops);
    }
    if let Some(ops) =
        combine_string_set_patch("include_features", matched, |o| o.include_features.as_ref())?
    {
        apply_string_set_patch(&mut out.include_features, &ops);
    }
    if let Some(ops) =
        combine_string_set_patch("only_features", matched, |o| o.only_features.as_ref())?
    {
        apply_string_set_patch(&mut out.only_features, &ops);
    }

    // Feature-set list fields
    if let Some(ops) = combine_feature_set_vec_patch("isolated_feature_sets", matched, |o| {
        o.isolated_feature_sets.as_ref()
    })? {
        out.isolated_feature_sets = apply_feature_set_vec_patch(&out.isolated_feature_sets, &ops);
    }

    if let Some(ops) = combine_feature_set_vec_patch("exclude_feature_sets", matched, |o| {
        o.exclude_feature_sets.as_ref()
    })? {
        out.exclude_feature_sets = apply_feature_set_vec_patch(&out.exclude_feature_sets, &ops);
    }

    if let Some(ops) = combine_feature_set_vec_patch("include_feature_sets", matched, |o| {
        o.include_feature_sets.as_ref()
    })? {
        out.include_feature_sets = apply_feature_set_vec_patch(&out.include_feature_sets, &ops);
    }

    // allow_feature_sets is treated as a singleton mode switch: at most one
    // matching override may specify it.
    let allow_patches: Vec<(&str, &FeatureSetVecPatch)> = matched
        .iter()
        .filter_map(|(expr, o)| o.allow_feature_sets.as_ref().map(|p| (*expr, p)))
        .collect();
    if allow_patches.len() > 1 {
        let exprs = allow_patches.iter().map(|(e, _)| *e).collect::<Vec<_>>();
        eyre::bail!(
            "multiple matching target overrides set allow_feature_sets: {}",
            exprs.join(", ")
        );
    }
    if let Some((_expr, patch)) = allow_patches.first() {
        let ops = feature_set_vec_patch_to_ops(patch);
        out.allow_feature_sets = apply_feature_set_vec_patch(&out.allow_feature_sets, &ops);
    }

    // Matrix metadata: deep-merge in deterministic order (cfg key order).
    for (_expr, ov) in matched {
        if let Some(matrix) = &ov.matrix {
            merge_matrix(&mut out.matrix, matrix)?;
        }
    }

    Ok(())
}

fn merge_matrix(
    base: &mut HashMap<String, serde_json::Value>,
    patch: &HashMap<String, serde_json::Value>,
) -> eyre::Result<()> {
    let mut out = serde_json::json!(base);
    out.merge::<Dfs>(&serde_json::json!(patch));
    *base = serde_json::from_value(out)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct StringSetOps {
    override_value: Option<HashSet<String>>,
    add: HashSet<String>,
    remove: HashSet<String>,
}

/// Apply combined patch operations to a string set.
///
/// The order is: start from override (or base), then remove, then add.
/// This means if a value appears in both `add` and `remove`, **add wins**.
fn apply_string_set_patch(target: &mut HashSet<String>, ops: &StringSetOps) {
    let mut out = if let Some(v) = &ops.override_value {
        v.clone()
    } else {
        target.clone()
    };

    for r in &ops.remove {
        out.remove(r);
    }
    out.extend(ops.add.iter().cloned());

    *target = out;
}

fn combine_string_set_patch<'a>(
    name: &str,
    matched: &[(&'a str, &'a TargetOverride)],
    get: impl Fn(&'a TargetOverride) -> Option<&'a StringSetPatch>,
) -> eyre::Result<Option<StringSetOps>> {
    let mut any = false;
    let mut override_value: Option<HashSet<String>> = None;
    let mut add: HashSet<String> = HashSet::new();
    let mut remove: HashSet<String> = HashSet::new();

    for (expr, ov) in matched {
        if let Some(patch) = get(ov) {
            any = true;

            if let Some(ovv) = patch.override_value() {
                match &override_value {
                    None => override_value = Some(ovv.clone()),
                    Some(existing) => {
                        if existing != ovv {
                            eyre::bail!(
                                "conflicting overrides for `{name}` from target override `{expr}`"
                            );
                        }
                    }
                }
            }

            add.extend(patch.add_values().iter().cloned());
            remove.extend(patch.remove_values().iter().cloned());
        }
    }

    if !any {
        return Ok(None);
    }

    Ok(Some(StringSetOps {
        override_value,
        add,
        remove,
    }))
}

fn combine_bool<'a>(
    name: &str,
    matched: &[(&'a str, &'a TargetOverride)],
    get: impl Fn(&'a TargetOverride) -> Option<bool>,
) -> eyre::Result<Option<bool>> {
    let mut out: Option<bool> = None;
    for (expr, ov) in matched {
        if let Some(v) = get(ov) {
            match out {
                None => out = Some(v),
                Some(existing) if existing == v => {}
                Some(_) => {
                    eyre::bail!("conflicting values for `{name}` in target override `{expr}`")
                }
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct FeatureSetVecOps {
    override_value: Option<BTreeSet<Vec<String>>>,
    add: BTreeSet<Vec<String>>,
    remove: BTreeSet<Vec<String>>,
}

fn feature_set_vec_patch_to_ops(patch: &FeatureSetVecPatch) -> FeatureSetVecOps {
    let override_value = patch.override_value().map(|v| normalize_feature_sets(v));
    let mut add = BTreeSet::new();
    for s in patch.add_values() {
        add.insert(normalize_feature_set(s));
    }
    let mut remove = BTreeSet::new();
    for s in patch.remove_values() {
        remove.insert(normalize_feature_set(s));
    }

    FeatureSetVecOps {
        override_value,
        add,
        remove,
    }
}

fn combine_feature_set_vec_patch<'a>(
    name: &str,
    matched: &[(&'a str, &'a TargetOverride)],
    get: impl Fn(&'a TargetOverride) -> Option<&'a FeatureSetVecPatch>,
) -> eyre::Result<Option<FeatureSetVecOps>> {
    let mut any = false;
    let mut override_value: Option<BTreeSet<Vec<String>>> = None;
    let mut add: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut remove: BTreeSet<Vec<String>> = BTreeSet::new();

    for (expr, ov) in matched {
        if let Some(patch) = get(ov) {
            any = true;

            if let Some(ovv) = patch.override_value() {
                let normalized = normalize_feature_sets(ovv);
                match &override_value {
                    None => override_value = Some(normalized),
                    Some(existing) => {
                        if existing != &normalized {
                            eyre::bail!(
                                "conflicting overrides for `{name}` from target override `{expr}`"
                            );
                        }
                    }
                }
            }

            for s in patch.add_values() {
                add.insert(normalize_feature_set(s));
            }
            for s in patch.remove_values() {
                remove.insert(normalize_feature_set(s));
            }
        }
    }

    if !any {
        return Ok(None);
    }

    Ok(Some(FeatureSetVecOps {
        override_value,
        add,
        remove,
    }))
}

/// Apply combined patch operations to a feature set list.
///
/// The order is: start from override (or base), then remove, then add.
/// This means if a set appears in both `add` and `remove`, **add wins**.
fn apply_feature_set_vec_patch(
    base: &[HashSet<String>],
    ops: &FeatureSetVecOps,
) -> Vec<HashSet<String>> {
    let mut out: BTreeSet<Vec<String>> = if let Some(v) = &ops.override_value {
        v.clone()
    } else {
        normalize_feature_sets(base)
    };

    for r in &ops.remove {
        out.remove(r);
    }
    for a in &ops.add {
        out.insert(a.clone());
    }

    out.into_iter()
        .map(|v| v.into_iter().collect::<HashSet<String>>())
        .collect()
}

fn normalize_feature_set(set: &HashSet<String>) -> Vec<String> {
    let mut v = set.iter().cloned().collect::<Vec<_>>();
    v.sort();
    v
}

fn normalize_feature_sets(sets: &[HashSet<String>]) -> BTreeSet<Vec<String>> {
    sets.iter().map(normalize_feature_set).collect()
}

#[cfg(test)]
mod test {
    use super::resolve_config;
    use crate::cfg_eval::CfgEvaluator;
    use crate::config::patch::{FeatureSetVecPatch, StringSetPatch};
    use crate::config::{Config, TargetOverride};
    use crate::target::TargetTriple;
    use color_eyre::eyre;
    use std::collections::{BTreeMap, HashMap, HashSet};

    #[derive(Default)]
    struct StubEval {
        matches: HashSet<String>,
    }

    impl CfgEvaluator for StubEval {
        fn matches(&mut self, cfg_expr: &str, _target: &TargetTriple) -> eyre::Result<bool> {
            Ok(self.matches.contains(cfg_expr))
        }
    }

    fn hs(values: &[&str]) -> HashSet<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    fn hss(sets: &[&[&str]]) -> Vec<HashSet<String>> {
        sets.iter().map(|s| hs(s)).collect()
    }

    #[test]
    fn additive_exclude_features() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["cuda"]),
                    remove: HashSet::new(),
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("cuda"));
        Ok(())
    }

    #[test]
    fn override_exclude_features_array_syntax() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Override(hs(&["cuda"]))),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(!out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("cuda"));
        Ok(())
    }

    #[test]
    fn conflicting_override_errors() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Override(hs(&["a"]))),
                ..TargetOverride::default()
            },
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Override(hs(&["b"]))),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let Err(err) = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval) else {
            eyre::bail!("expected conflicting override resolution to fail");
        };
        assert!(err.to_string().contains("conflicting overrides"));
        Ok(())
    }

    #[test]
    fn replace_disallows_add_remove() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                replace: true,
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["cuda"]),
                    remove: HashSet::new(),
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let Err(err) = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval) else {
            eyre::bail!("expected replace=true add/remove validation to fail");
        };
        assert!(err.to_string().contains("replace=true"));
        Ok(())
    }

    #[test]
    fn replace_starts_from_default() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            skip_optional_dependencies: true,
            ..Config::default()
        };
        base.matrix
            .insert("k".to_string(), serde_json::json!({"a": 1}));

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                replace: true,
                exclude_features: Some(StringSetPatch::Override(hs(&["cuda"]))),
                matrix: Some({
                    let mut m = HashMap::new();
                    m.insert("k".to_string(), serde_json::json!({"b": 2}));
                    m
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;

        // base fields are not inherited
        assert!(!out.exclude_features.contains("default"));
        assert!(!out.skip_optional_dependencies);

        // override is applied
        assert!(out.exclude_features.contains("cuda"));

        // matrix is merged onto default (empty)
        let v = out
            .matrix
            .get("k")
            .ok_or_else(|| eyre::eyre!("missing matrix key"))?;
        assert!(v.get("b").is_some());

        Ok(())
    }

    #[test]
    fn no_match_returns_base_unchanged() -> eyre::Result<()> {
        let base = Config {
            exclude_features: hs(&["default"]),
            skip_optional_dependencies: true,
            ..Config::default()
        };

        let mut eval = StubEval::default();
        // No matches configured, so nothing matches.

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.exclude_features, hs(&["default"]));
        assert!(out.skip_optional_dependencies);
        assert!(out.target.is_empty(), "target metadata should be cleared");
        Ok(())
    }

    #[test]
    fn remove_exclude_features() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default", "cuda", "metal"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: HashSet::new(),
                    remove: hs(&["cuda"]),
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("metal"));
        assert!(
            !out.exclude_features.contains("cuda"),
            "cuda should be removed"
        );
        Ok(())
    }

    #[test]
    fn multiple_matching_sections_combine_adds() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["a"]),
                    remove: HashSet::new(),
                }),
                ..TargetOverride::default()
            },
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["b"]),
                    remove: HashSet::new(),
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.exclude_features.contains("default"));
        assert!(out.exclude_features.contains("a"));
        assert!(out.exclude_features.contains("b"));
        Ok(())
    }

    #[test]
    fn add_wins_over_remove_for_same_value() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                exclude_features: Some(StringSetPatch::Patch {
                    r#override: None,
                    add: hs(&["cuda"]),
                    remove: hs(&["cuda"]),
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        // add is applied after remove, so "cuda" should be present.
        assert!(out.exclude_features.contains("cuda"));
        Ok(())
    }

    #[test]
    fn boolean_override_no_empty_feature_set() -> eyre::Result<()> {
        let mut base = Config {
            no_empty_feature_set: false,
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                no_empty_feature_set: Some(true),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert!(out.no_empty_feature_set);
        Ok(())
    }

    #[test]
    fn feature_set_vec_patch_add_include_feature_sets() -> eyre::Result<()> {
        let mut base = Config {
            include_feature_sets: hss(&[&["a", "b"]]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                include_feature_sets: Some(FeatureSetVecPatch::Patch {
                    r#override: None,
                    add: hss(&[&["c", "d"]]),
                    remove: Vec::new(),
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.include_feature_sets.len(), 2);
        let sets: Vec<HashSet<String>> = out.include_feature_sets;
        assert!(sets.contains(&hs(&["a", "b"])));
        assert!(sets.contains(&hs(&["c", "d"])));
        Ok(())
    }

    #[test]
    fn feature_set_vec_patch_remove() -> eyre::Result<()> {
        let mut base = Config {
            include_feature_sets: hss(&[&["a", "b"], &["c"]]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                include_feature_sets: Some(FeatureSetVecPatch::Patch {
                    r#override: None,
                    add: Vec::new(),
                    remove: hss(&[&["a", "b"]]),
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(out.include_feature_sets.len(), 1);
        assert!(out.include_feature_sets.contains(&hs(&["c"])));
        Ok(())
    }

    #[test]
    fn matrix_metadata_merge_adds_new_key() -> eyre::Result<()> {
        let mut base = Config::default();
        base.matrix
            .insert("existing".to_string(), serde_json::json!("keep"));

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                matrix: Some({
                    let mut m = HashMap::new();
                    m.insert("added".to_string(), serde_json::json!("new"));
                    m
                }),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());

        let out = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval)?;
        assert_eq!(
            out.matrix.get("existing"),
            Some(&serde_json::json!("keep")),
            "original key preserved"
        );
        assert_eq!(
            out.matrix.get("added"),
            Some(&serde_json::json!("new")),
            "new key added from patch"
        );
        Ok(())
    }

    #[test]
    fn allow_feature_sets_singleton_conflict() -> eyre::Result<()> {
        let mut base = Config {
            exclude_features: hs(&["default"]),
            ..Config::default()
        };

        let mut target = BTreeMap::new();
        target.insert(
            "cfg(unix)".to_string(),
            TargetOverride {
                allow_feature_sets: Some(FeatureSetVecPatch::Override(hss(&[&["a"]]))),
                ..TargetOverride::default()
            },
        );
        target.insert(
            "cfg(target_os = \"linux\")".to_string(),
            TargetOverride {
                allow_feature_sets: Some(FeatureSetVecPatch::Override(hss(&[&["b"]]))),
                ..TargetOverride::default()
            },
        );
        base.target = target;

        let mut eval = StubEval::default();
        eval.matches.insert("cfg(unix)".to_string());
        eval.matches
            .insert("cfg(target_os = \"linux\")".to_string());

        let Err(err) = resolve_config(&base, &TargetTriple("x".to_string()), &mut eval) else {
            eyre::bail!("expected allow_feature_sets singleton conflict");
        };
        assert!(err.to_string().contains("allow_feature_sets"));
        Ok(())
    }
}
