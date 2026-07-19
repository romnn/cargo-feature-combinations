use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::process;

/// Patch operations for the environment of a matrix-cell Cargo process.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct EnvPatch {
    /// Replace the accumulated environment patch before applying this scope's
    /// removals and additions.
    #[serde(default)]
    pub r#override: Option<BTreeMap<String, EnvValue>>,
    /// Variables to set or replace.
    #[serde(default)]
    pub add: BTreeMap<String, EnvValue>,
    /// Variables to remove from the child environment.
    #[serde(default)]
    pub remove: BTreeSet<String>,
}

/// An environment variable value that redacts its contents when debugged.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct EnvValue(String);

impl fmt::Debug for EnvValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnvValue(<redacted>)")
    }
}

impl EnvValue {
    pub(crate) fn from_validated(value: String) -> Self {
        Self(value)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

/// A resolved patch applied on top of the ambient process environment.
///
/// The set and removal collections are private so a variable cannot be in both
/// states at once.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedEnv {
    set: BTreeMap<String, EnvValue>,
    remove: BTreeSet<String>,
}

impl ResolvedEnv {
    pub(crate) fn apply_patch(&mut self, operations: &CombinedEnvOps) {
        if let Some(value) = &operations.r#override {
            self.set.clone_from(value);
            self.remove.clear();
        }
        for name in &operations.remove {
            self.set.remove(name);
            self.remove.insert(name.clone());
        }
        for (name, value) in &operations.add {
            self.remove.remove(name);
            self.set.insert(name.clone(), value.clone());
        }
    }

    pub(crate) fn set(&mut self, name: String, value: EnvValue) {
        self.remove.remove(&name);
        self.set.insert(name, value);
    }

    pub(crate) fn remove(&mut self, name: &str) {
        self.set.remove(name);
        self.remove.insert(name.to_string());
    }

    /// Apply this patch to a child-process command without enumerating or
    /// clearing the ambient environment.
    pub fn apply_to(&self, command: &mut process::Command) {
        for name in &self.remove {
            command.env_remove(name);
        }
        for (name, value) in &self.set {
            command.env(name, value.expose());
        }
    }

    /// Return a variable as the child process will see it after this patch.
    #[must_use]
    pub fn effective_var(&self, key: &str) -> Option<OsString> {
        if self.remove.contains(key) {
            return None;
        }
        self.set
            .get(key)
            .map(|value| OsString::from(value.expose()))
            .or_else(|| std::env::var_os(key))
    }

    #[must_use]
    pub(crate) fn mentions(&self, key: &str) -> bool {
        self.set.contains_key(key) || self.remove.contains(key)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CombinedEnvOps {
    r#override: Option<BTreeMap<String, EnvValue>>,
    add: BTreeMap<String, EnvValue>,
    remove: BTreeSet<String>,
}

pub(crate) fn combine_env_patches<'a>(
    source_kind: &str,
    patches: impl IntoIterator<Item = (&'a str, &'a EnvPatch)>,
) -> color_eyre::eyre::Result<Option<CombinedEnvOps>> {
    let mut any = false;
    let mut r#override = None;
    let mut add = BTreeMap::new();
    let mut remove = BTreeSet::new();

    for (section, patch) in patches {
        any = true;
        if let Some(value) = &patch.r#override {
            match &r#override {
                None => r#override = Some(value.clone()),
                Some(existing) if existing == value => {}
                Some(_) => {
                    color_eyre::eyre::bail!(
                        "conflicting overrides for `env` from {source_kind} `{section}`"
                    );
                }
            }
        }
        for (name, value) in &patch.add {
            match add.get(name) {
                None => {
                    add.insert(name.clone(), value.clone());
                }
                Some(existing) if existing == value => {}
                Some(_) => {
                    color_eyre::eyre::bail!(
                        "conflicting values for `env.add.{name}` in {source_kind} `{section}`"
                    );
                }
            }
        }
        remove.extend(patch.remove.iter().cloned());
    }

    Ok(any.then_some(CombinedEnvOps {
        r#override,
        add,
        remove,
    }))
}

pub(crate) fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("must not be empty");
    }
    if name.contains('=') {
        return Err("must not contain `=`");
    }
    if name.contains('\0') {
        return Err("must not contain NUL");
    }
    Ok(())
}

pub(crate) fn validate_value(value: &str) -> Result<(), &'static str> {
    if value.contains('\0') {
        return Err("must not contain NUL");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CombinedEnvOps, EnvPatch, EnvValue, ResolvedEnv};
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::process::Command;

    #[test]
    fn patch_deserializes_all_operations() {
        let patch: EnvPatch = serde_json::from_value(json!({
            "override": { "BASE": "base" },
            "remove": ["OLD"],
            "add": { "NEW": "new" },
        }))
        .expect("deserialize env patch");

        assert!(patch.r#override.as_ref().is_some_and(|map| {
            serde_json::to_value(map).expect("serialize override") == json!({ "BASE": "base" })
        }));
        assert_eq!(patch.remove.into_iter().collect::<Vec<_>>(), ["OLD"]);
        assert_eq!(
            serde_json::to_value(patch.add).expect("serialize additions"),
            json!({ "NEW": "new" })
        );
    }

    #[test]
    fn value_debug_output_is_redacted() {
        let value: EnvValue =
            serde_json::from_value(json!("secret-value")).expect("deserialize environment value");

        assert_eq!(format!("{value:?}"), "EnvValue(<redacted>)");
    }

    #[test]
    fn resolved_patch_preserves_disjoint_set_and_remove_states() {
        let mut resolved = ResolvedEnv::default();
        resolved.apply_patch(&CombinedEnvOps {
            r#override: None,
            add: BTreeMap::from([(
                "SHARED".to_string(),
                serde_json::from_value(json!("set")).expect("deserialize value"),
            )]),
            remove: BTreeSet::from(["SHARED".to_string(), "REMOVED".to_string()]),
        });

        assert!(resolved.set.contains_key("SHARED"));
        assert!(!resolved.remove.contains("SHARED"));
        assert!(resolved.remove.contains("REMOVED"));
    }

    #[test]
    fn apply_to_records_sets_and_removals_without_clearing_ambient_env() {
        let mut resolved = ResolvedEnv::default();
        resolved.apply_patch(&CombinedEnvOps {
            r#override: None,
            add: BTreeMap::from([(
                "ADDED".to_string(),
                serde_json::from_value(json!("value")).expect("deserialize value"),
            )]),
            remove: BTreeSet::from(["REMOVED".to_string()]),
        });
        let mut command = Command::new("cargo");

        resolved.apply_to(&mut command);
        let env = command.get_envs().collect::<BTreeMap<_, _>>();

        assert_eq!(
            env.get(OsStr::new("ADDED")),
            Some(&Some(OsStr::new("value")))
        );
        assert_eq!(env.get(OsStr::new("REMOVED")), Some(&None));
    }
}
