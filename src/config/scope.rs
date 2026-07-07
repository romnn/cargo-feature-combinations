use super::{
    Config, FeatureMatrixPatch, FlagConfig, ScopeConfig, TargetOverride, WorkspaceConfig,
    WorkspaceTargetOverride,
};
use crate::config::patch::{StringSetPatch, TargetListPatch};

/// One position in the config precedence chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ScopeId {
    WorkspaceBase,
    WorkspaceCommand,
    WorkspaceTarget,
    WorkspaceTargetCommand,
    PackageBase,
    PackageCommand,
    PackageTarget,
    PackageTargetCommand,
}

impl ScopeId {
    #[must_use]
    pub(crate) const fn is_command(self) -> bool {
        matches!(
            self,
            Self::WorkspaceCommand
                | Self::WorkspaceTargetCommand
                | Self::PackageCommand
                | Self::PackageTargetCommand
        )
    }

    #[must_use]
    pub(crate) const fn is_package(self) -> bool {
        matches!(
            self,
            Self::PackageBase
                | Self::PackageCommand
                | Self::PackageTarget
                | Self::PackageTargetCommand
        )
    }

    #[must_use]
    pub(crate) const fn source_kind(self) -> &'static str {
        match self {
            Self::WorkspaceBase => "workspace config",
            Self::WorkspaceCommand => "workspace subcommand override",
            Self::WorkspaceTarget => "workspace target override",
            Self::WorkspaceTargetCommand => "workspace target subcommand override",
            Self::PackageBase => "package config",
            Self::PackageCommand => "package subcommand override",
            Self::PackageTarget => "target override",
            Self::PackageTargetCommand => "target subcommand override",
        }
    }
}

/// Uniform borrowed view of one scope's payload.
#[derive(Clone, Copy, Default)]
pub(crate) struct ScopeView<'a> {
    pub(crate) replace: bool,
    pub(crate) driver: Option<&'a str>,
    pub(crate) expand_targets: Option<bool>,
    pub(crate) targets: Option<&'a TargetListPatch>,
    pub(crate) exclude_packages: Option<&'a StringSetPatch>,
    pub(crate) features: Option<&'a FeatureMatrixPatch>,
    pub(crate) flags: FlagConfig,
}

pub(crate) struct Layer<'a> {
    pub(crate) scope: ScopeId,
    pub(crate) command: Option<&'a str>,
    pub(crate) entries: Vec<(&'a str, ScopeView<'a>)>,
}

pub(crate) struct Chain<'a> {
    pub(crate) layers: Vec<Layer<'a>>,
}

impl<'a> Chain<'a> {
    #[must_use]
    pub(crate) fn base(
        ws: &'a WorkspaceConfig,
        pkg: Option<&'a Config>,
        raw: Option<&'a str>,
        resolved: Option<&'a str>,
    ) -> Self {
        let mut layers = vec![Layer {
            scope: ScopeId::WorkspaceBase,
            command: None,
            entries: vec![("", workspace_base_view(ws))],
        }];
        if let Some((name, scope)) = selected_command_entry(raw, resolved, &ws.base.subcommands) {
            layers.push(command_layer(ScopeId::WorkspaceCommand, name, "", scope));
        }
        if let Some(pkg) = pkg {
            layers.push(Layer {
                scope: ScopeId::PackageBase,
                command: None,
                entries: vec![("", package_base_view(pkg))],
            });
            if let Some((name, scope)) =
                selected_command_entry(raw, resolved, &pkg.base.subcommands)
            {
                layers.push(command_layer(ScopeId::PackageCommand, name, "", scope));
            }
        }
        Self { layers }
    }

    #[must_use]
    pub(crate) fn workspace(
        ws: &'a WorkspaceConfig,
        ws_matched: &'a [(String, &'a WorkspaceTargetOverride)],
        raw: Option<&'a str>,
        resolved: Option<&'a str>,
    ) -> Self {
        let mut chain = Self::base(ws, None, raw, resolved);
        if !ws_matched.is_empty() {
            chain.layers.push(Layer {
                scope: ScopeId::WorkspaceTarget,
                command: None,
                entries: ws_matched
                    .iter()
                    .map(|(expr, section)| (expr.as_str(), scope_view(&section.settings)))
                    .collect(),
            });
            let (command, entries) =
                target_command_entries(ws_matched.iter().map(|(expr, section)| {
                    (
                        expr.as_str(),
                        selected_command_entry(raw, resolved, &section.subcommands),
                    )
                }));
            if !entries.is_empty() {
                chain.layers.push(Layer {
                    scope: ScopeId::WorkspaceTargetCommand,
                    command,
                    entries,
                });
            }
        }
        chain
    }

    #[must_use]
    pub(crate) fn full(
        ws: &'a WorkspaceConfig,
        ws_matched: &'a [(String, &'a WorkspaceTargetOverride)],
        pkg: &'a Config,
        pkg_matched: Vec<(&'a str, &'a TargetOverride)>,
        raw: Option<&'a str>,
        resolved: Option<&'a str>,
    ) -> Self {
        let mut layers = Vec::new();
        let workspace = Self::workspace(ws, ws_matched, raw, resolved);
        layers.extend(workspace.layers);
        layers.push(Layer {
            scope: ScopeId::PackageBase,
            command: None,
            entries: vec![("", package_base_view(pkg))],
        });
        if let Some((name, scope)) = selected_command_entry(raw, resolved, &pkg.base.subcommands) {
            layers.push(command_layer(ScopeId::PackageCommand, name, "", scope));
        }
        if !pkg_matched.is_empty() {
            layers.push(Layer {
                scope: ScopeId::PackageTarget,
                command: None,
                entries: pkg_matched
                    .iter()
                    .map(|(expr, section)| (*expr, scope_view(&section.settings)))
                    .collect(),
            });
            let (command, entries) =
                target_command_entries(pkg_matched.into_iter().map(|(expr, section)| {
                    (
                        expr,
                        selected_command_entry(raw, resolved, &section.subcommands),
                    )
                }));
            if !entries.is_empty() {
                layers.push(Layer {
                    scope: ScopeId::PackageTargetCommand,
                    command,
                    entries,
                });
            }
        }
        Self { layers }
    }
}

