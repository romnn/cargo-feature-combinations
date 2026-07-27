//! Per-combination result summaries and end-of-run reporting.

use crate::DEFAULT_PKG_METADATA_SECTION;
use crate::implication::PrunedCombination;
use crate::package::FeatureCombinationError;
use crate::target::TargetTriple;

use itertools::Itertools;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::LazyLock;
use std::time::Duration;
use termcolor::{ColorChoice, StandardStream, WriteColor};

use super::{CYAN, DIMMED, ExitCode, GREEN, RED, YELLOW};

/// Target display context for a summary entry and command header.
///
/// `Hidden` preserves implicit single-host output, `Single` prints
/// `target = ...` (exact per-target attribution), and `Group` prints
/// `targets = [...]` for an aggregate multi-target invocation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum SummaryTarget {
    /// Implicit single-host run: no target field is shown.
    Hidden,
    /// A single concrete target with exact attribution.
    Single(TargetTriple),
    /// An aggregate group of targets sharing one Cargo invocation.
    Group(Vec<TargetTriple>),
}

impl SummaryTarget {
    /// The `target = ...,` / `targets = [...],` field prefix shown inside the
    /// `( ... )` of headers and summary entries, including the trailing
    /// `", "`. Empty for [`SummaryTarget::Hidden`].
    pub(super) fn field_prefix(&self) -> String {
        match self {
            Self::Hidden => String::new(),
            Self::Single(triple) => format!("target = {triple}, "),
            Self::Group(triples) => format!("targets = [{}], ", triples.iter().join(", ")),
        }
    }
}

/// Summary of the outcome for running (or pruning) a single feature set.
#[derive(Debug, Clone)]
pub(super) struct Summary {
    pub(super) package_name: String,
    pub(super) target: SummaryTarget,
    pub(super) features: Vec<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) pedantic_success: bool,
    pub(super) num_warnings: usize,
    pub(super) num_errors: usize,
    pub(super) num_suppressed: usize,
    /// If this combination was pruned, the features of the equivalent combo.
    pub(super) equivalent_to: Option<Vec<String>>,
}

impl Summary {
    fn is_pruned(&self) -> bool {
        self.equivalent_to.is_some()
    }
}

/// Extract per-crate warning counts from cargo output.
///
/// The iterator yields the number of warnings for each compiled crate that
/// matches the summary line produced by cargo.
pub(super) fn warning_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    static WARNING_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(
            clippy::expect_used,
            reason = "hard-coded regex pattern is expected to be valid"
        )]
        Regex::new(r"warning: .* generated (\d+) warnings?").expect("valid warning regex")
    });
    WARNING_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(0))
}

/// Extract per-crate error counts from cargo output.
///
/// The iterator yields the number of errors for each compiled crate that
/// matches the summary line produced by cargo.
pub(super) fn error_counts(output: &str) -> impl Iterator<Item = usize> + '_ {
    static ERROR_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(
            clippy::expect_used,
            reason = "hard-coded regex pattern is expected to be valid"
        )]
        Regex::new(r"error: could not compile `[^`]*`.*due to\s*(\d*)\s*previous errors?")
            .expect("valid error regex")
    });
    ERROR_REGEX
        .captures_iter(output)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().parse::<usize>().unwrap_or(1))
}

