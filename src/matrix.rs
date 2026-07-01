//! JSON matrix output built from execution plans.

use crate::plan::execution::ExecutionPlanSet;
use color_eyre::eyre;
use itertools::Itertools;

/// Build the JSON feature-matrix rows for the given execution plans.
///
/// Every row carries cargo-fc-owned top-level fields (`name`, `target`,
/// `features`) plus `metadata`, which contains the package's user-defined
/// matrix metadata.
#[must_use]
pub fn build_matrix_rows(plan_set: &ExecutionPlanSet) -> Vec<serde_json::Value> {
    let mut rows = Vec::new();

    for plan in &plan_set.plans {
        for pp in &plan.package_plans {
            let name = pp.package.name.to_string();
            let metadata = sorted_json_object(&pp.matrix);

            let features_list: Vec<String> = if pp.flags.packages_only {
                vec!["default".to_string()]
            } else {
                pp.combinations
                    .iter()
                    .map(|combo| combo.iter().join(","))
                    .collect()
            };

            for ft in features_list {
                let mut row = serde_json::Map::new();
                row.insert("features".to_string(), serde_json::json!(ft));
                row.insert("metadata".to_string(), metadata.clone());
                row.insert("name".to_string(), serde_json::json!(name.as_str()));
                row.insert(
                    "target".to_string(),
                    serde_json::json!(pp.target.triple.as_str()),
                );
                rows.push(serde_json::Value::Object(row));
            }
        }
    }

    rows
}

fn sorted_json_object(object: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    let mut entries: Vec<_> = object.iter().collect();
    entries.sort_by_key(|(key, _)| *key);

    let mut out = serde_json::Map::new();
    for (key, value) in entries {
        out.insert(key.clone(), sorted_json_value(value));
    }
    serde_json::Value::Object(out)
}

fn sorted_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(sorted_json_value).collect())
        }
        serde_json::Value::Object(object) => sorted_json_object(object),
        _ => value.clone(),
    }
}

/// Print a JSON feature matrix built from execution plans to stdout.
///
/// # Errors
///
/// Returns an error if serialization of the JSON matrix fails.
pub(crate) fn print_matrix_for_execution_plans(
    plan_set: &ExecutionPlanSet,
    pretty: bool,
) -> eyre::Result<()> {
    let rows = build_matrix_rows(plan_set);
    let matrix = if pretty {
        serde_json::to_string_pretty(&rows)
    } else {
        serde_json::to_string(&rows)
    }?;
    println!("{matrix}");
    Ok(())
}
