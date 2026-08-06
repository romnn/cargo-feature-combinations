//! Execution of resolved plans: step scheduling, target aggregation, and
//! fail-fast handling.

mod invoke;
mod summary;

pub(crate) use invoke::ProcessResult;
pub(crate) use summary::print_feature_combination_error;

use invoke::{CombinationResult, run_single_combination};
use summary::{Summary, SummaryTarget, append_pruned_summaries, print_summary};

use crate::cli::{CargoSubcommand, cargo_subcommand};
use crate::config::{ResolvedEnv, ResolvedFlags};
use crate::implication::PrunedCombination;
use crate::invocation_args::{GeneratedArgPlacement, PreparedInvocationArgs};
use crate::plan::execution::ExecutionPlanSet;
use crate::target::{EffectiveTarget, TargetTriple};
use crate::toolchain::{self, ToolchainOverride};
use crate::{print_note, print_warning};

use color_eyre::eyre;
use itertools::Itertools;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Write};
use std::sync::LazyLock;
use std::time::Instant;
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream};

static CYAN: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Cyan, true));
static RED: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Red, true));
static YELLOW: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Yellow, true));
static GREEN: LazyLock<ColorSpec> = LazyLock::new(|| color_spec(Color::Green, true));
static DIMMED: LazyLock<ColorSpec> = LazyLock::new(|| {
    let mut spec = ColorSpec::new();
    spec.set_dimmed(true);
    spec
});

/// An optional process exit code.
///
/// `None` means success (exit 0), `Some(code)` means the process should exit
/// with the given code.
pub type ExitCode = Option<i32>;

/// Build a [`ColorSpec`] with the given foreground color and bold setting.
#[must_use]
fn color_spec(color: Color, bold: bool) -> ColorSpec {
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color));
    spec.set_bold(bold);
    spec
}

/// Position of a feature combination within the overall run.
#[derive(Clone, Copy)]
struct Progress {
    index: usize,
    total: usize,
    width: usize,
}

/// The per-combination inputs for one Cargo invocation.
struct Invocation<'a> {
    package: &'a cargo_metadata::Package,
    features: &'a [String],
    /// Fully resolved cargo-fc flags for this package-target invocation.
    flags: ResolvedFlags,
    /// Target triples cargo-fc must inject as `--target` (configured sources).
    inject_targets: &'a [String],
    /// Display/attribution context for the header and summary entry.
    summary_target: &'a SummaryTarget,
    /// Finalized build driver to spawn instead of `$CARGO`/`cargo` for this
    /// invocation (e.g. `cargo-zigbuild`); `None` means plain `$CARGO`.
    driver: Option<&'a str>,
    env: &'a ResolvedEnv,
}

struct InvocationStep<'a> {
    package: &'a cargo_metadata::Package,
    features: Vec<String>,
    flags: ResolvedFlags,
    inject_targets: Vec<String>,
    summary_target: SummaryTarget,
    driver: Option<String>,
    env: ResolvedEnv,
}

enum Step<'a> {
    StartSerialBlock,
    Run(InvocationStep<'a>),
    AppendPruned {
        package_name: String,
        summary_target: SummaryTarget,
        pruned: Vec<PrunedCombination>,
    },
}

/// One aggregate-mode Cargo invocation after transposing target plans by package
/// and feature combination.
struct AggregateInvocationPlan<'a> {
    package: &'a cargo_metadata::Package,
    combo: Vec<String>,
    flags: ResolvedFlags,
    targets: Vec<EffectiveTarget>,
    /// Build driver shared by every target in this aggregated invocation.
    driver: Option<String>,
    /// Environment patch shared by every target in this aggregated invocation.
    env: ResolvedEnv,
}

/// Pre-computed state shared across all feature combinations in one execution.
struct RunContext<'a> {
    invocation_args: &'a PreparedInvocationArgs<'a>,
    /// `+toolchain` consumed from the forwarded args, if cargo-fc resolved one.
    toolchain: Option<&'a ToolchainOverride>,
}

