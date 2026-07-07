//! Optional Rust target installation via rustup.

use crate::plan::execution::ExecutionPlanSet;
use crate::print_note;
use crate::print_warning;
use crate::target::{TargetEnvironment, TargetTriple};
use color_eyre::eyre::{self, WrapErr};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Adapter for the toolchain manager used to install Rust target components.
pub(crate) trait TargetInstaller {
    /// Return the installed Rust target triples.
    ///
    /// # Errors
    ///
    /// Returns an error if the installer can not be invoked or its output can
    /// not be read.
    fn installed_targets(&self, working_dir: &Path) -> eyre::Result<BTreeSet<String>>;

    /// Install the missing target triples.
    ///
    /// # Errors
    ///
    /// Returns an error if installation fails.
    fn install_targets(&self, working_dir: &Path, targets: &[TargetTriple]) -> eyre::Result<()>;
}

/// Production target installer backed by `rustup`.
#[derive(Debug)]
pub(crate) struct RustupTargetInstaller {
    toolchain: Option<String>,
}

impl RustupTargetInstaller {
    pub(crate) fn new(toolchain: Option<String>) -> Self {
        Self {
            toolchain: toolchain.filter(|value| !value.is_empty()),
        }
    }

    fn apply_toolchain_arg(&self, command: &mut Command) {
        if let Some(toolchain) = &self.toolchain {
            command.args(["--toolchain", toolchain]);
        }
    }
}

