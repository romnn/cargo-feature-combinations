use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

/// Patch operations for a set-like configuration field.
///
/// A patch can either be:
///
/// - a plain array, which is interpreted as a full override
/// - a patch object with explicit `override`, `add`, and `remove` operations
///
/// Arrays are always treated as overrides to avoid ambiguity.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum StringSetPatch {
    /// Shorthand syntax: `key = ["a", "b"]`.
    Override(HashSet<String>),
    /// Explicit patch syntax: `key = { override = [...], add = [...], remove = [...] }`.
    Patch {
        /// If present, replace the entire value instead of applying add/remove.
        #[serde(default)]
        r#override: Option<HashSet<String>>,
        /// Values to add to the base set.
        #[serde(default)]
        add: HashSet<String>,
        /// Values to remove from the base set.
        #[serde(default)]
        remove: HashSet<String>,
    },
}

impl StringSetPatch {
    /// Return the override value, if the patch is an override.
    #[must_use]
    pub fn override_value(&self) -> Option<&HashSet<String>> {
        match self {
            Self::Override(v) => Some(v),
            Self::Patch { r#override, .. } => r#override.as_ref(),
        }
    }

    /// Return the set of values to add.
    #[must_use]
    pub fn add_values(&self) -> &HashSet<String> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
        match self {
            Self::Override(_) => &EMPTY,
            Self::Patch { add, .. } => add,
        }
    }

    /// Return the set of values to remove.
    #[must_use]
    pub fn remove_values(&self) -> &HashSet<String> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
        match self {
            Self::Override(_) => &EMPTY,
            Self::Patch { remove, .. } => remove,
        }
    }

    /// Return `true` if the patch contains any add/remove operations.
    #[must_use]
    pub fn has_add_or_remove(&self) -> bool {
        !self.add_values().is_empty() || !self.remove_values().is_empty()
    }
}

/// Patch operations for an ordered target-triple list.
///
/// This has the same TOML surface as [`StringSetPatch`], but it keeps values in
/// declaration order instead of normalizing through a set.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum TargetListPatch {
    /// Shorthand syntax: `targets = ["a", "b"]`.
    Override(Vec<String>),
    /// Explicit patch syntax: `targets = { override = [...], add = [...], remove = [...] }`.
    Patch {
        /// If present, replace the inherited list before applying add/remove.
        #[serde(default)]
        r#override: Option<Vec<String>>,
        /// Values to append to the inherited list.
        #[serde(default)]
        add: Vec<String>,
        /// Values to remove from the inherited list.
        #[serde(default)]
        remove: Vec<String>,
    },
}