/// Execution mode over the same execution plans.
///
/// Both modes are single-threaded and stream live output; they differ only in
/// how targets map onto Cargo invocations and how summary entries are keyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetExecutionMode {
    /// Default: one invocation per `(package, target, combo)`, exact per-target
    /// attribution.
    SerialPerTarget,
    /// `--aggregate-targets`: one invocation per `(package, combo)` carrying all
    /// that combo's targets as repeated `--target` flags, group-level
    /// attribution.
    Aggregate,
}

/// Resolve the effective target execution mode, emitting a note when an
/// explicitly requested `--aggregate-targets` falls back to serial or is a
/// no-op.
pub(crate) fn resolve_execution_mode(
    cargo_args: &[&str],
    plan_set: &ExecutionPlanSet<'_>,
    generated_arg_placement: GeneratedArgPlacement,
) -> TargetExecutionMode {
    let mut requested = 0usize;
    let mut total = 0usize;
    for plan in &plan_set.plans {
        for package_plan in &plan.package_plans {
            total += 1;
            requested += usize::from(package_plan.flags.aggregate_targets);
        }
    }

    if requested == 0 {
        return TargetExecutionMode::SerialPerTarget;
    }

    if requested != total {
        print_note!(
            "aggregate target execution is disabled because it resolves differently across package-targets; running targets serially"
        );
        return TargetExecutionMode::SerialPerTarget;
    }

    if plan_set.plans.len() <= 1 {
        if !plan_set.show_target {
            return TargetExecutionMode::SerialPerTarget;
        }
        print_note!("--aggregate-targets has no effect for a single target; running normally");
        return TargetExecutionMode::SerialPerTarget;
    }

    if generated_arg_placement == GeneratedArgPlacement::CargoCommand
        && cargo_subcommand(cargo_args) == CargoSubcommand::Run
    {
        print_note!(
            "--aggregate-targets does not apply to `run` (cargo runs one target at a time); running targets serially"
        );
        return TargetExecutionMode::SerialPerTarget;
    }

    if plan_set.show_pruned {
        print_note!(
            "--aggregate-targets is disabled because pruned summaries are target-specific; running targets serially"
        );
        return TargetExecutionMode::SerialPerTarget;
    }

    TargetExecutionMode::Aggregate
}

/// Set up shared Cargo invocation context and run the execution plans in the
/// given mode.
///
/// # Errors
///
/// Returns an error if a cargo process can not be spawned or if IO operations
/// fail while reading cargo's output.
pub fn run_execution_plans(
    plan_set: &ExecutionPlanSet,
    mut cargo_args: Vec<&str>,
    mode: TargetExecutionMode,
    generated_arg_placement: GeneratedArgPlacement,
) -> eyre::Result<ExitCode> {
    let start = Instant::now();

    // Taken before the args are split, because a `+toolchain` reaching the child
    // is a hard error there: `$CARGO` names a toolchain-pinned binary, and only
    // rustup's proxy ever understands the token.
    let toolchain = toolchain::take_override(&mut cargo_args, &toolchain::RustupToolchainResolver)?;
    let invocation_args = PreparedInvocationArgs::new(cargo_args, generated_arg_placement);

    let removed_feature_args = invocation_args.removed_feature_args();
    if !removed_feature_args.is_empty() {
        let flag_label = if removed_feature_args.len() == 1 {
            "flag"
        } else {
            "flags"
        };
        print_warning!(
            "ignoring cargo feature-selection {flag_label} incompatible with feature matrix: {}",
            removed_feature_args.iter().join(" ")
        );
    } else if invocation_args.preserved_feature_selection_for_unknown_command() {
        print_warning!(
            "leaving cargo feature-selection flags unchanged for unresolved cargo alias/custom subcommand"
        );
    }

    let wants_diagnostics = plan_set.plans.iter().any(|plan| {
        plan.package_plans
            .iter()
            .any(|package_plan| package_plan.flags.diagnostics_only)
    });
    if wants_diagnostics && invocation_args.has_message_format_arg_for_generated_args() {
        print_warning!("--diagnostics-only is ignored when --message-format is already specified");
    }

    let ctx = RunContext {
        invocation_args: &invocation_args,
        toolchain: toolchain.as_ref(),
    };

    let mut stdout = StandardStream::stdout(ColorChoice::Auto);
    let mut stderr = StandardStream::stderr(ColorChoice::Auto);
    let mut seen_diagnostics: HashSet<String> = HashSet::new();

    let steps = execution_steps(plan_set, mode);
    execute_steps(
        plan_set,
        &steps,
        &ctx,
        &mut seen_diagnostics,
        &mut stdout,
        &mut stderr,
        start,
    )
}