impl TargetInstaller for RustupTargetInstaller {
    fn installed_targets(&self, working_dir: &Path) -> eyre::Result<BTreeSet<String>> {
        let mut command = Command::new("rustup");
        command.args(["target", "list", "--installed"]);
        self.apply_toolchain_arg(&mut command);
        command.current_dir(working_dir);

        let output = command
            .output()
            .wrap_err("failed to invoke rustup to list installed targets")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eyre::bail!(
                "rustup target list --installed failed with exit code {:?}: {}",
                output.status.code(),
                stderr.trim(),
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }

    fn install_targets(&self, working_dir: &Path, targets: &[TargetTriple]) -> eyre::Result<()> {
        if targets.is_empty() {
            return Ok(());
        }

        let mut command = Command::new("rustup");
        command.args(["target", "add"]);
        self.apply_toolchain_arg(&mut command);
        command.args(targets.iter().map(TargetTriple::as_str));
        command.current_dir(working_dir);

        let status = command
            .status()
            .wrap_err("failed to invoke rustup to install missing targets")?;

        if !status.success() {
            eyre::bail!(
                "rustup target add failed with exit code {:?}",
                status.code(),
            );
        }

        Ok(())
    }
}

/// Install target components missing from the final execution plans.
///
/// Host targets are skipped because they are already part of the active
/// toolchain. The rustup calls are intentionally made only after target
/// planning so configured target capability, `--target`, `--no-targets`, and
/// target-specific package exclusion have already been resolved.
pub(crate) fn ensure_missing_targets_installed(
    plan_set: &ExecutionPlanSet<'_>,
    env: &impl TargetEnvironment,
    installer: &impl TargetInstaller,
) -> eyre::Result<()> {
    let host = match env.host_target() {
        Ok(host) => host,
        Err(err) => {
            print_warning!(
                "could not detect host target before installing Rust targets: {err}; continuing without installing targets",
            );
            return Ok(());
        }
    };
    let contexts = install_contexts(plan_set, &host)?;

    for context in &contexts {
        install_missing_for_context(context, installer, contexts.len() > 1);
    }

    Ok(())
}

struct InstallContext {
    working_dir: PathBuf,
    targets: Vec<TargetTriple>,
}

fn install_contexts(
    plan_set: &ExecutionPlanSet<'_>,
    host: &TargetTriple,
) -> eyre::Result<Vec<InstallContext>> {
    let mut contexts: Vec<InstallContext> = Vec::new();

    for plan in &plan_set.plans {
        if &plan.target == host {
            continue;
        }
        for package_plan in &plan.package_plans {
            if !package_plan.flags.install_missing_targets {
                continue;
            }
            if package_plan.combinations.is_empty() {
                continue;
            }
            let Some(parent) = package_plan.package.manifest_path.parent() else {
                eyre::bail!(
                    "could not find parent dir of package {}",
                    package_plan.package.manifest_path,
                );
            };
            let working_dir = parent.as_std_path().to_path_buf();
            let target = package_plan.target.triple.clone();

            match contexts
                .iter_mut()
                .find(|context| context.working_dir == working_dir)
            {
                Some(context) => push_unique_target(&mut context.targets, target),
                None => contexts.push(InstallContext {
                    working_dir,
                    targets: vec![target],
                }),
            }
        }
    }

    Ok(contexts)
}

fn push_unique_target(targets: &mut Vec<TargetTriple>, target: TargetTriple) {
    if !targets.iter().any(|existing| existing == &target) {
        targets.push(target);
    }
}

fn missing_targets(candidates: &[TargetTriple], installed: &BTreeSet<String>) -> Vec<TargetTriple> {
    candidates
        .iter()
        .filter(|target| !installed.contains(target.as_str()))
        .cloned()
        .collect()
}

fn install_missing_for_context(
    context: &InstallContext,
    installer: &impl TargetInstaller,
    show_working_dir: bool,
) {
    let installed_components = match installer.installed_targets(&context.working_dir) {
        Ok(components) => components,
        Err(err) => {
            print_warning!(
                "could not list installed Rust targets with rustup in {}: {err}; continuing without installing targets",
                context.working_dir.display(),
            );
            return;
        }
    };

    let missing = missing_targets(&context.targets, &installed_components);
    if missing.is_empty() {
        return;
    }

    let target_list = target_list(&missing);
    if show_working_dir {
        print_note!(
            "attempting to install missing Rust targets in {}: {target_list}",
            context.working_dir.display()
        );
    } else {
        print_note!("attempting to install missing Rust targets: {target_list}");
    }

    if let Err(err) = installer.install_targets(&context.working_dir, &missing) {
        print_warning!(
            "failed to install Rust targets with rustup in {}: {err}; retrying individually",
            context.working_dir.display(),
        );
        install_targets_individually(context, installer, &missing);
    }
}

fn install_targets_individually(
    context: &InstallContext,
    installer: &impl TargetInstaller,
    targets: &[TargetTriple],
) {
    for target in targets {
        if let Err(err) =
            installer.install_targets(&context.working_dir, std::slice::from_ref(target))
        {
            print_warning!(
                "failed to install Rust target `{target}` with rustup in {}: {err}; continuing",
                context.working_dir.display(),
            );
        }
    }
}

fn target_list(targets: &[TargetTriple]) -> String {
    targets
        .iter()
        .map(TargetTriple::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod test {
    use super::{
        TargetInstaller, ensure_missing_targets_installed, install_contexts, missing_targets,
    };
    use crate::package::test::{effective_target, package_with_manifest_path as package};
    use crate::plan::execution::{ExecutionPlan, ExecutionPlanSet, PackageExecutionPlan};
    use crate::target::{TargetEnvironment, TargetTriple};
    use color_eyre::eyre;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn triples(targets: &[TargetTriple]) -> Vec<&str> {
        targets.iter().map(TargetTriple::as_str).collect()
    }

    fn plan_set<'a>(
        targets: &[&str],
        packages: &[&'a cargo_metadata::Package],
    ) -> ExecutionPlanSet<'a> {
        let flags = crate::config::ResolvedFlags {
            install_missing_targets: true,
            ..crate::config::ResolvedFlags::default()
        };
        ExecutionPlanSet {
            plans: targets
                .iter()
                .map(|target| ExecutionPlan {
                    target: TargetTriple((*target).to_string()),
                    package_plans: packages
                        .iter()
                        .map(|package| PackageExecutionPlan {
                            package,
                            target: effective_target(target),
                            combinations: vec![Vec::new()],
                            pruned: Vec::new(),
                            matrix: serde_json::Map::new(),
                            flags,
                            driver: None,
                            ignored_diagnostics_config: false,
                        })
                        .collect(),
                })
                .collect(),
            show_pruned: false,
            show_target: targets.len() > 1,
        }
    }

    fn empty_plan_set<'a>(
        targets: &[&str],
        packages: &[&'a cargo_metadata::Package],
    ) -> ExecutionPlanSet<'a> {
        let mut plan_set = plan_set(targets, packages);
        for plan in &mut plan_set.plans {
            for package_plan in &mut plan.package_plans {
                package_plan.combinations.clear();
            }
        }
        plan_set
    }

    struct FakeEnv {
        fail_host: bool,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self { fail_host: false }
        }

        fn failing() -> Self {
            Self { fail_host: true }
        }
    }

