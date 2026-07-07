//! Shared deterministic stub adapters for the integration tests.
//!
//! These let the planning + matrix API run end to end over real
//! `cargo metadata` without any installed target or real `rustc`/`cargo`
//! invocation.

#![allow(
    dead_code,
    reason = "shared integration-test fixtures; not every test binary uses every item"
)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use cargo_feature_combinations::CfgEvaluator;
use cargo_feature_combinations::target::{TargetEnvironment, TargetTriple};
use color_eyre::eyre;

/// A fixed host-target environment with no `CARGO_BUILD_TARGET`.
pub struct HostEnv(pub &'static str);

impl TargetEnvironment for HostEnv {
    fn cargo_build_target(&self) -> Option<String> {
        None
    }
    fn host_target(&self) -> eyre::Result<TargetTriple> {
        Ok(TargetTriple(self.0.to_string()))
    }
}

/// cfg evaluator matching `(triple, cfg)` pairs exactly, with no real `rustc`.
#[derive(Default)]
pub struct PairEval {
    rules: Vec<(String, String)>,
}

impl PairEval {
    pub fn matching(triple: &str, cfg: &str) -> Self {
        Self {
            rules: vec![(triple.to_string(), cfg.to_string())],
        }
    }
}

impl CfgEvaluator for PairEval {
    fn matches(&mut self, cfg_expr: &str, target: &TargetTriple) -> eyre::Result<bool> {
        Ok(self
            .rules
            .iter()
            .any(|(t, c)| t == target.as_str() && c == cfg_expr))
    }
}

/// Create a single-crate temp workspace from the given `Cargo.toml` contents.
pub fn single_crate(cargo_toml: &str) -> eyre::Result<TempDir> {
    let temp = TempDir::new()?;
    temp.child("Cargo.toml").write_str(cargo_toml)?;
    temp.child("src/lib.rs").write_str("pub fn x() {}\n")?;
    Ok(temp)
}

/// Run `cargo metadata` (no deps) against a temp workspace.
pub fn metadata(temp: &TempDir) -> eyre::Result<cargo_metadata::Metadata> {
    Ok(cargo_metadata::MetadataCommand::new()
        .current_dir(temp.path())
        .no_deps()
        .exec()?)
}