fn execution_steps<'a>(
    plan_set: &'a ExecutionPlanSet<'a>,
    mode: TargetExecutionMode,
) -> Vec<Step<'a>> {
    match mode {
        TargetExecutionMode::SerialPerTarget => serial_steps(plan_set),
        TargetExecutionMode::Aggregate => aggregate_steps(plan_set),
    }
}

fn serial_steps<'a>(plan_set: &'a ExecutionPlanSet<'a>) -> Vec<Step<'a>> {
    let mut steps = Vec::new();
    for plan in &plan_set.plans {
        for pp in &plan.package_plans {
            let summary_target = if plan_set.show_target {
                SummaryTarget::Single(plan.target.clone())
            } else {
                SummaryTarget::Hidden
            };
            let inject_targets = if pp.target.source.should_inject_target_arg() {
                vec![pp.target.triple.0.clone()]
            } else {
                Vec::new()
            };

            steps.push(Step::StartSerialBlock);
            for combo in &pp.combinations {
                steps.push(Step::Run(InvocationStep {
                    package: pp.package,
                    features: combo.clone(),
                    flags: pp.flags,
                    inject_targets: inject_targets.clone(),
                    summary_target: summary_target.clone(),
                    driver: pp.driver.clone(),
                    env: pp.env.clone(),
                }));
            }
            steps.push(Step::AppendPruned {
                package_name: pp.package.name.to_string(),
                summary_target,
                pruned: pp.pruned.clone(),
            });
        }
    }
    steps
}

fn aggregate_steps<'a>(plan_set: &'a ExecutionPlanSet<'a>) -> Vec<Step<'a>> {
    aggregate_invocation_plans(plan_set)
        .into_iter()
        .map(|inv_plan| {
            let triples: Vec<TargetTriple> =
                inv_plan.targets.iter().map(|t| t.triple.clone()).collect();
            let summary_target = match triples.as_slice() {
                [single] => SummaryTarget::Single(single.clone()),
                _ => SummaryTarget::Group(triples),
            };
            let inject_targets = inv_plan
                .targets
                .iter()
                .filter(|t| t.source.should_inject_target_arg())
                .map(|t| t.triple.0.clone())
                .collect();

            Step::Run(InvocationStep {
                package: inv_plan.package,
                features: inv_plan.combo,
                flags: inv_plan.flags,
                inject_targets,
                summary_target,
                driver: inv_plan.driver,
                env: inv_plan.env,
            })
        })
        .collect()
}

