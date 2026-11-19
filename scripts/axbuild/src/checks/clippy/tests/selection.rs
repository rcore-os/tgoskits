use std::collections::HashSet;

use super::common::{args, metadata_with_packages, pkg};
use crate::checks::clippy::selection::{
    resolve_requested_packages, skip_unsupported_packages, validate_requested_packages,
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

    assert!(packages.iter().any(|pkg| pkg.name == "alpha"));
    assert!(packages.iter().any(|pkg| pkg.name == "beta"));
    assert!(packages.iter().all(|pkg| pkg.name != "gamma"));
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
    let resolved = resolve_requested_packages(&args(false, &[]), &packages).unwrap();

    assert!(resolved.iter().any(|pkg| pkg.name == "alpha"));
    assert!(resolved.iter().any(|pkg| pkg.name == "beta"));
}

#[test]
fn package_selection_overrides_default_workspace_selection() {
    let packages = vec![
        pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
        pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
    ];
    let resolved = resolve_requested_packages(&args(false, &["beta"]), &packages).unwrap();

    assert!(resolved.iter().any(|pkg| pkg.name == "beta"));
    assert!(resolved.iter().all(|pkg| pkg.name != "alpha"));
}

#[test]
fn unsupported_packages_are_filtered_from_generic_clippy() {
    let packages = vec![
        pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
        pkg(
            "axvisor",
            "axvisor 0.1.0 (path+file:///tmp/axvisor)",
            &[],
            None,
        ),
        pkg("mingo", "mingo 0.1.0 (path+file:///tmp/mingo)", &[], None),
    ];
    let resolved = resolve_requested_packages(&args(false, &[]), &packages).unwrap();

    let filtered = skip_unsupported_packages(resolved);

    assert!(filtered.iter().any(|package| package.name == "alpha"));
    assert!(filtered.iter().all(|package| package.name != "axvisor"));
    assert!(filtered.iter().all(|package| package.name != "mingo"));
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
fn discovered_packages_cannot_disappear_between_selection_and_check_expansion() {
    use super::common::expand;
    use crate::checks::clippy::selection::validate_package_coverage;
    let mut packages = vec![pkg("fixture-old", "fixture-old-id", &[], None)];
    let mut checks = expand(&packages);
    validate_package_coverage(&packages, &checks).unwrap();
    packages.push(pkg(
        "fixture-new",
        "fixture-new-id",
        &[("capability", &[])],
        None,
    ));
    assert!(
        validate_package_coverage(&packages, &checks)
            .unwrap_err()
            .to_string()
            .contains("fixture-new")
    );
    checks = expand(&packages);
    validate_package_coverage(&packages, &checks).unwrap();
    assert!(checks.iter().any(|check| check.package == "fixture-new"));
    let mut unknown = checks[0].clone();
    unknown.package = "not-a-member".into();
    checks.push(unknown);
    assert!(validate_package_coverage(&packages, &checks).is_err());
}