    impl TargetEnvironment for FakeEnv {
        fn cargo_build_target(&self) -> Option<String> {
            None
        }

        fn host_target(&self) -> eyre::Result<TargetTriple> {
            if self.fail_host {
                eyre::bail!("host failed");
            }
            Ok(TargetTriple("host".to_string()))
        }
    }

    struct FakeInstaller {
        installed: BTreeSet<String>,
        list_error: bool,
        fail_next_install: Cell<bool>,
        list_calls: RefCell<Vec<PathBuf>>,
        install_calls: RefCell<Vec<(PathBuf, Vec<String>)>>,
    }

    impl FakeInstaller {
        fn new(installed: &[&str]) -> Self {
            Self {
                installed: installed
                    .iter()
                    .map(|target| (*target).to_string())
                    .collect(),
                list_error: false,
                fail_next_install: Cell::new(false),
                list_calls: RefCell::new(Vec::new()),
                install_calls: RefCell::new(Vec::new()),
            }
        }

        fn with_list_error(mut self) -> Self {
            self.list_error = true;
            self
        }

        fn with_next_install_failure(self) -> Self {
            self.fail_next_install.set(true);
            self
        }
    }

    impl TargetInstaller for FakeInstaller {
        fn installed_targets(&self, working_dir: &Path) -> eyre::Result<BTreeSet<String>> {
            self.list_calls.borrow_mut().push(working_dir.to_path_buf());
            if self.list_error {
                eyre::bail!("list failed");
            }
            Ok(self.installed.clone())
        }

        fn install_targets(
            &self,
            working_dir: &Path,
            targets: &[TargetTriple],
        ) -> eyre::Result<()> {
            self.install_calls.borrow_mut().push((
                working_dir.to_path_buf(),
                targets
                    .iter()
                    .map(TargetTriple::as_str)
                    .map(ToOwned::to_owned)
                    .collect(),
            ));
            if self.fail_next_install.replace(false) {
                eyre::bail!("install failed");
            }
            Ok(())
        }
    }

    #[test]
    fn install_contexts_skip_host_and_group_by_package_working_dir() -> eyre::Result<()> {
        let package_a = package("a", "/workspace/a/Cargo.toml")?;
        let package_b = package("b", "/workspace/b/Cargo.toml")?;
        let plan_set = plan_set(&["host", "wasm"], &[&package_a, &package_b]);

        let contexts = install_contexts(&plan_set, &TargetTriple("host".to_string()))?;

        assert_eq!(contexts.len(), 2);
        assert_eq!(contexts[0].working_dir, PathBuf::from("/workspace/a"));
        assert_eq!(triples(&contexts[0].targets), vec!["wasm"]);
        assert_eq!(contexts[1].working_dir, PathBuf::from("/workspace/b"));
        assert_eq!(triples(&contexts[1].targets), vec!["wasm"]);
        Ok(())
    }