fn execute_steps(
    plan_set: &ExecutionPlanSet,
    steps: &[Step<'_>],
    ctx: &RunContext<'_>,
    seen_diagnostics: &mut HashSet<String>,
    stdout: &mut StandardStream,
    stderr: &mut StandardStream,
    start: Instant,
) -> eyre::Result<ExitCode> {
    let mut summary: Vec<Summary> = Vec::new();
    let total = steps
        .iter()
        .filter(|step| matches!(step, Step::Run(_)))
        .count();
    let width = total.to_string().len();
    let mut index = 0;
    let mut block_start = 0usize;

    for step in steps {
        match step {
            Step::StartSerialBlock => {
                block_start = summary.len();
            }
            Step::Run(inv_step) => {
                index += 1;
                let result = run_single_combination(
                    &Invocation {
                        package: inv_step.package,
                        features: &inv_step.features,
                        flags: inv_step.flags,
                        inject_targets: &inv_step.inject_targets,
                        summary_target: &inv_step.summary_target,
                        driver: inv_step.driver.as_deref(),
                        env: &inv_step.env,
                    },
                    ctx,
                    Progress {
                        index,
                        total,
                        width,
                    },
                    seen_diagnostics,
                    stderr,
                )?;
                if let Some(code) = record_result_and_maybe_stop(
                    &mut summary,
                    result,
                    plan_set.show_pruned,
                    ctx,
                    stdout,
                    stderr,
                    start,
                )? {
                    return Ok(Some(code));
                }
            }
            Step::AppendPruned {
                package_name,
                summary_target,
                pruned,
            } => {
                append_pruned_summaries(
                    &mut summary,
                    block_start,
                    package_name,
                    summary_target,
                    pruned.clone(),
                );
            }
        }
    }

    Ok(print_summary(
        &summary,
        plan_set.show_pruned,
        stdout,
        start.elapsed(),
    ))
}

fn record_result_and_maybe_stop(
    summary: &mut Vec<Summary>,
    result: CombinationResult,
    show_pruned: bool,
    _ctx: &RunContext<'_>,
    stdout: &mut StandardStream,
    stderr: &mut StandardStream,
    start: Instant,
) -> eyre::Result<ExitCode> {
    let CombinationResult {
        summary: result_summary,
        colored_output,
        flags,
    } = result;
    let should_stop = flags.fail_fast && !result_summary.pedantic_success;
    let exit_code = result_summary.exit_code;
    summary.push(result_summary);

    if !should_stop {
        return Ok(None);
    }

    if flags.summary_only {
        io::copy(&mut io::Cursor::new(colored_output), stderr)?;
        stderr.flush().ok();
    }
    Ok(Some(
        print_summary(summary, show_pruned, stdout, start.elapsed())
            .or(exit_code)
            .unwrap_or(1),
    ))
}

/// Transpose per-target execution plans into aggregate-mode invocations.
///
/// The resulting order is package first-appearance order, sorted canonical combo
/// order, then target-plan order within each combo's target group.
fn aggregate_invocation_plans<'a>(
    plan_set: &'a ExecutionPlanSet<'a>,
) -> Vec<AggregateInvocationPlan<'a>> {
    let mut package_order: Vec<&cargo_metadata::Package> = Vec::new();
    let mut seen_packages: HashSet<String> = HashSet::new();
    let mut grouped: HashMap<String, BTreeMap<AggregateKey, Vec<EffectiveTarget>>> = HashMap::new();
    for plan in &plan_set.plans {
        for pp in &plan.package_plans {
            let id = pp.package.id.repr.clone();
            if seen_packages.insert(id.clone()) {
                package_order.push(pp.package);
            }
            let entry = grouped.entry(id).or_default();
            for combo in &pp.combinations {
                entry
                    .entry(AggregateKey {
                        combo: combo.clone(),
                        flags: pp.flags,
                        driver: pp.driver.clone(),
                        env: pp.env.clone(),
                    })
                    .or_default()
                    .push(pp.target.clone());
            }
        }
    }

    let mut invocations = Vec::new();
    for package in package_order {
        let Some(combos) = grouped.remove(&package.id.repr) else {
            continue;
        };
        for (key, targets) in combos {
            invocations.push(AggregateInvocationPlan {
                package,
                combo: key.combo,
                flags: key.flags,
                targets,
                driver: key.driver,
                env: key.env,
            });
        }
    }

    invocations
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AggregateKey {
    combo: Vec<String>,
    flags: ResolvedFlags,
    driver: Option<String>,
    env: ResolvedEnv,
}

