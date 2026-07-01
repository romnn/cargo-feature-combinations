use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Combined patch operations for a set-like string field.
#[derive(Debug, Clone)]
pub(crate) struct StringSetPatchOps {
    override_value: Option<HashSet<String>>,
    add: HashSet<String>,
    remove: HashSet<String>,
}

impl StringSetPatchOps {
    /// Apply the combined patch to `base`.
    ///
    /// The order is: start from override (or base), then remove, then add.
    /// This means if a value appears in both `add` and `remove`, add wins.
    #[must_use]
    pub(crate) fn apply_to(&self, base: &HashSet<String>) -> HashSet<String> {
        let mut out = if let Some(value) = &self.override_value {
            value.clone()
        } else {
            base.clone()
        };

        for value in &self.remove {
            out.remove(value);
        }
        out.extend(self.add.iter().cloned());
        out
    }
}

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

pub(crate) fn combine_string_set_patches<'a>(
    name: &str,
    source_kind: &str,
    patches: impl IntoIterator<Item = (&'a str, &'a StringSetPatch)>,
) -> color_eyre::eyre::Result<Option<StringSetPatchOps>> {
    let mut any = false;
    let mut override_value: Option<HashSet<String>> = None;
    let mut add: HashSet<String> = HashSet::new();
    let mut remove: HashSet<String> = HashSet::new();

    for (expr, patch) in patches {
        any = true;

        if let Some(value) = patch.override_value() {
            match &override_value {
                None => override_value = Some(value.clone()),
                Some(existing) if existing == value => {}
                Some(_) => {
                    color_eyre::eyre::bail!(
                        "conflicting overrides for `{name}` from {source_kind} `{expr}`"
                    );
                }
            }
        }

        add.extend(patch.add_values().iter().cloned());
        remove.extend(patch.remove_values().iter().cloned());
    }

    if any {
        Ok(Some(StringSetPatchOps {
            override_value,
            add,
            remove,
        }))
    } else {
        Ok(None)
    }
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

#[cfg(test)]
mod test {
    use super::{FeatureSetVecPatch, StringSetPatch};
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
}