    #[test]
    fn missing_targets_filter_installed_without_reordering() {
        let candidates = vec![
            TargetTriple("wasm".to_string()),
            TargetTriple("windows".to_string()),
            TargetTriple("darwin".to_string()),
        ];
        let installed = BTreeSet::from(["windows".to_string()]);

        let targets = missing_targets(&candidates, &installed);

        assert_eq!(triples(&targets), vec!["wasm", "darwin"]);
    }

    #[test]
    fn ensure_missing_targets_installed_installs_only_missing_non_host_targets() -> eyre::Result<()>
    {
        let package = package("a", "/workspace/a/Cargo.toml")?;
        let plans = plan_set(&["host", "wasm", "windows"], &[&package]);
        let installer = FakeInstaller::new(&["windows"]);

        ensure_missing_targets_installed(&plans, &FakeEnv::new(), &installer)?;

        assert_eq!(
            installer.list_calls.borrow().as_slice(),
            &[PathBuf::from("/workspace/a")]
        );
        assert_eq!(
            installer.install_calls.borrow().as_slice(),
            &[(PathBuf::from("/workspace/a"), vec!["wasm".to_string()])]
        );
        Ok(())
    }

    #[test]
    fn ensure_missing_targets_installed_skips_rustup_when_only_host_is_planned() -> eyre::Result<()>
    {
        let package = package("a", "/workspace/a/Cargo.toml")?;
        let plans = plan_set(&["host"], &[&package]);
        let installer = FakeInstaller::new(&[]);

        ensure_missing_targets_installed(&plans, &FakeEnv::new(), &installer)?;

        assert!(installer.list_calls.borrow().is_empty());
        assert!(installer.install_calls.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn ensure_missing_targets_installed_continues_when_rustup_list_fails() -> eyre::Result<()> {
        let package = package("a", "/workspace/a/Cargo.toml")?;
        let plans = plan_set(&["wasm"], &[&package]);
        let installer = FakeInstaller::new(&[]).with_list_error();

        ensure_missing_targets_installed(&plans, &FakeEnv::new(), &installer)?;

        assert_eq!(
            installer.list_calls.borrow().as_slice(),
            &[PathBuf::from("/workspace/a")]
        );
        assert!(installer.install_calls.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn ensure_missing_targets_installed_continues_when_host_detection_fails() -> eyre::Result<()> {
        let package = package("a", "/workspace/a/Cargo.toml")?;
        let plans = plan_set(&["wasm"], &[&package]);
        let installer = FakeInstaller::new(&[]);

        ensure_missing_targets_installed(&plans, &FakeEnv::failing(), &installer)?;

        assert!(installer.list_calls.borrow().is_empty());
        assert!(installer.install_calls.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn ensure_missing_targets_installed_skips_zero_combination_package_plans() -> eyre::Result<()> {
        let package = package("a", "/workspace/a/Cargo.toml")?;
        let plans = empty_plan_set(&["wasm"], &[&package]);
        let installer = FakeInstaller::new(&[]);

        ensure_missing_targets_installed(&plans, &FakeEnv::new(), &installer)?;

        assert!(installer.list_calls.borrow().is_empty());
        assert!(installer.install_calls.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn ensure_missing_targets_installed_retries_batch_failure_individually() -> eyre::Result<()> {
        let package = package("a", "/workspace/a/Cargo.toml")?;
        let plans = plan_set(&["wasm", "windows"], &[&package]);
        let installer = FakeInstaller::new(&[]).with_next_install_failure();

        ensure_missing_targets_installed(&plans, &FakeEnv::new(), &installer)?;

        assert_eq!(
            installer.install_calls.borrow().as_slice(),
            &[
                (
                    PathBuf::from("/workspace/a"),
                    vec!["wasm".to_string(), "windows".to_string()]
                ),
                (PathBuf::from("/workspace/a"), vec!["wasm".to_string()]),
                (PathBuf::from("/workspace/a"), vec!["windows".to_string()])
            ]
        );
        Ok(())
    }
}
