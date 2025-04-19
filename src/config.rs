use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
    #[serde(default)]
    pub isolated_feature_sets: Vec<HashSet<String>>,
    #[serde(default)]
    pub skip_feature_sets: Vec<HashSet<String>>,
    #[serde(default)]
    pub denylist: HashSet<String>,
    #[serde(default)]
    pub exact_combinations: Vec<HashSet<String>>,
    #[serde(default)]
    pub exclude_packages: Vec<String>,
    #[serde(default)]
    pub matrix: HashMap<String, serde_json::Value>,
}
