//! Forwarded Cargo argument splitting and generated-argument placement.
//!
//! cargo-fc generates Cargo args such as `--target`, `--no-default-features`,
//! and `--features=...` for each planned invocation. Most commands need those
//! before Cargo's `--`: `run --package app -- arg` becomes
//! `run --package app --features=x -- arg`. When the alias body itself supplies
//! `--`, cargo-fc preserves that alias boundary. For example,
//! `lint = "run --package wrapper -- lint"` becomes
//! `run --package wrapper -- lint --features=x`, so the wrapped command
//! receives the generated args.

use crate::cli::{CargoSubcommand, cargo_subcommand};

struct FeatureSelectionNormalization<'a> {
    cargo: Vec<&'a str>,
    removed: Vec<&'a str>,
    saw_feature_selection: bool,
}

pub(crate) enum PreparedInvocationArgs<'a> {
    /// A normal Cargo command; generated args are inserted before program args.
    CargoCommand {
        cargo_args: Vec<&'a str>,
        extra_args: Vec<&'a str>,
        removed_feature_args: Vec<&'a str>,
        has_unmanaged_feature_selection_args: bool,
    },
    /// An expanded `cargo run` alias whose alias body supplied `--`; generated
    /// args are appended after that alias boundary.
    AliasWrapper {
        cargo_args: Vec<&'a str>,
        after_separator_args: Vec<&'a str>,
    },
}

/// Where cargo-fc generated arguments belong relative to Cargo's `--` split.
///
/// The default path inserts generated args before `--`, where Cargo reads
/// `--target`, `--features`, and `--message-format`. An expanded `cargo run`
/// alias whose body supplied `--` keeps generated args after that separator;
/// this is what lets wrapper aliases receive the target/package/features
/// selected by cargo-fc, e.g. `lint = "run --package wrapper -- lint"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedArgPlacement {
    /// Generated args belong to the Cargo command itself, before any `--`.
    CargoCommand,
    /// Generated args belong after an alias-provided `--` separator.
    AliasWrapper,
}

impl<'a> PreparedInvocationArgs<'a> {
    pub(crate) fn new(mut args: Vec<&'a str>, placement: GeneratedArgPlacement) -> Self {
        let extra_args_idx = args
            .iter()
            .position(|arg| *arg == "--")
            .unwrap_or(args.len());
        let extra_args = args.split_off(extra_args_idx);

        match placement {
            GeneratedArgPlacement::AliasWrapper => {
                // Cargo-side args before the alias-provided `--` configure the
                // wrapper package itself (`cargo run --package wrapper --features
                // wrapper-ui -- ...`). They are not the target package's feature
                // matrix, so leave even feature-looking flags untouched.
                Self::AliasWrapper {
                    cargo_args: args,
                    after_separator_args: extra_args,
                }
            }
            GeneratedArgPlacement::CargoCommand => {
                // Do not normalize feature-selection flags in `extra_args`.
                // Those args belong to an opaque wrapper/program; a
                // `--features` token there may be a wrapper flag, not Cargo
                // feature selection.
                let FeatureSelectionNormalization {
                    cargo,
                    removed,
                    saw_feature_selection,
                    ..
                } = normalize_feature_selection_args(args);
                Self::CargoCommand {
                    cargo_args: cargo,
                    extra_args,
                    removed_feature_args: removed,
                    has_unmanaged_feature_selection_args: saw_feature_selection,
                }
            }
        }
    }

    pub(crate) fn is_missing_command(&self) -> bool {
        match self {
            Self::CargoCommand {
                cargo_args,
                extra_args,
                ..
            } => cargo_args.is_empty() && extra_args.is_empty(),
            Self::AliasWrapper {
                cargo_args,
                after_separator_args,
            } => cargo_args.is_empty() && after_separator_args.is_empty(),
        }
    }

    pub(crate) fn has_message_format_arg_for_generated_args(&self) -> bool {
        match self {
            Self::CargoCommand { cargo_args, .. } => has_message_format_arg(cargo_args),
            Self::AliasWrapper {
                after_separator_args,
                ..
            } => {
                // Outer `cargo run --message-format` configures the wrapper
                // package. Generated diagnostics args are inserted after the
                // alias `--`, so only the wrapped command's own args conflict.
                let (wrapped_command_args, _) =
                    split_alias_args_at_wrapped_separator(after_separator_args);
                has_message_format_arg(wrapped_command_args)
            }
        }
    }