impl TargetListPatch {
    /// Return the override value, if the patch is an override.
    #[must_use]
    pub fn override_value(&self) -> Option<&[String]> {
        match self {
            Self::Override(v) => Some(v),
            Self::Patch { r#override, .. } => r#override.as_deref(),
        }
    }

    /// Return the ordered values to add.
    #[must_use]
    pub fn add_values(&self) -> &[String] {
        match self {
            Self::Override(_) => &[],
            Self::Patch { add, .. } => add,
        }
    }

    /// Return the ordered values to remove.
    #[must_use]
    pub fn remove_values(&self) -> &[String] {
        match self {
            Self::Override(_) => &[],
            Self::Patch { remove, .. } => remove,
        }
    }

    /// Return `true` if the patch contains any add/remove operations.
    #[must_use]
    pub fn has_add_or_remove(&self) -> bool {
        !self.add_values().is_empty() || !self.remove_values().is_empty()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TargetListOps {
    override_value: Option<Vec<String>>,
    add: Vec<String>,
    remove: Vec<String>,
}

impl TargetListOps {
    /// Apply operations while preserving declaration order.
    #[must_use]
    pub(crate) fn apply_to(&self, base: &[String]) -> Vec<String> {
        let mut out = self
            .override_value
            .clone()
            .unwrap_or_else(|| dedup_ordered(base.iter().cloned()));
        let remove: HashSet<&str> = self.remove.iter().map(String::as_str).collect();
        out.retain(|value| !remove.contains(value.as_str()));
        let mut existing: HashSet<String> = out.iter().cloned().collect();
        out.extend(
            self.add
                .iter()
                .filter(|value| existing.insert((*value).clone()))
                .cloned(),
        );
        out
    }
}

/// Combine sibling target-list patches without losing declaration order.
pub(crate) fn combine_target_list_patches<'a>(
    name: &str,
    source_kind: &str,
    patches: impl IntoIterator<Item = (&'a str, &'a TargetListPatch)>,
) -> color_eyre::eyre::Result<Option<TargetListOps>> {
    let mut any = false;
    let mut override_value: Option<Vec<String>> = None;
    let mut add = Vec::new();
    let mut remove = Vec::new();

    for (expr, patch) in patches {
        any = true;

        if let Some(value) = patch.override_value() {
            let value = dedup_ordered(value.iter().cloned());
            match &override_value {
                None => override_value = Some(value),
                Some(existing) if *existing == value => {}
                Some(_) => {
                    color_eyre::eyre::bail!(
                        "conflicting overrides for `{name}` from {source_kind} `{expr}`"
                    );
                }
            }
        }

        extend_ordered_unique(&mut add, patch.add_values().iter().cloned());
        extend_ordered_unique(&mut remove, patch.remove_values().iter().cloned());
    }

    Ok(any.then_some(TargetListOps {
        override_value,
        add,
        remove,
    }))
}

fn dedup_ordered(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn extend_ordered_unique(out: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    let mut seen: HashSet<String> = out.iter().cloned().collect();
    out.extend(
        values
            .into_iter()
            .filter(|value| seen.insert(value.clone())),
    );
}

/// Patch operations for a list of feature sets (each represented as a set of strings).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum FeatureSetVecPatch {
    /// Shorthand syntax: `key = [["a"], ["b", "c"]]`.
    Override(Vec<HashSet<String>>),
    /// Explicit patch syntax.
    Patch {
        /// If present, replace the entire list instead of applying add/remove.
        #[serde(default)]
        r#override: Option<Vec<HashSet<String>>>,
        /// Feature sets to append.
        #[serde(default)]
        add: Vec<HashSet<String>>,
        /// Feature sets to remove.
        #[serde(default)]
        remove: Vec<HashSet<String>>,
    },
}

impl FeatureSetVecPatch {
    /// Return the override value, if the patch is an override.
    #[must_use]
    pub fn override_value(&self) -> Option<&Vec<HashSet<String>>> {
        match self {
            Self::Override(v) => Some(v),
            Self::Patch { r#override, .. } => r#override.as_ref(),
        }
    }

    /// Return the feature sets to add.
    #[must_use]
    pub fn add_values(&self) -> &[HashSet<String>] {
        static EMPTY: std::sync::LazyLock<Vec<HashSet<String>>> =
            std::sync::LazyLock::new(Vec::new);
        match self {
            Self::Override(_) => &EMPTY,
            Self::Patch { add, .. } => add,
        }
    }

    /// Return the feature sets to remove.
    #[must_use]
    pub fn remove_values(&self) -> &[HashSet<String>] {
        static EMPTY: std::sync::LazyLock<Vec<HashSet<String>>> =
            std::sync::LazyLock::new(Vec::new);
        match self {
            Self::Override(_) => &EMPTY,
            Self::Patch { remove, .. } => remove,
        }
    }

    /// Return `true` if the patch contains any add/remove operations.
    #[must_use]
    pub fn has_add_or_remove(&self) -> bool {
        !self.add_values().is_empty() || !self.remove_values().is_empty()
    }
}

/// A single override's contribution to a set-like field, normalized to a set of
/// comparable elements `Elem`.
///
/// Both [`StringSetPatch`] (elements are feature names) and
/// [`FeatureSetVecPatch`] (elements are whole feature sets, normalized to a
/// sorted `Vec<String>`) implement this, so one patch engine
/// ([`combine_set_patches`] + [`SetPatchOps`]) resolves every set-like field
/// regardless of its element type.
pub(crate) trait SetPatchInput {
    type Elem: Ord + Clone;

    /// The full replacement value, if this patch is an override. Materialized as
    /// a `BTreeSet` because it is compared for conflict detection and stored.
    fn override_elems(&self) -> Option<BTreeSet<Self::Elem>>;
    /// Elements to union into the base value.
    fn add_elems(&self) -> impl Iterator<Item = Self::Elem> + '_;
    /// Elements to subtract from the base value.
    fn remove_elems(&self) -> impl Iterator<Item = Self::Elem> + '_;
}

fn normalize_feature_set(set: &HashSet<String>) -> Vec<String> {
    let mut v = set.iter().cloned().collect::<Vec<_>>();
    v.sort();
    v
}

impl SetPatchInput for StringSetPatch {
    type Elem = String;

    fn override_elems(&self) -> Option<BTreeSet<String>> {
        self.override_value().map(|v| v.iter().cloned().collect())
    }
    fn add_elems(&self) -> impl Iterator<Item = String> + '_ {
        self.add_values().iter().cloned()
    }
    fn remove_elems(&self) -> impl Iterator<Item = String> + '_ {
        self.remove_values().iter().cloned()
    }
}

impl SetPatchInput for FeatureSetVecPatch {
    type Elem = Vec<String>;

