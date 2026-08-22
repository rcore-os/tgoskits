use std::path::PathBuf;

use super::common::test_workspace;
use crate::support::git::{
    IncrementalPackageSelection, selection::select_incremental_packages_for_paths,
};

#[test]
fn changed_top_level_crate_affected_set_is_only_itself() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("crates/gamma/src/lib.rs")],
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: vec!["gamma".into()],
            affected: vec!["gamma".into()],
        }
    );
}

#[test]
fn changed_crate_selects_reverse_dependencies() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("crates/alpha/src/lib.rs")],
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: vec!["alpha".into()],
            affected: vec!["alpha".into(), "beta".into(), "gamma".into()],
        }
    );
}

#[test]
fn changed_middle_crate_selects_itself_and_dependents() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("crates/beta/src/lib.rs")],
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: vec!["beta".into()],
            affected: vec!["beta".into(), "gamma".into()],
        }
    );
}

#[test]
fn no_changes_selects_no_packages() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        Vec::<PathBuf>::new(),
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: Vec::new(),
            affected: Vec::new(),
        }
    );
}

#[test]
fn markdown_inside_crates_does_not_expand_incremental_packages() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [
            PathBuf::from("crates/alpha/README.md"),
            PathBuf::from("crates/beta/src/lib.rs"),
        ],
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: vec!["beta".into()],
            affected: vec!["beta".into(), "gamma".into()],
        }
    );
}

#[test]
fn lockfile_only_change_falls_back_to_full() {
    // Cargo.lock is Soft: a dep-version-only update with no source changes
    // can still affect compilation via transitive deps, proc macros, or
    // build scripts, so a pure lockfile diff must trigger a full run.
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("Cargo.lock")],
    )
    .unwrap();

    assert!(matches!(
        selected,
        IncrementalPackageSelection::Full { reason } if reason.contains("Cargo.lock")
    ));
}

#[test]
fn lockfile_change_keeps_incremental_selection_when_packages_changed() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [
            PathBuf::from("Cargo.lock"),
            PathBuf::from("crates/beta/Cargo.toml"),
        ],
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: vec!["beta".into()],
            affected: vec!["beta".into(), "gamma".into()],
        }
    );
}

#[test]
fn root_cargo_toml_only_falls_back_to_full() {
    // Without the semantic old/new root-manifest classification, the path-only
    // selector must conservatively treat Cargo.toml as a hard global input.
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("Cargo.toml")],
    )
    .unwrap();

    assert!(matches!(
        selected,
        IncrementalPackageSelection::Full { reason } if reason.contains("Cargo.toml")
    ));
}

#[test]
fn root_cargo_toml_with_package_change_still_falls_back_to_full() {
    // The production --since path supplies a semantic old/new root-manifest
    // classification. Without it, a package path cannot make an otherwise
    // unknown Cargo.toml change safe for incremental selection.
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [
            PathBuf::from("Cargo.toml"),
            PathBuf::from("crates/alpha/src/lib.rs"),
        ],
    )
    .unwrap();

    assert!(matches!(
        selected,
        IncrementalPackageSelection::Full { reason } if reason.contains("Cargo.toml")
    ));
}
#[test]
fn global_config_file_falls_back_to_full_run() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from(".cargo/config.toml")],
    )
    .unwrap();

    assert!(matches!(
        selected,
        IncrementalPackageSelection::Full { reason } if reason.contains(".cargo")
    ));
}

#[test]
fn unrelated_outside_package_file_selects_no_packages() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("docs/guide.md")],
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: Vec::new(),
            affected: Vec::new(),
        }
    );
}

#[test]
fn unrelated_outside_package_file_does_not_hide_package_changes() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths(
        root.path(),
        &metadata,
        &workspace_packages,
        [
            PathBuf::from(".github/workflows/review.yml"),
            PathBuf::from("crates/beta/src/lib.rs"),
        ],
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: vec!["beta".into()],
            affected: vec!["beta".into(), "gamma".into()],
        }
    );
}
