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
    let default = DefaultDriver::resolve(plan_set);
    for plan in &mut plan_set.plans {
        let fallback = default.for_target(&plan.target);
        for pp in &mut plan.package_plans {
            pp.driver = finalize_driver(pp.driver.as_deref(), fallback)?;
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
    /// The probed cross driver. `None` when no plan needs it or when the probe
    /// failed, which [`Self::resolve`] warned about.
    cross_driver: Option<String>,
}

impl DefaultDriver {
    /// Resolve the default for this plan set, immutable from here on.
    ///
    /// [`CROSS_TARGET_DRIVER`] is probed (and a failed probe warned about)
    /// exactly when some cross-target plan leaves a driver unset — a host-only
    /// run or one with explicit drivers everywhere spawns and prints nothing.
    fn resolve(plan_set: &ExecutionPlanSet<'_>) -> Self {
        let mut default = Self {
            host: plan_set.host.clone(),
            cross_driver: None,
        };
        let needs_cross_default = plan_set.plans.iter().any(|plan| {
            default.needs_cross_driver(&plan.target)
                && plan.package_plans.iter().any(|pp| pp.driver.is_none())
        });
        if needs_cross_default {
            default.cross_driver = match probe_driver(CROSS_TARGET_DRIVER) {
                Ok(()) => Some(CROSS_TARGET_DRIVER.to_string()),
                Err(failure) => {
                    let detail = failure
                        .detail
                        .map(|detail| format!("\n{detail}"))
                        .unwrap_or_default();
                    print_warning!(
                        "build driver `{CROSS_TARGET_DRIVER}` was selected automatically for a cross-target run but {}; using plain cargo{detail}",
                        failure.reason
                    );
                    None
                }
            };
        }
        default
    }

    /// Whether `target` is a cross target that plain cargo cannot be trusted to
    /// build. An undetectable host answers `false` for every target.
    fn needs_cross_driver(&self, target: &TargetTriple) -> bool {
        self.host
            .as_ref()
            .is_some_and(|host| crate::target::is_cross(host, target))
    }

    /// The driver to fall back to for `target`, or `None` for plain cargo.
    fn for_target(&self, target: &TargetTriple) -> Option<&str> {
        if self.needs_cross_driver(target) {
            self.cross_driver.as_deref()
        } else {
            None
        }
    }
}

/// Why an automatic driver cannot be used, split so the caller's warning can
/// finish its sentence before any multi-line probe output.
struct ProbeFailure {
    /// Clause completing "…was selected automatically but {reason}".
    reason: String,
    /// The probe's stderr, when it produced any.
    detail: Option<String>,
}

/// Check that `driver --version` runs and succeeds.
///
/// A non-zero exit is treated as unavailable just like a missing binary: a
/// broken installation (for example a tool-manager shim with no version
/// configured) would otherwise pass the probe and then fail every cross row
/// with no diagnostics.
fn probe_driver(driver: &str) -> Result<(), ProbeFailure> {
    match Command::new(driver)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            Err(ProbeFailure {
                reason: format!("`{driver} --version` failed with {}", output.status),
                detail: (!stderr.is_empty()).then(|| stderr.to_string()),
            })
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Err(ProbeFailure {
            reason: "was not found".to_string(),
            detail: None,
        }),
        Err(err) => Err(ProbeFailure {
            reason: format!("could not be probed: {err}"),
            detail: None,
        }),
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
    use super::{DefaultDriver, finalize_driver, probe_driver};
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
        let default = DefaultDriver {
            host: target::detect_host(&DriverTestEnv { host: Some("host") }),
            cross_driver: None,
        };

        assert!(!default.needs_cross_driver(&TargetTriple("host".to_string())));
    }

    /// The host target keeps plain cargo even when the same run also plans a
    /// cross target, so its artifacts stay interchangeable with an ordinary
    /// `cargo` build.
    #[test]
    fn default_driver_is_per_target_not_per_run() {
        let default = DefaultDriver {
            host: target::detect_host(&DriverTestEnv { host: Some("host") }),
            cross_driver: Some("cargo-zigbuild".to_string()),
        };

        assert_eq!(default.for_target(&TargetTriple("host".to_string())), None);
        assert_eq!(
            default.for_target(&TargetTriple("wasm".to_string())),
            Some("cargo-zigbuild")
        );

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
        let default = DefaultDriver {
            host: target::detect_host(&DriverTestEnv { host: None }),
            cross_driver: Some("cargo-zigbuild".to_string()),
        };

        assert!(!default.needs_cross_driver(&TargetTriple("wasm".to_string())));
        assert_eq!(default.for_target(&TargetTriple("wasm".to_string())), None);
    }

    #[test]
    fn probe_driver_accepts_working_driver() {
        // rustc is on PATH for every test run of this crate and its
        // `--version` exits successfully.
        assert!(probe_driver("rustc").is_ok());
    }

    #[test]
    fn probe_driver_reports_missing_driver() {
        let failure = probe_driver("cargo-fc-test-missing-driver").unwrap_err();
        assert_eq!(failure.reason, "was not found");
        assert!(failure.detail.is_none());
    }

    /// A driver that spawns but cannot report a version (e.g. a tool-manager
    /// shim with no version configured) must fail the probe with its stderr
    /// preserved, not count as available.
    #[cfg(unix)]
    #[test]
    fn probe_driver_rejects_failing_version_probe() -> eyre::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = assert_fs::TempDir::new()?;
        let script = dir.path().join("broken-driver");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'shim: no version set' >&2\nexit 3\n",
        )?;
        let mut permissions = std::fs::metadata(&script)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions)?;

        let failure = probe_driver(&script.to_string_lossy()).unwrap_err();
        assert!(failure.reason.contains("failed with"), "{}", failure.reason);
        assert_eq!(failure.detail.as_deref(), Some("shim: no version set"));
        Ok(())
    }
}
