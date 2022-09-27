use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
    #[serde(default)]
    pub skip_feature_sets: Vec<HashSet<String>>,
    #[serde(default)]
    pub denylist: HashSet<String>,
}