#[cfg(test)]
mod test {
    use super::{ResolvedEnv, aggregate_invocation_plans};

    fn resolved_env(value: serde_json::Value) -> eyre::Result<ResolvedEnv> {
        let patch: crate::config::EnvPatch = serde_json::from_value(value)?;
        let operations = crate::config::env::combine_env_patches("test", [("", &patch)])?
            .ok_or_else(|| eyre::eyre!("test env patch unexpectedly absent"))?;
        let mut resolved = ResolvedEnv::default();
        resolved.apply_patch(&operations);
        Ok(resolved)
    }
    use crate::config::ResolvedFlags;
    use crate::package::test::{effective_target, package};
    use crate::plan::execution::{ExecutionPlan, ExecutionPlanSet, PackageExecutionPlan};
    use crate::target::TargetTriple;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn string_vec(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(String::from).collect()
    }

    fn package_plan<'a>(
        package: &'a cargo_metadata::Package,
        target: &str,
        combinations: Vec<Vec<String>>,
        flags: ResolvedFlags,
    ) -> PackageExecutionPlan<'a> {
        PackageExecutionPlan {
            package,
            target: effective_target(target),
            combinations,
            pruned: Vec::new(),
            matrix: serde_json::Map::new(),
            flags,
            driver: None,
            env: ResolvedEnv::default(),
            ignored_diagnostics_config: false,
        }
    }

    #[test]
    fn aggregate_invocation_plans_group_by_package_combo_and_target_order() -> eyre::Result<()> {
        let package_a = package("a")?;
        let package_b = package("b")?;
        let plan_set = ExecutionPlanSet {
            plans: vec![
                ExecutionPlan {
                    target: TargetTriple("t1".to_string()),
                    package_plans: vec![
                        package_plan(
                            &package_a,
                            "t1",
                            vec![string_vec(&["b"]), string_vec(&[])],
                            ResolvedFlags::default(),
                        ),
                        package_plan(
                            &package_b,
                            "t1",
                            vec![string_vec(&["z"])],
                            ResolvedFlags::default(),
                        ),
                    ],
                },
                ExecutionPlan {
                    target: TargetTriple("t2".to_string()),
                    package_plans: vec![
                        package_plan(
                            &package_a,
                            "t2",
                            vec![string_vec(&[]), string_vec(&["a"])],
                            ResolvedFlags::default(),
                        ),
                        package_plan(
                            &package_b,
                            "t2",
                            vec![string_vec(&["z"])],
                            ResolvedFlags::default(),
                        ),
                    ],
                },
            ],
            show_pruned: false,
            show_target: true,
        };

        let simplified: Vec<_> = aggregate_invocation_plans(&plan_set)
            .into_iter()
            .map(|inv| {
                (
                    inv.package.name.to_string(),
                    inv.combo,
                    inv.targets
                        .into_iter()
                        .map(|target| target.triple.0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();

        sim_assert_eq!(
            simplified,
            vec![
                (
                    "a".to_string(),
                    string_vec(&[]),
                    vec!["t1".to_string(), "t2".to_string()]
                ),
                ("a".to_string(), string_vec(&["a"]), vec!["t2".to_string()]),
                ("a".to_string(), string_vec(&["b"]), vec!["t1".to_string()]),
                (
                    "b".to_string(),
                    string_vec(&["z"]),
                    vec!["t1".to_string(), "t2".to_string()]
                ),
            ],
        );
        Ok(())
    }

    #[test]
    fn aggregate_invocation_plans_split_by_resolved_flags() -> eyre::Result<()> {
        let package = package("a")?;
        let dedupe_flags = ResolvedFlags {
            diagnostics_only: true,
            dedupe: true,
            ..ResolvedFlags::default()
        };
        let plan_set = ExecutionPlanSet {
            plans: vec![
                ExecutionPlan {
                    target: TargetTriple("t1".to_string()),
                    package_plans: vec![package_plan(
                        &package,
                        "t1",
                        vec![string_vec(&[])],
                        ResolvedFlags::default(),
                    )],
                },
                ExecutionPlan {
                    target: TargetTriple("t2".to_string()),
                    package_plans: vec![package_plan(
                        &package,
                        "t2",
                        vec![string_vec(&[])],
                        dedupe_flags,
                    )],
                },
            ],
            show_pruned: false,
            show_target: true,
        };

        let simplified: Vec<_> = aggregate_invocation_plans(&plan_set)
            .into_iter()
            .map(|inv| {
                (
                    inv.combo,
                    inv.flags,
                    inv.targets
                        .into_iter()
                        .map(|target| target.triple.0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();

        sim_assert_eq!(
            simplified,
            vec![
                (
                    string_vec(&[]),
                    ResolvedFlags::default(),
                    vec!["t1".to_string()]
                ),
                (string_vec(&[]), dedupe_flags, vec!["t2".to_string()]),
            ],
        );
        Ok(())
    }

    #[test]
    fn aggregate_invocation_plans_split_by_resolved_driver() -> eyre::Result<()> {
        let package = package("a")?;
        let make_plan = |target: &str, driver: Option<&str>| {
            let mut package_plan = package_plan(
                &package,
                target,
                vec![string_vec(&[])],
                ResolvedFlags::default(),
            );
            package_plan.driver = driver.map(ToString::to_string);
            ExecutionPlan {
                target: TargetTriple(target.to_string()),
                package_plans: vec![package_plan],
            }
        };
        let plan_set = ExecutionPlanSet {
            plans: vec![
                make_plan("t1", None),
                make_plan("t2", Some("cargo-zigbuild")),
            ],
            show_pruned: false,
            show_target: true,
        };

        let simplified = aggregate_invocation_plans(&plan_set)
            .into_iter()
            .map(|invocation| {
                (
                    invocation.driver,
                    invocation
                        .targets
                        .into_iter()
                        .map(|target| target.triple.0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();

        sim_assert_eq!(
            simplified,
            vec![
                (None, vec!["t1".to_string()]),
                (Some("cargo-zigbuild".to_string()), vec!["t2".to_string()]),
            ]
        );
        Ok(())
    }

    #[test]
    fn aggregate_invocation_plans_split_by_resolved_env() -> eyre::Result<()> {
        let package = package("a")?;
        let configured_env = resolved_env(serde_json::json!({
            "add": { "ORT_STRATEGY": "download" },
        }))?;
        let make_plan = |target: &str, env: ResolvedEnv| {
            let mut package_plan = package_plan(
                &package,
                target,
                vec![string_vec(&[])],
                ResolvedFlags::default(),
            );
            package_plan.env = env;
            ExecutionPlan {
                target: TargetTriple(target.to_string()),
                package_plans: vec![package_plan],
            }
        };
        let plan_set = ExecutionPlanSet {
            plans: vec![
                make_plan("t1", ResolvedEnv::default()),
                make_plan("t2", configured_env.clone()),
                make_plan("t3", configured_env.clone()),
            ],
            show_pruned: false,
            show_target: true,
        };

        let simplified = aggregate_invocation_plans(&plan_set)
            .into_iter()
            .map(|invocation| {
                (
                    invocation.env,
                    invocation
                        .targets
                        .into_iter()
                        .map(|target| target.triple.0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();

        sim_assert_eq!(
            simplified,
            vec![
                (ResolvedEnv::default(), vec!["t1".to_string()]),
                (configured_env, vec!["t2".to_string(), "t3".to_string()]),
            ]
        );
        Ok(())
    }
}
