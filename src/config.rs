use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
    #[serde(default)]
    pub skip_feature_sets: Vec<HashSet<String>>,
    #[serde(default)]
    pub denylist: HashSet<String>,
    #[serde(default)]
    pub matrix: HashMap<String, serde_json::Value>,
}
