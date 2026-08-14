use crate::plan::execution::ExecutionPlanSet;
use crate::print_warning;
use crate::target::TargetTriple;
use color_eyre::eyre;
use std::io;
use std::process::{Command, Stdio};

/// The driver cargo-fc reaches for on its own when a target cannot be built by
/// plain cargo.
const CROSS_TARGET_DRIVER: &str = "cargo-zigbuild";

/// Finalize the spawned build driver for every package-target plan.
///
/// Config + `--driver` are already resolved per (package x target x command)
/// into [`crate::plan::execution::PackageExecutionPlan::driver`]. This pass
/// turns each into the program actually spawned: an explicit config/CLI driver
/// is normalized (`"cargo"` -> plain `$CARGO`), while an unset driver falls
/// back to cargo-fc's built-in default for that plan's target.
pub(crate) fn finalize_plan_drivers(plan_set: &mut ExecutionPlanSet) -> eyre::Result<()> {
    let needs_default = plan_set
        .plans
        .iter()
        .flat_map(|plan| &plan.package_plans)
        .any(|pp| pp.driver.is_none());
    let mut default = needs_default.then(|| DefaultDriver::new(plan_set.host.clone()));

    for plan in &mut plan_set.plans {
        let fallback = default
            .as_mut()
            .and_then(|default| default.for_target(&plan.target));
        for pp in &mut plan.package_plans {
            pp.driver = finalize_driver(pp.driver.as_deref(), fallback.as_deref())?;
        }
    }
    Ok(())
}

/// cargo-fc's built-in driver default, resolved per target.
///
/// The default belongs to the target, not to the run: a plan for the host
/// target compiles exactly the way an ordinary `cargo` invocation would, so
/// routing it through the cross driver only makes its artifacts
/// fingerprint-incompatible with everyday `cargo check` / `cargo test` in the
/// same target directory — the two then invalidate each other on every switch.
/// Only the targets plain cargo cannot build get [`CROSS_TARGET_DRIVER`].
struct DefaultDriver {
    /// `None` when host detection failed. Every target then falls back to plain
    /// cargo, mirroring missing-target installation behavior.
    host: Option<TargetTriple>,
    /// Whether [`CROSS_TARGET_DRIVER`] has been looked up yet, and what the
    /// lookup found.
    cross: CrossDriverProbe,
}

/// Result of looking up [`CROSS_TARGET_DRIVER`] on `PATH`, memoized so a run
/// with many cross targets probes (and warns) once.
enum CrossDriverProbe {
    Unprobed,
    Available,
    Missing,
}

impl DefaultDriver {
    fn new(host: Option<TargetTriple>) -> Self {
        Self {
            host,
            cross: CrossDriverProbe::Unprobed,
        }
    }

    /// Whether `target` is a cross target that plain cargo cannot be trusted to
    /// build. An undetectable host answers `false` for every target.
    fn needs_cross_driver(&self, target: &TargetTriple) -> bool {
        self.host.as_ref().is_some_and(|host| host != target)
    }

    /// The driver to fall back to for `target`, or `None` for plain cargo.
    ///
    /// The cross driver is probed at most once per run, and only if a cross
    /// target actually reaches this.
    fn for_target(&mut self, target: &TargetTriple) -> Option<String> {
        if !self.needs_cross_driver(target) {
            return None;
        }
        if matches!(self.cross, CrossDriverProbe::Unprobed) {
            self.cross = if driver_is_available(CROSS_TARGET_DRIVER) {
                CrossDriverProbe::Available
            } else {
                print_warning!(
                    "build driver `{CROSS_TARGET_DRIVER}` was selected automatically for a cross-target run but was not found; using plain cargo"
                );
                CrossDriverProbe::Missing
            };
        }
        match self.cross {
            CrossDriverProbe::Available => Some(CROSS_TARGET_DRIVER.to_string()),
            CrossDriverProbe::Unprobed | CrossDriverProbe::Missing => None,
        }
    }
}

fn driver_is_available(driver: &str) -> bool {
    match Command::new(driver)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            print_warning!("could not probe build driver `{driver}`: {err}; using plain cargo");
            false
        }
    }
}

/// Turn a resolved per-plan driver into the spawned program: an explicit value
/// is normalized (`"cargo"` -> plain `$CARGO`), an unset value uses `default`.
pub(crate) fn finalize_driver(
    configured: Option<&str>,
    default: Option<&str>,
) -> eyre::Result<Option<String>> {
    match configured {
        Some(driver) => normalize_driver(driver),
        None => Ok(default.map(ToString::to_string)),
    }
}

fn normalize_driver(driver: &str) -> eyre::Result<Option<String>> {
    let driver = driver.trim();
    if driver.is_empty() {
        eyre::bail!("build driver (`--driver` or `driver`) must not be empty");
    }
    if driver == "cargo" {
        Ok(None)
    } else {
        Ok(Some(driver.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{DefaultDriver, finalize_driver};
    use crate::target::{self, TargetTriple};
    use color_eyre::eyre;

    struct DriverTestEnv {
        host: Option<&'static str>,
    }

    impl target::TargetEnvironment for DriverTestEnv {
        fn cargo_build_target(&self) -> Option<String> {
            None
        }

        fn host_target(&self) -> eyre::Result<target::TargetTriple> {
            let Some(host) = self.host else {
                eyre::bail!("host failed");
            };
            Ok(target::TargetTriple(host.to_string()))
        }
    }

    #[test]
    fn default_driver_is_plain_cargo_for_the_host_target() {
        let default =
            DefaultDriver::new(target::detect_host(&DriverTestEnv { host: Some("host") }));

        assert!(!default.needs_cross_driver(&TargetTriple("host".to_string())));
    }

    /// The host target keeps plain cargo even when the same run also plans a
    /// cross target, so its artifacts stay interchangeable with an ordinary
    /// `cargo` build.
    #[test]
    fn default_driver_is_per_target_not_per_run() {
        let default =
            DefaultDriver::new(target::detect_host(&DriverTestEnv { host: Some("host") }));

        assert!(!default.needs_cross_driver(&TargetTriple("host".to_string())));
        assert!(default.needs_cross_driver(&TargetTriple("wasm".to_string())));
    }

    #[test]
    fn finalize_driver_treats_explicit_cargo_as_plain_cargo() -> eyre::Result<()> {
        assert_eq!(
            finalize_driver(Some("cargo"), Some("cargo-zigbuild"))?,
            None
        );
        Ok(())
    }

    #[test]
    fn finalize_driver_uses_explicit_custom_driver() -> eyre::Result<()> {
        assert_eq!(
            finalize_driver(Some("cross"), None)?,
            Some("cross".to_string())
        );
        Ok(())
    }

    #[test]
    fn finalize_driver_uses_default_only_when_unset() -> eyre::Result<()> {
        assert_eq!(
            finalize_driver(None, Some("cargo-zigbuild"))?,
            Some("cargo-zigbuild".to_string())
        );
        assert_eq!(finalize_driver(None, None)?, None);
        Ok(())
    }

    #[test]
    fn finalize_driver_rejects_empty_driver() {
        assert!(finalize_driver(Some("   "), None).is_err());
    }

    #[test]
    fn default_driver_falls_back_to_plain_cargo_when_host_detection_fails() {
        let default = DefaultDriver::new(target::detect_host(&DriverTestEnv { host: None }));

        assert!(!default.needs_cross_driver(&TargetTriple("wasm".to_string())));
    }
}
