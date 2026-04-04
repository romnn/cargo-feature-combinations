use crate::target::TargetTriple;
use cfg_expr::{Expression, Predicate};
use color_eyre::eyre::{self, WrapErr};
use std::collections::{HashMap, HashSet};
use std::process::Command;

/// Evaluate Cargo-style `cfg(...)` expressions for a specific target.
///
/// The default implementation uses `rustc --print cfg --target <triple>` to
/// obtain the active cfg set, and `cfg-expr` to parse and evaluate expressions.
pub trait CfgEvaluator {
    /// Return whether the given cfg expression matches the provided target.
    ///
    /// # Errors
    ///
    /// Returns an error if the cfg expression cannot be parsed or evaluated.
    fn matches(&mut self, cfg_expr: &str, target: &TargetTriple) -> eyre::Result<bool>;
}

#[derive(Debug, Default, Clone)]
struct RustcCfgSet {
    flags: HashSet<String>,
    key_values: HashMap<String, HashSet<String>>,
}

impl RustcCfgSet {
    fn from_rustc_print_cfg(output: &str) -> Self {
        let mut set = Self::default();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some((key, val)) = line.split_once('=') {
                let key = key.trim();
                let mut val = val.trim();
                if let Some(stripped) = val.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                    val = stripped;
                }
                set.key_values
                    .entry(key.to_string())
                    .or_default()
                    .insert(val.to_string());
            } else {
                set.flags.insert(line.to_string());
            }
        }
        set
    }

    fn has_flag(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }

    fn has_kv(&self, key: &str, val: &str) -> bool {
        self.key_values
            .get(key)
            .is_some_and(|vals| vals.contains(val))
    }
}

/// `CfgEvaluator` implementation backed by `rustc --print cfg`.
///
/// Results are cached per target triple for the duration of this evaluator.
#[derive(Debug, Default)]
pub struct RustcCfgEvaluator {
    cache: HashMap<String, RustcCfgSet>,
}

impl RustcCfgEvaluator {
    fn cfg_set_for(&mut self, target: &TargetTriple) -> eyre::Result<&RustcCfgSet> {
        let key = target.as_str();

        if !self.cache.contains_key(key) {
            let output = Command::new("rustc")
                .args(["--print", "cfg", "--target", key])
                .output()
                .wrap_err_with(|| {
                    format!("failed to invoke rustc to obtain cfg set for target `{key}`")
                })?;

            if !output.status.success() {
                eyre::bail!(
                    "rustc --print cfg --target {} failed with exit code {:?}",
                    key,
                    output.status.code()
                );
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let set = RustcCfgSet::from_rustc_print_cfg(&stdout);
            self.cache.insert(key.to_string(), set);
        }

        self.cache
            .get(key)
            .ok_or_else(|| eyre::eyre!("failed to cache rustc cfg set"))
    }

    fn validate_supported(expr: &Expression) -> eyre::Result<()> {
        for pred in expr.predicates() {
            if let Predicate::Feature(_) = pred {
                eyre::bail!(
                    "cfg expressions using `feature = \"...\"` are not supported in cargo-feature-combinations target overrides"
                )
            }
        }
        Ok(())
    }
}

fn endian_str(e: cfg_expr::targets::Endian) -> &'static str {
    match e {
        cfg_expr::targets::Endian::big => "big",
        cfg_expr::targets::Endian::little => "little",
    }
}

impl CfgEvaluator for RustcCfgEvaluator {
    fn matches(&mut self, cfg_expr: &str, target: &TargetTriple) -> eyre::Result<bool> {
        let set = self.cfg_set_for(target)?;

        let expr = Expression::parse(cfg_expr)
            .wrap_err_with(|| format!("failed to parse cfg expression `{cfg_expr}`"))?;
        Self::validate_supported(&expr)?;

        Ok(expr.eval(|pred| match pred {
            Predicate::Target(tp) => {
                // For target_* predicates, `rustc --print cfg` provides exact
                // results even for custom targets, so we evaluate by direct
                // membership in the cfg set.
                //
                // We still special-case `TargetPredicate` evaluation by relying
                // on rustc output rather than builtin target tables.
                match tp {
                    cfg_expr::expr::TargetPredicate::Arch(a) => {
                        set.has_kv("target_arch", a.as_ref())
                    }
                    cfg_expr::expr::TargetPredicate::Os(o) => set.has_kv("target_os", o.as_ref()),
                    cfg_expr::expr::TargetPredicate::Env(e) => set.has_kv("target_env", e.as_ref()),
                    cfg_expr::expr::TargetPredicate::Family(f) => {
                        set.has_kv("target_family", f.as_ref())
                    }
                    cfg_expr::expr::TargetPredicate::Vendor(v) => {
                        set.has_kv("target_vendor", v.as_ref())
                    }
                    cfg_expr::expr::TargetPredicate::Abi(a) => set.has_kv("target_abi", a.as_ref()),
                    cfg_expr::expr::TargetPredicate::Endian(e) => {
                        set.has_kv("target_endian", endian_str(*e))
                    }
                    cfg_expr::expr::TargetPredicate::Panic(p) => set.has_kv("panic", p.as_ref()),
                    cfg_expr::expr::TargetPredicate::PointerWidth(w) => {
                        set.has_kv("target_pointer_width", &w.to_string())
                    }
                    cfg_expr::expr::TargetPredicate::HasAtomic(a) => {
                        set.has_kv("target_has_atomic", &a.to_string())
                    }
                }
            }
            Predicate::TargetFeature(feat) => set.has_kv("target_feature", feat),
            Predicate::Flag(name) => set.has_flag(name),
            Predicate::KeyValue { key, val } => set.has_kv(key, val),
            Predicate::Test => set.has_flag("test"),
            Predicate::DebugAssertions => set.has_flag("debug_assertions"),
            Predicate::ProcMacro => set.has_flag("proc_macro"),
            Predicate::Feature(_feat) => false,
        }))
    }
}

#[cfg(test)]
mod test {
    use super::{CfgEvaluator, RustcCfgEvaluator};
    use crate::target::TargetDetector;
    use color_eyre::eyre;

    #[test]
    fn matches_simple_true_for_target_arch() -> eyre::Result<()> {
        let mut eval = RustcCfgEvaluator::default();
        let host = crate::target::RustcTargetDetector::default().detect_target(&Vec::new())?;

        // Host must match its own arch.
        let cfg_set = std::process::Command::new("rustc")
            .args(["--print", "cfg"])
            .output()?;
        assert!(cfg_set.status.success());
        let stdout = String::from_utf8_lossy(&cfg_set.stdout);
        let arch = stdout
            .lines()
            .find_map(|l| {
                l.strip_prefix("target_arch=\"")
                    .and_then(|r| r.strip_suffix("\""))
            })
            .ok_or_else(|| {
                eyre::eyre!("expected rustc --print cfg output to contain target_arch")
            })?;

        let expr = format!("cfg(target_arch = \"{arch}\")");
        assert!(eval.matches(&expr, &host)?);
        Ok(())
    }

    #[test]
    fn rejects_feature_predicate() -> eyre::Result<()> {
        let mut eval = RustcCfgEvaluator::default();
        let host = crate::target::RustcTargetDetector::default().detect_target(&Vec::new())?;

        let err = match eval.matches(r#"cfg(feature = "foo")"#, &host) {
            Ok(v) => eyre::bail!("expected cfg(feature=...) to be rejected, got {v}"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("not supported"));

        Ok(())
    }
}