    fn override_elems(&self) -> Option<BTreeSet<Vec<String>>> {
        self.override_value()
            .map(|v| v.iter().map(normalize_feature_set).collect())
    }
    fn add_elems(&self) -> impl Iterator<Item = Vec<String>> + '_ {
        self.add_values().iter().map(normalize_feature_set)
    }
    fn remove_elems(&self) -> impl Iterator<Item = Vec<String>> + '_ {
        self.remove_values().iter().map(normalize_feature_set)
    }
}

/// The combined patch for one field across all sibling overrides of a layer.
///
/// The order of application is: start from override (or base), then remove, then
/// add. If an element appears in both `add` and `remove`, **add wins**.
#[derive(Debug, Clone)]
pub(crate) struct SetPatchOps<E: Ord + Clone> {
    override_value: Option<BTreeSet<E>>,
    add: BTreeSet<E>,
    remove: BTreeSet<E>,
}

impl<E: Ord + Clone> SetPatchOps<E> {
    /// Build operations from one patch. A single patch cannot have conflicting
    /// overrides, so this is infallible.
    pub(crate) fn from_single<P>(patch: &P) -> Self
    where
        P: SetPatchInput<Elem = E>,
    {
        Self {
            override_value: patch.override_elems(),
            add: patch.add_elems().collect(),
            remove: patch.remove_elems().collect(),
        }
    }

    /// `base` is only materialized when this patch is not a full override, so a
    /// pure-override layer skips converting the base value it would discard.
    fn apply(&self, base: impl FnOnce() -> BTreeSet<E>) -> BTreeSet<E> {
        let mut out = match &self.override_value {
            Some(value) => value.clone(),
            None => base(),
        };
        for value in &self.remove {
            out.remove(value);
        }
        out.extend(self.add.iter().cloned());
        out
    }
}

impl SetPatchOps<String> {
    /// Apply onto a plain string set (e.g. `exclude_features`, `exclude_packages`).
    #[must_use]
    pub(crate) fn apply_to(&self, base: &HashSet<String>) -> HashSet<String> {
        self.apply(|| base.iter().cloned().collect())
            .into_iter()
            .collect()
    }
}

impl SetPatchOps<Vec<String>> {
    /// Apply onto a list of feature sets (e.g. `isolated_feature_sets`).
    #[must_use]
    pub(crate) fn apply_to_feature_sets(&self, base: &[HashSet<String>]) -> Vec<HashSet<String>> {
        self.apply(|| base.iter().map(normalize_feature_set).collect())
            .into_iter()
            .map(|set| set.into_iter().collect())
            .collect()
    }
}