    pub(crate) fn with_generated_args<'b>(&'b self, generated_args: Vec<&'b str>) -> Vec<&'b str>
    where
        'a: 'b,
    {
        let mut args: Vec<&'b str> = self.cargo_args().to_vec();
        match self {
            Self::AliasWrapper {
                after_separator_args,
                ..
            } => {
                // The first `--` belongs to the alias expansion. If the
                // wrapped command has its own `--`, generated args must go
                // before that second separator, e.g.
                // `... -- lint -- bin-arg` becomes
                // `... -- lint --target T --features=F -- bin-arg`.
                let (wrapped_command_args, wrapped_extra_args) =
                    split_alias_args_at_wrapped_separator(after_separator_args);
                args.extend(wrapped_command_args.iter().copied());
                args.extend(generated_args);
                args.extend(wrapped_extra_args.iter().copied());
            }
            Self::CargoCommand { extra_args, .. } => {
                // Direct Cargo commands get generated args before `--`; args
                // after `--` belong to the user's binary and may coincidentally
                // look like Cargo flags.
                args.extend(generated_args);
                args.extend(extra_args.iter().copied());
            }
        }
        args
    }

    fn cargo_args(&self) -> &[&'a str] {
        match self {
            Self::CargoCommand { cargo_args, .. } | Self::AliasWrapper { cargo_args, .. } => {
                cargo_args
            }
        }
    }

    pub(crate) fn removed_feature_args(&self) -> &[&'a str] {
        match self {
            Self::CargoCommand {
                removed_feature_args,
                ..
            } => removed_feature_args,
            Self::AliasWrapper { .. } => &[],
        }
    }

    pub(crate) fn preserved_feature_selection_for_unknown_command(&self) -> bool {
        match self {
            Self::CargoCommand {
                cargo_args,
                has_unmanaged_feature_selection_args,
                ..
            } => {
                *has_unmanaged_feature_selection_args
                    && cargo_subcommand(cargo_args) == CargoSubcommand::Other
            }
            Self::AliasWrapper { .. } => false,
        }
    }

    #[cfg(test)]
    fn args_after_separator(&self) -> &[&'a str] {
        match self {
            Self::CargoCommand { extra_args, .. } => extra_args,
            Self::AliasWrapper {
                after_separator_args,
                ..
            } => after_separator_args,
        }
    }
}

fn split_alias_args_at_wrapped_separator<'slice, 'arg>(
    after_separator_args: &'slice [&'arg str],
) -> (&'slice [&'arg str], &'slice [&'arg str]) {
    let wrapped_separator_idx = after_separator_args
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, arg)| (*arg == "--").then_some(index))
        .unwrap_or(after_separator_args.len());
    after_separator_args.split_at(wrapped_separator_idx)
}

/// Normalize cargo feature-selection flags before running the matrix.
///
/// For known cargo subcommands, explicit feature-selection flags are removed
/// because `cargo-fc` supplies `--no-default-features` and `--features=...`
/// itself for each combination. For unresolved aliases and custom subcommands,
/// the arguments are left unchanged because they may interpret those flags
/// differently.
fn normalize_feature_selection_args(cargo_args: Vec<&str>) -> FeatureSelectionNormalization<'_> {
    fn feature_selection_span_length_at(args: &[&str], index: usize) -> Option<usize> {
        let arg = *args.get(index)?;

        // Cargo feature selection can appear either as a standalone flag
        // (`--all-features`, `--no-default-features`), as a flag followed by a
        // value (`--features foo`, `-F foo`), or inline (`--features=foo`,
        // `-Ffoo`). Return how many argv slots belong to that one logical flag.
        match arg {
            "--all-features" | "--no-default-features" => Some(1),
            "--features" | "-F" => {
                let has_value = args
                    .get(index + 1)
                    .is_some_and(|next_arg| !next_arg.starts_with('-'));
                Some(if has_value { 2 } else { 1 })
            }
            _ if arg.starts_with("--features=") || (arg.starts_with("-F") && arg.len() > 2) => {
                Some(1)
            }
            _ => None,
        }
    }

    let subcommand = cargo_subcommand(&cargo_args);
    if subcommand == CargoSubcommand::Other {
        // For unresolved aliases and custom subcommands, we cannot safely
        // assume that `--all-features` or `--features` belong to Cargo itself.
        // They may have their own meaning for those flags, so the only correct
        // behavior here is to leave the argv unchanged.
        let has_feature_selection_args = cargo_args.iter().enumerate().any(|(index, _arg)| {
            feature_selection_span_length_at(cargo_args.as_slice(), index).is_some()
        });
        return FeatureSelectionNormalization {
            cargo: cargo_args,
            removed: Vec::new(),
            saw_feature_selection: has_feature_selection_args,
        };
    }

    // For known cargo subcommands, the feature matrix owns feature selection.
    // Strip explicit user-provided feature flags here so the later
    // `--no-default-features --features=...` pair is the only feature
    // selection cargo sees for each combination.
    let mut forwarded_args = Vec::with_capacity(cargo_args.len());
    let mut removed_args = Vec::new();
    let mut index = 0;

    while let Some(arg) = cargo_args.get(index).copied() {
        if let Some(span_len) = feature_selection_span_length_at(cargo_args.as_slice(), index) {
            // Preserve the original tokens so the caller can emit one clear
            // warning describing exactly which flags were ignored.
            if let Some(span_args) = cargo_args.get(index..index + span_len) {
                removed_args.extend(span_args.iter().copied());
                debug_assert!(span_len > 0);
                index += span_len;
            } else {
                forwarded_args.push(arg);
                index += 1;
            }
        } else {
            forwarded_args.push(arg);
            index += 1;
        }
    }

    let has_feature_selection_args = !removed_args.is_empty();
    FeatureSelectionNormalization {
        cargo: forwarded_args,
        removed: removed_args,
        saw_feature_selection: has_feature_selection_args,
    }
}

