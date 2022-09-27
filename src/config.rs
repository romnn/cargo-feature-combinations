use serde::{Deserialize, Serialize};
use serde_json::Result;
use std::collections::HashSet;

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
    #[serde(default)]
    pub skip_feature_sets: Vec<HashSet<String>>,
    // #[serde(default)]
    // pub skip_optional_dependencies: bool,
    // #[serde(default)]
    // pub extra_features: HashSet<String>,
    #[serde(default)]
    pub denylist: HashSet<String>,
    // #[serde(default)]
    // pub allowlist: HashSet<String>,
}
