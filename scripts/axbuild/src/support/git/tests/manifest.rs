use std::{collections::BTreeSet, path::PathBuf};

use super::common::test_workspace;
use crate::support::git::{
    IncrementalPackageSelection,
    manifest::{RootManifestChange, classify_root_manifest_change},
    selection::select_incremental_packages_for_paths_with_root_manifest_change,
};

#[test]
fn root_cargo_toml_workspace_dependency_change_keeps_incremental_package_selection() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths_with_root_manifest_change(
        root.path(),
        &metadata,
        &workspace_packages,
        [
            PathBuf::from("Cargo.toml"),
            PathBuf::from("crates/beta/Cargo.toml"),
        ],
        Some(RootManifestChange::LocalWorkspaceDependencies(
            BTreeSet::from(["beta".to_string()]),
        )),
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
fn root_cargo_toml_semantic_noop_selects_no_packages() {
    let old_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;
    let new_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            # Comment-only edits should not force all clippy packages.
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;
    let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();
    assert_eq!(
        change,
        RootManifestChange::LocalWorkspaceDependencies(BTreeSet::new())
    );

    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths_with_root_manifest_change(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("Cargo.toml")],
        Some(change),
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
fn root_cargo_toml_workspace_package_metadata_change_selects_no_packages() {
    let old_manifest = r#"
            [workspace.package]
            version = "0.5.11"
            authors = ["RCore Team <yuchen@tsinghua.edu.cn>"]

            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;
    let new_manifest = r#"
            [workspace.package]
            version = "0.5.12"
            authors = ["RCore Team <yuchen@tsinghua.edu.cn>", "CI Bot <ci@example.com>"]

            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;
    let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();
    assert_eq!(
        change,
        RootManifestChange::LocalWorkspaceDependencies(BTreeSet::new())
    );

    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths_with_root_manifest_change(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("Cargo.toml")],
        Some(change),
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
fn root_cargo_toml_semantic_noop_keeps_incremental_package_selection() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths_with_root_manifest_change(
        root.path(),
        &metadata,
        &workspace_packages,
        [
            PathBuf::from("Cargo.toml"),
            PathBuf::from("crates/beta/src/lib.rs"),
        ],
        Some(RootManifestChange::LocalWorkspaceDependencies(
            BTreeSet::new(),
        )),
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
fn root_cargo_toml_workspace_dependency_change_skips_removed_packages() {
    let (root, metadata, workspace_packages) = test_workspace();
    let selected = select_incremental_packages_for_paths_with_root_manifest_change(
        root.path(),
        &metadata,
        &workspace_packages,
        [PathBuf::from("Cargo.toml")],
        Some(RootManifestChange::LocalWorkspaceDependencies(
            BTreeSet::from(["beta".to_string(), "removed".to_string()]),
        )),
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
fn root_manifest_classifier_uses_package_name_for_local_dependency_alias() {
    let old_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha_alias = { version = "0.1.0", path = "crates/alpha", package = "alpha" }
        "#;
    let new_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha_alias = { version = "0.2.0", path = "crates/alpha", package = "alpha" }
        "#;

    let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();

    assert_eq!(
        change,
        RootManifestChange::LocalWorkspaceDependencies(BTreeSet::from(["alpha".to_string()]))
    );
}

#[test]
fn root_manifest_classifier_tracks_local_dependency_alias_package_change() {
    let old_manifest = r#"
            [workspace]
            members = ["crates/alpha", "crates/beta"]

            [workspace.dependencies]
            local_alias = { version = "0.1.0", path = "crates/alpha", package = "alpha" }
        "#;
    let new_manifest = r#"
            [workspace]
            members = ["crates/alpha", "crates/beta"]

            [workspace.dependencies]
            local_alias = { version = "0.1.0", path = "crates/beta", package = "beta" }
        "#;

    let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();

    assert_eq!(
        change,
        RootManifestChange::LocalWorkspaceDependencies(BTreeSet::from([
            "alpha".to_string(),
            "beta".to_string()
        ]))
    );
}

#[test]
fn root_manifest_classifier_accepts_local_workspace_dependency_removal() {
    let old_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
            beta = { version = "0.1.0", path = "crates/beta" }
        "#;
    let new_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;

    let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();

    assert_eq!(
        change,
        RootManifestChange::LocalWorkspaceDependencies(BTreeSet::from(["beta".to_string()]))
    );
}

#[test]
fn root_manifest_classifier_keeps_external_dependency_changes_hard() {
    let old_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            anyhow = "1.0"
        "#;
    let new_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            anyhow = "2.0"
        "#;

    let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();

    assert_eq!(change, RootManifestChange::Hard);
}

#[test]
fn root_manifest_classifier_keeps_workspace_members_changes_hard() {
    let old_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;
    let new_manifest = r#"
            [workspace]
            members = ["crates/alpha", "crates/beta"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;

    let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();

    assert_eq!(change, RootManifestChange::Hard);
}
