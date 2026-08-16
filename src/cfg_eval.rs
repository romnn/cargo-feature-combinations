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
#[derive(Debug)]
pub struct RustcCfgEvaluator {
    cache: HashMap<String, RustcCfgSet>,
    /// The run's host triple, resolved once by the caller. `None` records that
    /// host detection failed: evaluating `cfg(cross)` then errors, while
    /// host-independent expressions are unaffected.
    host: Option<TargetTriple>,
}

impl RustcCfgEvaluator {
    /// Create an evaluator for a run whose host resolved to `host`.
    #[must_use]
    pub fn new(host: Option<TargetTriple>) -> Self {
        Self {
            cache: HashMap::new(),
            host,
        }
    }

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
                // Classify the failure at this adapter boundary: surface rustc's
                // own reason, and add a rustup hint when the triple looks like a
                // valid-but-not-installed target. (Note: `rustc --print cfg`
                // succeeds for known targets even when their std is missing, so
                // in practice this path is reached for unknown/invalid triples;
                // the rustup hint is only added when rustc says so.)
                use std::fmt::Write as _;
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                let mut message = format!("failed to read cfg for target `{key}`");
                if !stderr.is_empty() {
                    message.push('\n');
                    message.push_str(stderr);
                }
                if stderr.contains("not installed") || stderr.contains("rustup target add") {
                    let _ = write!(message, "\nhint: run `rustup target add {key}`");
                }
                eyre::bail!(message);
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
            match pred {
                Predicate::Feature(_) => eyre::bail!(
                    "cfg expressions using `feature = \"...\"` are not supported in cargo-feature-combinations target overrides"
                ),
                // Bare identifiers reach `Flag` only when cfg-expr has no typed
                // predicate for them (`unix`, `windows`, `test`,
                // `debug_assertions` and `proc_macro` are typed), so anything
                // but cargo-fc's own `cross` is a typo or a custom `--cfg` that
                // `rustc --print cfg` can never report. Evaluating such a flag
                // would silently be `false` and disable the override with no
                // diagnostic, so fail loudly instead.
                Predicate::Flag(name) if name != "cross" => eyre::bail!(
                    "unknown cfg flag `{name}` in a cargo-feature-combinations target override; the only supported bare flag is `cross`, which matches when the evaluated target differs from the rustc host"
                ),
                _ => {}
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
        let expr = Expression::parse(cfg_expr)
            .wrap_err_with(|| format!("failed to parse cfg expression `{cfg_expr}`"))?;
        Self::validate_supported(&expr)?;

        // `cross` compares the evaluated target against the rustc host. A
        // failed host detection is reported here — at the expression that
        // needs the host — so the eval closure below stays infallible.
        let uses_cross = expr
            .predicates()
            .any(|pred| matches!(pred, Predicate::Flag("cross")));
        let is_cross = if uses_cross {
            let host = self.host.as_ref().ok_or_else(|| {
                eyre::eyre!(
                    "`cfg(cross)` requires the rustc host target, which could not be detected"
                )
            })?;
            crate::target::is_cross(host, target)
        } else {
            false
        };

        let set = self.cfg_set_for(target)?;

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
            // `validate_supported` rejects every bare flag except `cross`.
            Predicate::Flag(_name) => is_cross,
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
    use crate::target::{TargetTriple, host_triple};
    use color_eyre::eyre;

    /// Real triples usable as an injected host and a cross target on any
    /// machine: both spellings ship in every rustc target list, and
    /// `rustc --print cfg` works without the target installed.
    fn linux_x86() -> TargetTriple {
        TargetTriple("x86_64-unknown-linux-gnu".to_string())
    }

    fn linux_aarch64() -> TargetTriple {
        TargetTriple("aarch64-unknown-linux-gnu".to_string())
    }

    #[test]
    fn matches_simple_true_for_target_arch() -> eyre::Result<()> {
        let mut eval = RustcCfgEvaluator::new(None);
        let host = host_triple()?;

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
        let mut eval = RustcCfgEvaluator::new(None);

        let err = match eval.matches(r#"cfg(feature = "foo")"#, &linux_x86()) {
            Ok(v) => eyre::bail!("expected cfg(feature=...) to be rejected, got {v}"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("not supported"));

        Ok(())
    }

    #[test]
    fn cross_flag_tracks_host_vs_target() -> eyre::Result<()> {
        let host = linux_x86();
        let cross = linux_aarch64();
        let mut eval = RustcCfgEvaluator::new(Some(host.clone()));

        assert!(!eval.matches("cfg(cross)", &host)?);
        assert!(eval.matches("cfg(not(cross))", &host)?);
        assert!(eval.matches("cfg(cross)", &cross)?);
        assert!(eval.matches(r#"cfg(all(cross, target_os = "linux"))"#, &cross)?);

        // `cross` is an ordinary predicate to the expression evaluator, so it
        // composes with every combinator at any nesting depth.
        assert!(eval.matches("cfg(any(cross, windows))", &cross)?);
        assert!(!eval.matches("cfg(any(cross, windows))", &host)?);
        assert!(eval.matches("cfg(not(any(cross, windows)))", &host)?);
        assert!(eval.matches(
            r#"cfg(all(unix, not(all(cross, target_arch = "x86_64"))))"#,
            &cross
        )?);
        Ok(())
    }

    #[test]
    fn cross_errors_when_host_is_unknown() -> eyre::Result<()> {
        let mut eval = RustcCfgEvaluator::new(None);

        let err = match eval.matches("cfg(cross)", &linux_x86()) {
            Ok(v) => eyre::bail!("expected cfg(cross) without a host to fail, got {v}"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("could not be detected"), "{err}");
        Ok(())
    }

    #[test]
    fn rejects_unknown_bare_flag() -> eyre::Result<()> {
        let mut eval = RustcCfgEvaluator::new(None);

        // spellcheck:ignore-next-line -- a deliberate misspelling of `cross` is under test
        let err = match eval.matches("cfg(corss)", &linux_x86()) {
            // spellcheck:ignore-next-line
            Ok(v) => eyre::bail!("expected cfg(corss) to be rejected, got {v}"),
            Err(err) => err,
        };
        assert!(
            // spellcheck:ignore-next-line
            err.to_string().contains("unknown cfg flag `corss`"),
            "{err}"
        );
        Ok(())
    }

    /// Bare identifiers that cfg-expr types (`unix`, `test`, and friends) must
    /// not trip the unknown-flag rejection.
    #[test]
    fn typed_bare_identifiers_still_evaluate() -> eyre::Result<()> {
        let mut eval = RustcCfgEvaluator::new(None);

        assert!(eval.matches("cfg(unix)", &linux_aarch64())?);
        assert!(!eval.matches("cfg(windows)", &linux_aarch64())?);
        Ok(())
    }
}
