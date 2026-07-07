use crate::plan::execution::ExecutionPlanSet;
use crate::print_warning;
use crate::target::TargetEnvironment;
use color_eyre::eyre;
use std::io;
use std::process::{Command, Stdio};

/// Finalize the spawned build driver for every package-target plan.
///
/// Config + `--driver` are already resolved per (package x target x command)
/// into [`crate::plan::execution::PackageExecutionPlan::driver`]. This pass
/// turns each into the program actually spawned: an explicit config/CLI driver
/// is normalized (`"cargo"` -> plain `$CARGO`), while an unset driver falls
/// back to cargo-fc's cross-target default (`cargo-zigbuild` when available and
/// any planned target is a cross target, else plain `cargo`).
pub(crate) fn finalize_plan_drivers(
    plan_set: &mut ExecutionPlanSet,
    env: &impl TargetEnvironment,
) -> eyre::Result<()> {
    let needs_default = plan_set
        .plans
        .iter()
        .flat_map(|plan| &plan.package_plans)
        .any(|pp| pp.driver.is_none());
    let default = if needs_default {
        usable_default_driver(plan_set, env)
    } else {
        None
    };

    for plan in &mut plan_set.plans {
        for pp in &mut plan.package_plans {
            pp.driver = finalize_driver(pp.driver.as_deref(), default.as_deref())?;
        }
    }
    Ok(())
}

fn usable_default_driver(
    plan_set: &ExecutionPlanSet,
    env: &impl TargetEnvironment,
) -> Option<String> {
    let default = cross_target_default_driver(plan_set, env)?;
    if driver_is_available(&default) {
        Some(default)
    } else {
        print_warning!(
            "build driver `{default}` was selected automatically for a cross-target run but was not found; using plain cargo"
        );
        None
    }
}

/// cargo-fc's built-in driver default: `cargo-zigbuild` when any planned target
/// is a cross target, else plain `cargo`. Host detection failure degrades to
/// plain cargo with a warning, mirroring missing-target installation behavior.
pub(crate) fn cross_target_default_driver(
    plan_set: &ExecutionPlanSet,
    env: &impl TargetEnvironment,
) -> Option<String> {
    if plan_set.plans.is_empty() {
        return None;
    }
    let host = match env.host_target() {
        Ok(host) => host,
        Err(err) => {
            print_warning!(
                "could not detect host target to select a build driver: {err}; using plain cargo"
            );
            return None;
        }
    };
    let cross = plan_set.plans.iter().any(|plan| plan.target != host);
    cross.then(|| "cargo-zigbuild".to_string())
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
    use super::{cross_target_default_driver, finalize_driver};
    use crate::plan;
    use crate::target;
    use color_eyre::eyre;

    fn execution_plan_set(
        targets: &[&str],
        show_pruned: bool,
    ) -> plan::execution::ExecutionPlanSet<'static> {
        plan::execution::ExecutionPlanSet {
            plans: targets
                .iter()
                .map(|target| plan::execution::ExecutionPlan {
                    target: target::TargetTriple((*target).to_string()),
                    package_plans: Vec::new(),
                })
                .collect(),
            show_pruned,
            show_target: targets.len() > 1,
        }
    }

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
    fn cross_target_default_is_plain_cargo_for_host_only_plan() {
        let default = cross_target_default_driver(
            &execution_plan_set(&["host"], false),
            &DriverTestEnv { host: Some("host") },
        );

        assert_eq!(default, None);
    }

    #[test]
    fn cross_target_default_is_zigbuild_for_cross_plan() {
        let default = cross_target_default_driver(
            &execution_plan_set(&["host", "wasm"], false),
            &DriverTestEnv { host: Some("host") },
        );

        assert_eq!(default, Some("cargo-zigbuild".to_string()));
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
    fn cross_target_default_falls_back_to_plain_cargo_when_host_detection_fails() {
        let default = cross_target_default_driver(
            &execution_plan_set(&["wasm"], false),
            &DriverTestEnv { host: None },
        );

        assert_eq!(default, None);
    }
}