pub(crate) fn print_feature_combination_error(err: &FeatureCombinationError) {
    let mut stderr = StandardStream::stderr(ColorChoice::Auto);

    let _ = stderr.set_color(&RED);
    let _ = write!(&mut stderr, "error");
    let _ = stderr.reset();
    let _ = writeln!(&mut stderr, ": feature matrix generation failed");

    match err {
        FeatureCombinationError::TooManyConfigurations {
            package,
            num_features,
            num_configurations,
            limit,
        } => {
            let _ = stderr.set_color(&YELLOW);
            let _ = writeln!(&mut stderr, "  reason: too many configurations");
            let _ = stderr.reset();

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  package:");
            let _ = stderr.reset();
            let _ = writeln!(&mut stderr, " {package}");

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  features considered:");
            let _ = stderr.reset();
            let _ = writeln!(&mut stderr, " {num_features}");

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  combinations:");
            let _ = stderr.reset();
            let _ = writeln!(
                &mut stderr,
                " {}",
                num_configurations.map_or_else(|| "unbounded".to_string(), |v| v.to_string())
            );

            let _ = stderr.set_color(&CYAN);
            let _ = write!(&mut stderr, "  limit:");
            let _ = stderr.reset();
            let _ = writeln!(&mut stderr, " {limit}");

            let _ = stderr.set_color(&GREEN);
            let _ = writeln!(&mut stderr, "  hint:");
            let _ = stderr.reset();
            let _ = writeln!(
                &mut stderr,
                "    Consider restricting the matrix using [{DEFAULT_PKG_METADATA_SECTION}].only_features",
            );
            let _ = writeln!(
                &mut stderr,
                "    or splitting features into isolated_feature_sets, or excluding features via exclude_features."
            );
        }
    }
}

/// Print an aggregated summary for all executed feature combinations.
///
/// Returns the [`ExitCode`] of the first failing feature combination, or
/// `None` if all combinations succeeded.
///
#[must_use]
pub(super) fn print_summary(
    summary: &[Summary],
    show_pruned: bool,
    stdout: &mut termcolor::StandardStream,
    elapsed: Duration,
) -> ExitCode {
    let num_packages = summary
        .iter()
        .map(|s| &s.package_name)
        .collect::<HashSet<_>>()
        .len();
    // Key executed/pruned combinations by (package, target, features) so that
    // identical feature sets across targets do not collapse.
    let num_total = summary
        .iter()
        .map(|s| {
            (
                &s.package_name,
                &s.target,
                s.features.iter().collect::<Vec<_>>(),
            )
        })
        .collect::<HashSet<_>>()
        .len();
    let num_pruned = summary.iter().filter(|s| s.is_pruned()).count();
    let num_executed = num_total - num_pruned;

    let mut target_set: HashSet<&TargetTriple> = HashSet::new();
    for s in summary {
        match &s.target {
            SummaryTarget::Hidden => {}
            SummaryTarget::Single(triple) => {
                target_set.insert(triple);
            }
            SummaryTarget::Group(triples) => {
                target_set.extend(triples.iter());
            }
        }
    }
    let num_targets = target_set.len();
    let targets_clause = if num_targets > 1 {
        format!(" across {num_targets} targets")
    } else {
        String::new()
    };

    let _ = writeln!(stdout);
    stdout.set_color(&CYAN).ok();
    let _ = write!(stdout, "    Finished ");
    stdout.reset().ok();
    if num_pruned > 0 {
        let _ = write!(
            stdout,
            "{num_executed} of {num_total} feature combination{} for {num_packages} package{}{targets_clause} in {:.2}s",
            if num_total == 1 { "" } else { "s" },
            if num_packages == 1 { "" } else { "s" },
            elapsed.as_secs_f64(),
        );
        stdout.set_color(&DIMMED).ok();
        let _ = write!(stdout, " ({num_pruned} pruned)");
        stdout.reset().ok();
    } else {
        let _ = write!(
            stdout,
            "{num_total} feature combination{} for {num_packages} package{}{targets_clause} in {:.2}s",
            if num_total == 1 { "" } else { "s" },
            if num_packages == 1 { "" } else { "s" },
            elapsed.as_secs_f64(),
        );
    }
    let _ = writeln!(stdout);
    let _ = writeln!(stdout);

    let max_errors = summary.iter().map(|s| s.num_errors).max().unwrap_or(0);
    let max_warnings = summary.iter().map(|s| s.num_warnings).max().unwrap_or(0);
    let max_suppressed = summary.iter().map(|s| s.num_suppressed).max().unwrap_or(0);
    let show_suppressed = max_suppressed > 0;
    let errors_width = max_errors.to_string().len();
    let warnings_width = max_warnings.to_string().len();
    let suppressed_width = max_suppressed.to_string().len();

    let mut first_bad_exit_code: Option<i32> = None;

    for s in summary {
        if !show_pruned && s.is_pruned() {
            continue;
        }
        let fmt = SummaryFormat {
            show_suppressed,
            errors_width,
            warnings_width,
            suppressed_width,
        };
        print_summary_entry(s, stdout, &fmt);
        if !s.pedantic_success {
            let exit_code = match s.exit_code {
                Some(code) if code != 0 => code,
                _ => 1,
            };
            first_bad_exit_code = first_bad_exit_code.or(Some(exit_code));
        }
    }
    let _ = writeln!(stdout);

    first_bad_exit_code
}

