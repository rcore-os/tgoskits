use std::{collections::BTreeSet, path::PathBuf};

use cargo_metadata::Metadata;

use super::common::{package, test_workspace};
use crate::support::git::{
    IncrementalPackageSelection, selection::select_incremental_packages_for_paths,
    top_level_affected_workspace_packages,
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
fn top_level_frontier_covers_a_dependency_cycle_at_the_top() {
    // `a` and `b` form a cycle (only reachable through dev-dependencies) and
    // sit at the top of the affected set. The bare "maximal element" rule
    // drops both; the coverage guarantee must still promote one as a root so
    // the whole cycle is linted with-deps.
    let root = tempfile::tempdir().unwrap();
    let ru = root.path().display().to_string();
    let a = format!("a 0.1.0 (path+file://{ru}/crates/a)");
    let b = format!("b 0.1.0 (path+file://{ru}/crates/b)");
    let leaf = format!("leaf 0.1.0 (path+file://{ru}/crates/leaf)");
    let dep = |name: &str, pkg: &str| {
        serde_json::json!({
            "name": name,
            "pkg": pkg,
            "dep_kinds": [{ "kind": null, "target": null }]
        })
    };
    let value = serde_json::json!({
        "packages": [
            package(root.path(), "a", &["b", "leaf"]),
            package(root.path(), "b", &["a", "leaf"]),
            package(root.path(), "leaf", &[]),
        ],
        "workspace_members": [a, b, leaf],
        "workspace_default_members": [a, b, leaf],
        "resolve": {
            "nodes": [
                { "id": a, "dependencies": [b, leaf], "deps": [dep("b", &b), dep("leaf", &leaf)], "features": [] },
                { "id": b, "dependencies": [a, leaf], "deps": [dep("a", &a), dep("leaf", &leaf)], "features": [] },
                { "id": leaf, "dependencies": [], "deps": [], "features": [] },
            ],
            "root": null
        },
        "target_directory": root.path().join("target"),
        "version": 1,
        "workspace_root": root.path(),
        "metadata": null,
    });
    let metadata: Metadata = serde_json::from_value(value).unwrap();
    let packages = metadata.packages.clone();

    let affected = BTreeSet::from(["a".to_string(), "b".to_string(), "leaf".to_string()]);
    let frontier = top_level_affected_workspace_packages(&metadata, &packages, &affected);

    // One cycle representative is promoted; its with-deps run covers the whole
    // cycle plus `leaf`. The bare maximal-element rule would return an empty
    // frontier and silently skip `a`/`b`.
    assert_eq!(frontier, vec!["a".to_string()]);
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
    // Root Cargo.toml is Hard: a manifest-only change with no code changes
    // (e.g. a [workspace.dependencies] bump) must still fall back to Full.
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
    // Root Cargo.toml is Hard: even when package source files are also in the
    // diff (e.g. a new crate was added *and* a workspace dependency was
    // bumped), the global manifest change requires a full run.  We cannot
    // distinguish "only added a member" from "bumped a workspace dep" without
    // parsing diff hunks, so Hard must always win.
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