fn workspace_base_view(ws: &WorkspaceConfig) -> ScopeView<'_> {
    scope_view(&ws.base.settings)
}

fn package_base_view(pkg: &Config) -> ScopeView<'_> {
    scope_view(&pkg.base.settings)
}

fn scope_view(scope: &ScopeConfig) -> ScopeView<'_> {
    ScopeView {
        replace: scope.replace,
        driver: scope.driver.as_deref(),
        expand_targets: scope.expand_targets,
        targets: scope.targets.as_ref(),
        exclude_packages: scope.exclude_packages.as_ref(),
        features: Some(&scope.features),
        flags: scope.flags,
    }
}

fn command_layer<'a>(
    scope: ScopeId,
    command: &'a str,
    label: &'a str,
    command_scope: &'a ScopeConfig,
) -> Layer<'a> {
    Layer {
        scope,
        command: Some(command),
        entries: vec![(label, scope_view(command_scope))],
    }
}

fn target_command_entries<'a>(
    commands: impl IntoIterator<Item = (&'a str, Option<(&'a str, &'a ScopeConfig)>)>,
) -> (Option<&'a str>, Vec<(&'a str, ScopeView<'a>)>) {
    let mut command_name = None;
    let mut entries = Vec::new();
    for (expr, command) in commands {
        if let Some((name, scope)) = command {
            command_name.get_or_insert(name);
            entries.push((expr, scope_view(scope)));
        }
    }
    (command_name, entries)
}

fn selected_command_entry<'a>(
    raw: Option<&str>,
    resolved: Option<&str>,
    subcommands: &'a std::collections::BTreeMap<String, ScopeConfig>,
) -> Option<(&'a str, &'a ScopeConfig)> {
    if let Some(raw) = raw
        && let Some(entry) = command_entry_for_token(raw, subcommands)
    {
        return Some(entry);
    }
    if resolved == raw {
        None
    } else {
        resolved.and_then(|token| command_entry_for_token(token, subcommands))
    }
}

fn command_entry_for_token<'a>(
    token: &str,
    subcommands: &'a std::collections::BTreeMap<String, ScopeConfig>,
) -> Option<(&'a str, &'a ScopeConfig)> {
    if let Some((name, scope)) = subcommands.get_key_value(token) {
        return Some((name.as_str(), scope));
    }
    let canonical = crate::cli::builtin_canonical_command(token)?;
    subcommands
        .get_key_value(canonical)
        .map(|(name, scope)| (name.as_str(), scope))
}

#[cfg(test)]
mod tests {
    use super::command_entry_for_token;
    use crate::config::ScopeConfig;
    use std::collections::BTreeMap;

    #[test]
    fn builtin_short_alias_inherits_long_command_policy() {
        let subcommands = BTreeMap::from([(
            "build".to_string(),
            ScopeConfig {
                expand_targets: Some(false),
                ..ScopeConfig::default()
            },
        )]);

        let override_config = command_entry_for_token("b", &subcommands);

        assert_eq!(
            override_config.and_then(|(_name, config)| config.expand_targets),
            Some(false)
        );
    }

    #[test]
    fn builtin_short_alias_exact_policy_wins_over_long_command_policy() {
        let subcommands = BTreeMap::from([
            (
                "build".to_string(),
                ScopeConfig {
                    expand_targets: Some(false),
                    ..ScopeConfig::default()
                },
            ),
            (
                "b".to_string(),
                ScopeConfig {
                    expand_targets: Some(true),
                    ..ScopeConfig::default()
                },
            ),
        ]);

        let override_config = command_entry_for_token("b", &subcommands);

        assert_eq!(
            override_config.and_then(|(_name, config)| config.expand_targets),
            Some(true)
        );
    }

    #[test]
    fn command_entry_returns_matched_name() {
        let subcommands = BTreeMap::from([("test".to_string(), ScopeConfig::default())]);

        let (name, _config) = command_entry_for_token("t", &subcommands).expect("alias match");

        assert_eq!(name, "test");
    }
}