/// Column widths and display flags for summary entry formatting.
pub(super) struct SummaryFormat {
    show_suppressed: bool,
    errors_width: usize,
    warnings_width: usize,
    suppressed_width: usize,
}

fn print_summary_entry(s: &Summary, stdout: &mut termcolor::StandardStream, fmt: &SummaryFormat) {
    if s.is_pruned() {
        stdout.set_color(&DIMMED).ok();
        let _ = write!(stdout, "        SKIP ");
        stdout.reset().ok();
    } else if !s.pedantic_success {
        stdout.set_color(&RED).ok();
        let _ = write!(stdout, "        FAIL ");
    } else if s.num_warnings > 0 {
        stdout.set_color(&YELLOW).ok();
        let _ = write!(stdout, "        WARN ");
    } else {
        stdout.set_color(&GREEN).ok();
        let _ = write!(stdout, "        PASS ");
    }
    stdout.reset().ok();

    let feat = s.features.iter().join(", ");
    let target = s.target.field_prefix();
    let ew = fmt.errors_width;
    let ww = fmt.warnings_width;
    let sw = fmt.suppressed_width;
    let ne = s.num_errors;
    let nw = s.num_warnings;
    let ns = s.num_suppressed;
    if fmt.show_suppressed {
        let _ = write!(
            stdout,
            "{} ( {target}{ne:>ew$} errors, {nw:>ww$} warnings, {ns:>sw$} suppressed, features = [{feat}] )",
            s.package_name,
        );
    } else {
        let _ = write!(
            stdout,
            "{} ( {target}{ne:>ew$} errors, {nw:>ww$} warnings, features = [{feat}] )",
            s.package_name,
        );
    }

    if let Some(equiv) = &s.equivalent_to {
        let equiv = equiv.iter().join(", ");
        stdout.set_color(&DIMMED).ok();
        let _ = writeln!(stdout, " \u{2190} equivalent to [{equiv}]");
        stdout.reset().ok();
    } else {
        let _ = writeln!(stdout);
    }
}

/// Append pruned summaries for a single `(package, target)` block, looking up
/// the equivalent combo's error/warning counts from already-executed summaries
/// scoped to that block, then sort the block by features for interleaved
/// display.
pub(super) fn append_pruned_summaries(
    summary: &mut Vec<Summary>,
    pkg_start: usize,
    package_name: &str,
    summary_target: &SummaryTarget,
    pruned: Vec<PrunedCombination>,
) {
    let executed: HashMap<Vec<String>, Summary> = summary
        .get(pkg_start..)
        .unwrap_or_default()
        .iter()
        .filter(|s| !s.is_pruned())
        .map(|s| (s.features.clone(), s.clone()))
        .collect();

    for p in pruned {
        let Some(equiv) = executed.get(&p.equivalent_to) else {
            continue;
        };
        summary.push(Summary {
            package_name: package_name.to_string(),
            target: summary_target.clone(),
            features: p.features,
            equivalent_to: Some(p.equivalent_to),
            num_errors: equiv.num_errors,
            num_warnings: equiv.num_warnings,
            num_suppressed: equiv.num_suppressed,
            exit_code: None,
            pedantic_success: true,
        });
    }

    if let Some(slice) = summary.get_mut(pkg_start..) {
        slice.sort_by(|a, b| a.features.cmp(&b.features));
    }
}

