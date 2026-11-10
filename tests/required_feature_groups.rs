//! Integration tests for `required_feature_groups`.
//!
//! These load the setting from real manifests and resolve it along the full precedence chain,
//! `base → subcommands.<cmd> → target.'cfg(...)' → target.'cfg(...)'.subcommands.<cmd>`, through
//! the public planning and matrix API with deterministic stub adapters.

use assert_fs::prelude::*;
use color_eyre::eyre;
use color_eyre::eyre::OptionExt as _;
use indoc::{formatdoc, indoc};
use std::collections::BTreeSet;

mod common;
use common::{HostEnv, PairEval, matrix_rows_for_command, metadata, single_crate};

const LINUX: &str = "cfg(target_os = \"linux\")";
const UNIX: &str = "cfg(unix)";

/// Builds a single crate with independent features `a` to `d` and the given cargo-fc settings.
///
/// Every crate runs on the one configured target `t-linux`, so a `PairEval` decides which
/// `target.'cfg(...)'` sections match.
fn crate_with_settings(settings: &str) -> eyre::Result<assert_fs::TempDir> {
    single_crate(&formatdoc! {r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"

        [features]
        a = []
        b = []
        c = []
        d = []

        [package.metadata.cargo-fc]
        targets = ["t-linux"]
        {settings}
    "#})
}

/// The feature sets of the matrix one command resolves, each sorted.
fn feature_sets(
    settings: &str,
    command: Option<&str>,
    mut evaluator: PairEval,
) -> eyre::Result<BTreeSet<Vec<String>>> {
    let temp = crate_with_settings(settings)?;
    let meta = metadata(&temp)?;
    let rows = matrix_rows_for_command(&meta, command, &HostEnv("host-triple"), &mut evaluator)?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get("features").and_then(serde_json::Value::as_str))
        .map(|features| {
            let mut set = features
                .split(',')
                .filter(|feature| !feature.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            set.sort();
            set
        })
        .collect())
}

/// Every subset of `features`, the matrix of a crate with no matrix settings.
fn powerset(features: &[&str]) -> BTreeSet<Vec<String>> {
    (0..1u32 << features.len())
        .map(|mask| {
            features
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, feature)| (*feature).to_string())
                .collect()
        })
        .collect()
}

/// The rows of the unrestricted matrix that meet every group, by named features.
fn rows_meeting(groups: &[&[&str]]) -> BTreeSet<Vec<String>> {
    powerset(&["a", "b", "c", "d"])
        .into_iter()
        .filter(|row| {
            groups
                .iter()
                .all(|group| group.iter().any(|member| row.iter().any(|f| f == member)))
        })
        .collect()
}

fn on_linux() -> PairEval {
    PairEval::matching("t-linux", LINUX)
}