fn has_message_format_arg(args: &[&str]) -> bool {
    args.iter()
        .any(|arg| *arg == "--message-format" || arg.starts_with("--message-format="))
}

#[cfg(test)]
mod test {
    use super::{GeneratedArgPlacement, PreparedInvocationArgs, normalize_feature_selection_args};
    use crate::cli::{CargoSubcommand, cargo_subcommand};
    use similar_asserts::assert_eq as sim_assert_eq;

    #[test]
    fn appends_generated_args_before_double_dash_for_cargo_run() {
        let invocation_args = PreparedInvocationArgs::new(
            vec!["run", "--package", "app", "--", "arg"],
            GeneratedArgPlacement::CargoCommand,
        );
        let args = invocation_args
            .with_generated_args(vec!["--no-default-features", "--features=default"]);

        sim_assert_eq!(
            args,
            vec![
                "run",
                "--package",
                "app",
                "--no-default-features",
                "--features=default",
                "--",
                "arg",
            ],
        );
    }

    #[test]
    fn appends_generated_args_after_double_dash_for_run_wrapper_aliases() {
        let invocation_args = PreparedInvocationArgs::new(
            vec!["run", "--package", "clippy-wrapper", "--", "lint"],
            GeneratedArgPlacement::AliasWrapper,
        );
        let args = invocation_args.with_generated_args(vec![
            crate::diagnostics_only::MESSAGE_FORMAT,
            "--package",
            "micromux-cli",
            "--no-default-features",
            "--features=default",
        ]);

        sim_assert_eq!(
            args,
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--",
                "lint",
                crate::diagnostics_only::MESSAGE_FORMAT,
                "--package",
                "micromux-cli",
                "--no-default-features",
                "--features=default",
            ],
        );
    }

    #[test]
    fn inserts_generated_args_before_wrapped_command_double_dash() {
        let invocation_args = PreparedInvocationArgs::new(
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--",
                "lint",
                "--fix",
                "--",
                "--program-arg",
                "--features",
                "program-feature",
            ],
            GeneratedArgPlacement::AliasWrapper,
        );
        let args = invocation_args.with_generated_args(vec![
            "--target",
            "wasm32-unknown-unknown",
            "--features=mcp",
        ]);

        sim_assert_eq!(
            args,
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--",
                "lint",
                "--fix",
                "--target",
                "wasm32-unknown-unknown",
                "--features=mcp",
                "--",
                "--program-arg",
                "--features",
                "program-feature",
            ],
        );
    }

    #[test]
    fn detects_user_message_format_after_double_dash_for_run_wrapper_aliases() {
        let wrapper = PreparedInvocationArgs::new(
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--",
                "lint",
                "--message-format=json",
            ],
            GeneratedArgPlacement::AliasWrapper,
        );
        let direct_run = PreparedInvocationArgs::new(
            vec!["run", "--package", "app", "--", "--message-format=json"],
            GeneratedArgPlacement::CargoCommand,
        );

        assert!(wrapper.has_message_format_arg_for_generated_args());
        assert!(!direct_run.has_message_format_arg_for_generated_args());
    }

    #[test]
    fn ignores_message_format_after_wrapped_command_double_dash() {
        let wrapper = PreparedInvocationArgs::new(
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--",
                "lint",
                "--",
                "--message-format=json",
            ],
            GeneratedArgPlacement::AliasWrapper,
        );

        assert!(!wrapper.has_message_format_arg_for_generated_args());
    }

    #[test]
    fn ignores_wrapper_cargo_message_format_for_run_wrapper_aliases() {
        let wrapper = PreparedInvocationArgs::new(
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--message-format=json",
                "--",
                "lint",
            ],
            GeneratedArgPlacement::AliasWrapper,
        );

        assert!(!wrapper.has_message_format_arg_for_generated_args());
    }

    #[test]
    fn message_format_detection_requires_the_cargo_flag_name() {
        let wrapper = PreparedInvocationArgs::new(
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--",
                "lint",
                "--message-formatting=json",
            ],
            GeneratedArgPlacement::AliasWrapper,
        );

        assert!(!wrapper.has_message_format_arg_for_generated_args());
    }

    #[test]
    fn leaves_feature_selection_flags_after_double_dash_untouched() {
        let invocation_args = PreparedInvocationArgs::new(
            vec![
                "run",
                "--package",
                "wrapper",
                "--",
                "lint",
                "--features",
                "wrapper-feature",
                "--all-features",
            ],
            GeneratedArgPlacement::CargoCommand,
        );

        sim_assert_eq!(
            invocation_args.cargo_args(),
            vec!["run", "--package", "wrapper"],
        );
        sim_assert_eq!(invocation_args.removed_feature_args(), Vec::<&str>::new());
        sim_assert_eq!(
            invocation_args.args_after_separator(),
            vec![
                "--",
                "lint",
                "--features",
                "wrapper-feature",
                "--all-features"
            ],
        );
    }

    #[test]
    fn preserves_wrapper_cargo_feature_flags_for_run_wrapper_aliases() {
        let invocation_args = PreparedInvocationArgs::new(
            vec![
                "run",
                "--package",
                "wrapper",
                "--features",
                "wrapper-feature",
                "--",
                "lint",
            ],
            GeneratedArgPlacement::AliasWrapper,
        );

        sim_assert_eq!(
            invocation_args.cargo_args(),
            vec![
                "run",
                "--package",
                "wrapper",
                "--features",
                "wrapper-feature",
            ],
        );
        sim_assert_eq!(invocation_args.removed_feature_args(), Vec::<&str>::new());
        sim_assert_eq!(invocation_args.args_after_separator(), vec!["--", "lint"]);
    }

    #[test]
    fn strips_feature_selection_flags_for_known_cargo_commands() {
        let normalization = normalize_feature_selection_args(vec![
            "--config",
            "net.retry=2",
            "clippy",
            "-vv",
            "--all-features",
            "--features",
            "foo,bar",
            "--no-default-features",
            "--color=always",
        ]);

        sim_assert_eq!(
            cargo_subcommand(&normalization.cargo),
            CargoSubcommand::Lint
        );
        sim_assert_eq!(
            normalization.cargo,
            vec!["--config", "net.retry=2", "clippy", "-vv", "--color=always"]
        );
        sim_assert_eq!(
            normalization.removed,
            vec![
                "--all-features",
                "--features",
                "foo,bar",
                "--no-default-features",
            ]
        );
        assert!(normalization.saw_feature_selection);
    }

    #[test]
    fn preserves_known_cargo_command_args_when_no_feature_selection_is_present() {
        let normalization = normalize_feature_selection_args(vec![
            "--config",
            "net.retry=2",
            "clippy",
            "--color=always",
        ]);

        sim_assert_eq!(
            cargo_subcommand(&normalization.cargo),
            CargoSubcommand::Lint
        );
        sim_assert_eq!(
            normalization.cargo,
            vec!["--config", "net.retry=2", "clippy", "--color=always"]
        );
        sim_assert_eq!(normalization.removed, Vec::<&str>::new());
        assert!(!normalization.saw_feature_selection);
    }

    #[test]
    fn preserves_feature_selection_flags_for_unknown_aliases() {
        let normalization = normalize_feature_selection_args(vec!["lint", "--all-features"]);

        sim_assert_eq!(
            cargo_subcommand(&normalization.cargo),
            CargoSubcommand::Other
        );
        sim_assert_eq!(normalization.cargo, vec!["lint", "--all-features"]);
        sim_assert_eq!(normalization.removed, Vec::<&str>::new());
        assert!(normalization.saw_feature_selection);
    }
}
