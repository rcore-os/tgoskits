use std::{collections::HashSet, path::Path};

use super::common::{args, metadata_with_packages, pkg};
use crate::clippy::selection::{
    resolve_requested_packages, validate_clippy_args, validate_requested_packages,
    workspace_packages,
};

#[test]
fn workspace_package_extraction_keeps_only_workspace_members() {
    let metadata = metadata_with_packages(
        vec![
            pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("gamma", "gamma 0.1.0 (path+file:///tmp/gamma)", &[], None),
        ],
        &[
            "beta 0.1.0 (path+file:///tmp/beta)",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
        ],
    );

    let packages = workspace_packages(&metadata);

    assert_eq!(
        packages
            .iter()
            .map(|pkg| pkg.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
}

fn known_packages() -> HashSet<&'static str> {
    HashSet::from(["alpha", "beta", "gamma"])
}

#[test]
fn default_mode_selects_every_workspace_package() {
    let packages = vec![
        pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
        pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
    ];
    let metadata = metadata_with_packages(
        packages.clone(),
        &[
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            "beta 0.1.0 (path+file:///tmp/beta)",
        ],
    );
    let resolved = resolve_requested_packages(
        &args(false, &[]),
        Path::new("/tmp/ws"),
        &metadata,
        &packages,
    )
    .unwrap();

    assert_eq!(
        resolved
            .iter()
            .map(|pkg| pkg.package.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
}

#[test]
fn package_selection_overrides_default_workspace_selection() {
    let packages = vec![
        pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
        pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
    ];
    let metadata = metadata_with_packages(
        packages.clone(),
        &[
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            "beta 0.1.0 (path+file:///tmp/beta)",
        ],
    );
    let resolved = resolve_requested_packages(
        &args(false, &["beta"]),
        Path::new("/tmp/ws"),
        &metadata,
        &packages,
    )
    .unwrap();

    assert_eq!(
        resolved
            .iter()
            .map(|pkg| pkg.package.name.as_str())
            .collect::<Vec<_>>(),
        vec!["beta"]
    );
}

#[test]
fn duplicate_explicit_packages_are_rejected() {
    let known = known_packages();
    let err = validate_requested_packages(&["alpha".into(), "alpha".into()], &known).unwrap_err();

    assert!(
        err.to_string()
            .contains("duplicate workspace package `alpha`")
    );
}

#[test]
fn since_rejects_explicit_package_selection() {
    let mut args = args(false, &["alpha"]);
    args.since = Some("origin/main".to_string());

    let err = validate_clippy_args(&args).unwrap_err();

    assert!(
        err.to_string()
            .contains("cannot be combined with `--package`")
    );
}

#[test]
fn since_rejects_all_selection() {
    let mut args = args(true, &[]);
    args.since = Some("origin/main".to_string());

    let err = validate_clippy_args(&args).unwrap_err();

    assert!(err.to_string().contains("cannot be combined with `--all`"));
}