#[test_util::test]
fn base_groups_drop_rows_without_a_member() -> eyre::Result<()> {
    let settings = indoc! {r#"
        required_feature_groups = [["a", "b"]]
    "#};

    let have = feature_sets(settings, None, PairEval::default())?;

    // Every remaining row enables `a` or `b`, and `c` and `d` still vary around them.
    assert_eq!(have, rows_meeting(&[&["a", "b"]]));
    assert!(!have.contains(&Vec::new()));
    assert!(have.contains(&vec!["b".to_string(), "c".to_string(), "d".to_string()]));
    Ok(())
}

#[test_util::test]
fn several_groups_must_all_be_met() -> eyre::Result<()> {
    let settings = indoc! {r#"
        required_feature_groups = [["a", "b"], ["c"]]
    "#};

    let have = feature_sets(settings, None, PairEval::default())?;

    assert_eq!(have, rows_meeting(&[&["a", "b"], &["c"]]));
    Ok(())
}

#[test_util::test]
fn members_reached_through_implications_meet_the_group() -> eyre::Result<()> {
    let temp = single_crate(indoc! {r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"

        [features]
        default = ["rustls"]
        native-tls = []
        rustls = []
        rustls-webpki-roots = ["rustls"]
        logging = []

        [package.metadata.cargo-fc]
        required_feature_groups = [["native-tls", "rustls"]]
        mutually_exclusive_features = [["native-tls", "rustls"]]
        exclude_feature_sets = [
            ["default", "native-tls"],
            ["native-tls", "rustls-webpki-roots"],
        ]
    "#})?;
    let meta = metadata(&temp)?;
    let rows = matrix_rows_for_command(
        &meta,
        None,
        &HostEnv("host-triple"),
        &mut PairEval::default(),
    )?;
    let have = rows
        .iter()
        .filter_map(|row| row.get("features").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();

    // `default` and `rustls-webpki-roots` reach `rustls` only through their implications.
    assert!(have.contains("rustls-webpki-roots"), "{have:?}");
    assert!(have.contains("default"), "{have:?}");
    assert!(have.contains("logging,native-tls"), "{have:?}");

    // Rows that reach no backend are gone, and none combines the two backends.
    assert!(!have.contains(""), "{have:?}");
    assert!(!have.contains("logging"), "{have:?}");
    let names = |row: &str, feature: &str| row.split(',').any(|named| named == feature);
    assert!(
        !have.iter().any(|row| names(row, "native-tls")
            && ["rustls", "rustls-webpki-roots", "default"]
                .iter()
                .any(|other| names(row, other))),
        "{have:?}"
    );
    Ok(())
}

/// `tls = ["foo/bar"]` makes Cargo enable the optional dependency's implicit `foo` feature, so
/// `[tls]` meets the group even though `skip_optional_dependencies` keeps `foo` out of the matrix.
#[test_util::test]
fn dependency_features_meet_groups_through_the_optional_dependency() -> eyre::Result<()> {
    let temp = single_crate(indoc! {r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"

        [dependencies]
        foo = { path = "foo", optional = true }

        [features]
        native = []
        tls = ["foo/bar"]

        [package.metadata.cargo-fc]
        skip_optional_dependencies = true
        required_feature_groups = [["foo", "native"]]
    "#})?;
    temp.child("foo/Cargo.toml").write_str(indoc! {r#"
        [package]
        name = "foo"
        version = "0.1.0"
        edition = "2021"

        [features]
        bar = []
    "#})?;
    temp.child("foo/src/lib.rs").write_str("")?;
    let meta = metadata(&temp)?;
    let rows = matrix_rows_for_command(
        &meta,
        None,
        &HostEnv("host-triple"),
        &mut PairEval::default(),
    )?;
    let have = rows
        .iter()
        .filter_map(|row| row.get("features").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();

    assert_eq!(have, BTreeSet::from(["native", "native,tls", "tls"]));
    Ok(())
}

#[test_util::test]
fn target_add_extends_the_base_groups() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = {{ add = [["c"]] }}
    "#};

    // A matching target section adds its group to the base one.
    let linux = feature_sets(&settings, None, on_linux())?;
    assert_eq!(linux, rows_meeting(&[&["a", "b"], &["c"]]));

    // Without a match, only the base group applies.
    let other = feature_sets(&settings, None, PairEval::default())?;
    assert_eq!(other, rows_meeting(&[&["a", "b"]]));
    Ok(())
}

#[test_util::test]
fn target_remove_drops_a_base_group() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"], ["c"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = {{ remove = [["c"]] }}
    "#};

    let have = feature_sets(&settings, None, on_linux())?;

    assert_eq!(have, rows_meeting(&[&["a", "b"]]));
    Ok(())
}

#[test_util::test]
fn target_array_overrides_the_base_groups() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = [["c"]]
    "#};

    let have = feature_sets(&settings, None, on_linux())?;

    // The array replaces the base group, so rows with only `a` or `b` no longer qualify.
    assert_eq!(have, rows_meeting(&[&["c"]]));
    Ok(())
}

#[test_util::test]
fn add_wins_over_remove_in_one_patch() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = {{ remove = [["a", "b"]], add = [["b", "a"]] }}
    "#};

    let have = feature_sets(&settings, None, on_linux())?;

    // Remove applies before add, and groups compare as sets, so the group survives.
    assert_eq!(have, rows_meeting(&[&["a", "b"]]));
    Ok(())
}

#[test_util::test]
fn matching_target_sections_union_their_patches() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{UNIX}']
        required_feature_groups = {{ add = [["c"]] }}

        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = {{ remove = [["a", "b"]] }}
    "#};

    let have = feature_sets(
        &settings,
        None,
        PairEval::matching_all("t-linux", &[UNIX, LINUX]),
    )?;

    // Both sections apply: one adds `[c]` and the other removes the base group.
    assert_eq!(have, rows_meeting(&[&["c"]]));
    Ok(())
}

#[test_util::test]
fn matching_target_sections_with_different_overrides_conflict() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        [package.metadata.cargo-fc.target.'{UNIX}']
        required_feature_groups = [["a"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = [["b"]]
    "#};

    let err = feature_sets(
        &settings,
        None,
        PairEval::matching_all("t-linux", &[UNIX, LINUX]),
    )
    .err()
    .ok_or_eyre("two matching sections may not override the groups differently")?;

    assert!(
        format!("{err:?}").contains("required_feature_groups"),
        "{err:?}"
    );
    Ok(())
}

/// Each scope of the precedence chain overrides the broader ones, and a target section beats a
/// bare `subcommands.<cmd>` section.
#[test_util::test]
fn groups_follow_the_precedence_chain() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a"]]

        [package.metadata.cargo-fc.subcommands.test]
        required_feature_groups = [["b"]]

        [package.metadata.cargo-fc.subcommands.check]
        required_feature_groups = [["b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = [["c"]]

        [package.metadata.cargo-fc.target.'{LINUX}'.subcommands.test]
        required_feature_groups = [["d"]]
    "#};

    // Package base
    let have = feature_sets(&settings, Some("build"), PairEval::default())?;
    assert_eq!(have, rows_meeting(&[&["a"]]));

    // Subcommand over base
    let have = feature_sets(&settings, Some("test"), PairEval::default())?;
    assert_eq!(have, rows_meeting(&[&["b"]]));

    // Target over base
    let have = feature_sets(&settings, Some("build"), on_linux())?;
    assert_eq!(have, rows_meeting(&[&["c"]]));

    // Target over subcommand
    let have = feature_sets(&settings, Some("check"), on_linux())?;
    assert_eq!(have, rows_meeting(&[&["c"]]));

    // Target subcommand over everything
    let have = feature_sets(&settings, Some("test"), on_linux())?;
    assert_eq!(have, rows_meeting(&[&["d"]]));
    Ok(())
}

#[test_util::test]
fn subcommand_patch_builds_on_the_base_groups() -> eyre::Result<()> {
    let settings = indoc! {r#"
        required_feature_groups = [["a", "b"], ["c"]]

        [package.metadata.cargo-fc.subcommands.test]
        required_feature_groups = { remove = [["c"]], add = [["d"]] }
    "#};

    let have = feature_sets(settings, Some("test"), PairEval::default())?;
    assert_eq!(have, rows_meeting(&[&["a", "b"], &["d"]]));

    // Other commands keep the base groups.
    let have = feature_sets(settings, Some("build"), PairEval::default())?;
    assert_eq!(have, rows_meeting(&[&["a", "b"], &["c"]]));
    Ok(())
}

#[test_util::test]
fn inherit_false_discards_the_base_groups() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        inherit = false
    "#};

    let have = feature_sets(&settings, None, on_linux())?;

    // The fresh target config has no groups, so the full powerset returns.
    assert_eq!(have, powerset(&["a", "b", "c", "d"]));
    Ok(())
}

#[test_util::test]
fn inherit_false_rejects_group_patches() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        inherit = false
        required_feature_groups = {{ add = [["c"]] }}
    "#};

    let err = feature_sets(&settings, None, on_linux())
        .err()
        .ok_or_eyre("add has nothing to extend on a fresh config")?;

    assert!(
        format!("{err:?}").contains("required_feature_groups"),
        "{err:?}"
    );
    Ok(())
}

#[test_util::test]
fn target_exclusion_that_empties_the_matrix_is_an_error() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        exclude_features = ["a", "b"]
    "#};

    let err = feature_sets(&settings, None, on_linux())
        .err()
        .ok_or_eyre("a matrix the groups emptied must not pass silently")?;
    let message = format!("{err:?}");
    assert!(message.contains("[a, b]"), "{message}");
    assert!(message.contains("every generated combination"), "{message}");

    // Elsewhere the base matrix is unaffected.
    let have = feature_sets(&settings, None, PairEval::default())?;
    assert_eq!(have, rows_meeting(&[&["a", "b"]]));
    Ok(())
}

#[test_util::test]
fn exact_sets_keep_a_matrix_the_groups_emptied() -> eyre::Result<()> {
    let settings = formatdoc! {r#"
        required_feature_groups = [["a", "b"]]

        [package.metadata.cargo-fc.target.'{LINUX}']
        exclude_features = ["a", "b"]
        include_feature_sets = [["a", "c"]]
    "#};

    let have = feature_sets(&settings, None, on_linux())?;

    assert_eq!(
        have,
        BTreeSet::from([vec!["a".to_string(), "c".to_string()]])
    );
    Ok(())
}

#[test_util::test]
fn manifest_rejects_an_empty_group() -> eyre::Result<()> {
    let settings = formatdoc! {r"
        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = {{ add = [[]] }}
    "};

    let err = feature_sets(&settings, None, PairEval::default())
        .err()
        .ok_or_eyre("an empty group can never be met")?;

    assert!(format!("{err:?}").contains("empty group"), "{err:?}");
    Ok(())
}

#[test_util::test]
fn manifest_rejects_unknown_members_in_any_scope() -> eyre::Result<()> {
    // The section does not match, yet the typo still fails config load.
    let settings = formatdoc! {r#"
        [package.metadata.cargo-fc.target.'{LINUX}']
        required_feature_groups = [["a", "typo"]]
    "#};

    let err = feature_sets(&settings, None, PairEval::default())
        .err()
        .ok_or_eyre("unknown group members must be rejected")?;

    assert!(
        format!("{err:?}").contains(&format!(
            "`typo` (target.'{LINUX}'.required_feature_groups)"
        )),
        "{err:?}"
    );
    Ok(())
}

#[test_util::test]
fn workspace_scope_rejects_required_groups() -> eyre::Result<()> {
    let temp = single_crate(indoc! {r#"
        [package]
        name = "solo"
        version = "0.1.0"
        edition = "2021"

        [features]
        a = []

        [workspace.metadata.cargo-fc]
        required_feature_groups = [["a"]]
    "#})?;
    let meta = metadata(&temp)?;

    let err = matrix_rows_for_command(
        &meta,
        None,
        &HostEnv("host-triple"),
        &mut PairEval::default(),
    )
    .err()
    .ok_or_eyre("feature-matrix settings are per-package")?;

    assert!(format!("{err:?}").contains("per-package"), "{err:?}");
    Ok(())
}