/// Combine the sibling patches of one layer into a single [`SetPatchOps`].
///
/// Conflicting `override` values from different siblings are an error; `add` and
/// `remove` contributions are unioned. Returns `None` when no sibling touched
/// the field. Works for any [`SetPatchInput`], so string sets and feature-set
/// lists share this one implementation.
pub(crate) fn combine_set_patches<'a, P>(
    name: &str,
    source_kind: &str,
    patches: impl IntoIterator<Item = (&'a str, &'a P)>,
) -> color_eyre::eyre::Result<Option<SetPatchOps<P::Elem>>>
where
    P: SetPatchInput + 'a,
{
    let mut any = false;
    let mut override_value: Option<BTreeSet<P::Elem>> = None;
    let mut add: BTreeSet<P::Elem> = BTreeSet::new();
    let mut remove: BTreeSet<P::Elem> = BTreeSet::new();

    for (expr, patch) in patches {
        any = true;

        if let Some(value) = patch.override_elems() {
            match &override_value {
                None => override_value = Some(value),
                Some(existing) if *existing == value => {}
                Some(_) => {
                    color_eyre::eyre::bail!(
                        "conflicting overrides for `{name}` from {source_kind} `{expr}`"
                    );
                }
            }
        }

        add.extend(patch.add_elems());
        remove.extend(patch.remove_elems());
    }

    if any {
        Ok(Some(SetPatchOps {
            override_value,
            add,
            remove,
        }))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod test {
    use super::{FeatureSetVecPatch, StringSetPatch, TargetListPatch};
    use color_eyre::eyre;
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn string_set_patch_array_is_override() -> eyre::Result<()> {
        let v = json!(["a", "b"]);
        let p: StringSetPatch = serde_json::from_value(v)?;
        let mut expected: HashSet<String> = HashSet::new();
        expected.insert("a".to_string());
        expected.insert("b".to_string());

        assert_eq!(p.override_value(), Some(&expected));
        assert!(p.add_values().is_empty());
        assert!(p.remove_values().is_empty());
        Ok(())
    }

    #[test]
    fn string_set_patch_object_add_remove() -> eyre::Result<()> {
        let v = json!({"add": ["a"], "remove": ["b"]});
        let p: StringSetPatch = serde_json::from_value(v)?;
        assert!(p.override_value().is_none());
        assert!(p.add_values().contains("a"));
        assert!(p.remove_values().contains("b"));
        Ok(())
    }

    #[test]
    fn feature_set_vec_patch_array_is_override() -> eyre::Result<()> {
        let v = json!([["a"], ["b", "c"]]);
        let p: FeatureSetVecPatch = serde_json::from_value(v)?;
        assert!(p.override_value().is_some());
        assert!(p.add_values().is_empty());
        assert!(p.remove_values().is_empty());
        Ok(())
    }

    fn hs(values: &[&str]) -> HashSet<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn combine_set_patches_unifies_string_sets() -> eyre::Result<()> {
        // One generic engine handles the `HashSet<String>` element type.
        let base = hs(&["default"]);
        let add: StringSetPatch = serde_json::from_value(json!({ "add": ["cuda"] }))?;
        let remove: StringSetPatch = serde_json::from_value(json!({ "remove": ["default"] }))?;

        let ops = super::combine_set_patches(
            "exclude_features",
            "target override",
            [("cfg(a)", &add), ("cfg(b)", &remove)],
        )?
        .expect("patches present");

        assert_eq!(ops.apply_to(&base), hs(&["cuda"]));
        Ok(())
    }

    #[test]
    fn combine_set_patches_unifies_feature_set_lists() -> eyre::Result<()> {
        // The same engine handles the `Vec<HashSet<String>>` element type.
        let base = vec![hs(&["a"])];
        let add: FeatureSetVecPatch = serde_json::from_value(json!({ "add": [["b", "c"]] }))?;

        let ops = super::combine_set_patches(
            "include_feature_sets",
            "target override",
            [("cfg(a)", &add)],
        )?
        .expect("patch present");

        let mut got = ops.apply_to_feature_sets(&base);
        got.sort_by_key(super::normalize_feature_set);
        assert_eq!(got, vec![hs(&["a"]), hs(&["b", "c"])]);
        Ok(())
    }

    #[test]
    fn combine_set_patches_reports_conflicting_overrides() {
        let a: StringSetPatch = serde_json::from_value(json!(["x"])).unwrap();
        let b: StringSetPatch = serde_json::from_value(json!(["y"])).unwrap();

        let err = super::combine_set_patches(
            "exclude_features",
            "target override",
            [("cfg(a)", &a), ("cfg(b)", &b)],
        )
        .expect_err("conflicting overrides must error");
        assert!(err.to_string().contains("conflicting overrides"));
    }

    #[test]
    fn target_list_patch_array_is_ordered_override() -> eyre::Result<()> {
        let patch: TargetListPatch = serde_json::from_value(json!(["b", "a", "b"]))?;

        let ops = super::combine_target_list_patches("targets", "package config", [("", &patch)])?
            .expect("patch present");

        assert_eq!(ops.apply_to(&["base".to_string()]), vec!["b", "a"]);
        Ok(())
    }

    #[test]
    fn target_list_patch_object_applies_in_declaration_order() -> eyre::Result<()> {
        let patch: TargetListPatch =
            serde_json::from_value(json!({ "remove": ["base"], "add": ["z", "a", "z"] }))?;

        let ops = super::combine_target_list_patches("targets", "package config", [("", &patch)])?
            .expect("patch present");

        assert_eq!(
            ops.apply_to(&["base".to_string(), "kept".to_string()]),
            vec!["kept", "z", "a"],
        );
        Ok(())
    }

    #[test]
    fn target_list_patch_conflicting_overrides_error() -> eyre::Result<()> {
        let a: TargetListPatch = serde_json::from_value(json!(["a", "b"]))?;
        let b: TargetListPatch = serde_json::from_value(json!(["b", "a"]))?;

        let err = super::combine_target_list_patches(
            "targets",
            "workspace target override",
            [("cfg(a)", &a), ("cfg(b)", &b)],
        )
        .expect_err("ordered overrides differ");

        assert!(err.to_string().contains("conflicting overrides"));
        Ok(())
    }
}
