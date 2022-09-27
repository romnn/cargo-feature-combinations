use super::config::Config;
use itertools::Itertools;
use std::collections::HashSet;

pub trait Dependency {
    fn as_feature(&self) -> Option<String>;
}

pub trait Package {
    fn optional_dependencies(&self) -> Vec<String>;
    fn config(&self) -> Config;
}

impl Dependency for cargo_metadata::Dependency {
    fn as_feature(&self) -> Option<String> {
        self.optional
            .then(|| self.rename.as_ref().unwrap_or(&self.name))
            .cloned()
        // .map(Feature)
    }
}

impl Package for cargo_metadata::Package {
    fn optional_dependencies(&self) -> Vec<String> {
        self.dependencies
            .iter()
            .filter_map(Dependency::as_feature)
            .collect()
    }
}

pub fn feature_combinations<'a>(
    package: &'a cargo_metadata::Package,
    config: &'a Config,
) -> impl Iterator<Item = HashSet<String>> + 'a {
    // let len = features.len();
    package
        .features
        .keys()
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|ft| !config.denylist.contains(*ft))
        .powerset()
        .filter_map(|set| {
            let set: HashSet<_> = set.into_iter().cloned().collect();
            let skip = config
                .skip_feature_sets
                .iter()
                // .any(|skip_set| skip_set.as_ref().difference(&set).count() == 0);
                .any(|skip_set| skip_set.is_subset(&set));
            if skip {
                None
            } else {
                Some(set)
            }
        })
}

// pub fn feature_combinations<'a>(
//     features: impl Iterator<Item = &'a String>,
// ) -> impl Iterator<Item = Vec<&'a String>> {
//     // let len = features.len();
//     features.powerset()
// }