#[cfg(test)]
mod test {
    use super::{Summary, SummaryTarget, error_counts, print_summary, warning_counts};
    use crate::target::TargetTriple;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn summary_with_failure(exit_code: Option<i32>, pedantic_success: bool) -> Summary {
        Summary {
            package_name: "pkg".to_string(),
            target: SummaryTarget::Hidden,
            features: Vec::new(),
            exit_code,
            pedantic_success,
            num_warnings: usize::from(!pedantic_success),
            num_errors: 0,
            num_suppressed: 0,
            equivalent_to: None,
        }
    }

    #[test]
    fn summary_target_field_prefix() {
        sim_assert_eq!(SummaryTarget::Hidden.field_prefix(), "");
        sim_assert_eq!(
            SummaryTarget::Single(TargetTriple("t-a".to_string())).field_prefix(),
            "target = t-a, "
        );
        sim_assert_eq!(
            SummaryTarget::Group(vec![
                TargetTriple("t-a".to_string()),
                TargetTriple("t-b".to_string()),
            ])
            .field_prefix(),
            "targets = [t-a, t-b], "
        );
    }

    #[test]
    fn print_summary_returns_one_for_pedantic_warning_exit_zero() {
        let summary = vec![summary_with_failure(Some(0), false)];
        let mut stdout = termcolor::StandardStream::stdout(termcolor::ColorChoice::Never);

        let exit = print_summary(&summary, false, &mut stdout, std::time::Duration::ZERO);

        sim_assert_eq!(exit, Some(1));
    }

    #[test]
    fn print_summary_returns_one_for_failure_without_exit_code() {
        let summary = vec![summary_with_failure(None, false)];
        let mut stdout = termcolor::StandardStream::stdout(termcolor::ColorChoice::Never);

        let exit = print_summary(&summary, false, &mut stdout, std::time::Duration::ZERO);

        sim_assert_eq!(exit, Some(1));
    }

    #[test]
    fn print_summary_preserves_nonzero_failure_exit_code() {
        let summary = vec![summary_with_failure(Some(101), false)];
        let mut stdout = termcolor::StandardStream::stdout(termcolor::ColorChoice::Never);

        let exit = print_summary(&summary, false, &mut stdout, std::time::Duration::ZERO);

        sim_assert_eq!(exit, Some(101));
    }

    #[test]
    fn error_regex_single_mod_multiple_errors() {
        let stderr = include_str!("../../test-data/single_mod_multiple_errors_stderr.txt");
        let errors: Vec<_> = error_counts(stderr).collect();
        sim_assert_eq!(&errors, &vec![2]);
    }

    #[test]
    fn error_regex_with_target_kind() {
        let stderr =
            "error: could not compile `docparser-paddleocr-vl` (lib) due to 24 previous errors";
        let errors: Vec<_> = error_counts(stderr).collect();
        sim_assert_eq!(&errors, &vec![24]);
    }

    #[test]
    fn error_regex_with_target_kind_bin() {
        let stderr =
            "error: could not compile `my-crate` (bin \"my-crate\") due to 3 previous errors";
        let errors: Vec<_> = error_counts(stderr).collect();
        sim_assert_eq!(&errors, &vec![3]);
    }

    #[test]
    fn warning_regex_two_mod_multiple_warnings() {
        let stderr = include_str!("../../test-data/two_mods_warnings_stderr.txt");
        let warnings: Vec<_> = warning_counts(stderr).collect();
        sim_assert_eq!(&warnings, &vec![6, 7]);
    }
}
