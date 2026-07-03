use anyhow::bail;

use super::{
    ARCEOS_AXTEST_GROUP, ARCEOS_C_TEST_GROUP, ARCEOS_RUST_TEST_GROUP, ARCEOS_TEST_SUITE_OS,
    assets::{
        arceos_c_test_dir, arceos_rust_test_dir, arceos_test_group_dir, arceos_test_suit_qemu_archs,
    },
    c_qemu::{
        arceos_c_test_suit_build_config_path, arceos_c_test_suit_qemu_config_path,
        c_qemu_features_for_list,
    },
    discovery::{
        arceos_test_suit_build_config_path, arceos_test_suit_qemu_config_path,
        discover_qemu_cases_in_dir,
    },
    rust_qemu::rust_qemu_features_for_list,
};
use crate::{
    arceos::ArceOS,
    test::{qemu as qemu_test, suite as test_suite},
};

pub(super) fn list_rust_qemu_cases(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Option<String>> {
    let cases = rust_qemu_case_names(arceos, target, selected_case, allow_missing_selected_case)?;
    if cases.is_empty() {
        return Ok(None);
    }
    Ok(Some(qemu_test::render_case_tree(
        ARCEOS_RUST_TEST_GROUP,
        cases,
    )))
}

pub(super) fn list_c_qemu_cases(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let cases = c_qemu_case_names(arceos, target, selected_case)?;
    if cases.is_empty() {
        return Ok(None);
    }
    Ok(Some(qemu_test::render_case_tree(
        ARCEOS_C_TEST_GROUP,
        cases,
    )))
}

pub(super) fn list_generic_qemu_cases(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let dir = arceos_test_group_dir(arceos.app.workspace_root(), group);
    let cases: Vec<String> = match target {
        Some((arch, target)) => {
            discover_qemu_cases_in_dir(&dir, arch, target, selected_case, group)?
                .into_iter()
                .map(|case| case.case.name)
                .collect()
        }
        None => qemu_test::discover_all_qemu_cases(&dir, selected_case, "ArceOS", group)
            .map_err(anyhow::Error::new)?,
    };
    if cases.is_empty() {
        return Ok(None);
    }
    Ok(Some(qemu_test::render_case_tree(group, cases)))
}

pub(super) fn all_qemu_case_groups(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<(String, Vec<qemu_test::ListedQemuCase>)>> {
    let mut groups = Vec::new();
    for group in
        test_suite::discover_group_names(arceos.app.workspace_root(), ARCEOS_TEST_SUITE_OS)?
    {
        let cases: Option<Vec<qemu_test::ListedQemuCase>> = match group.as_str() {
            ARCEOS_RUST_TEST_GROUP => rust_qemu_listed_cases(arceos, selected_case)
                .ok()
                .filter(|cases| !cases.is_empty()),
            ARCEOS_C_TEST_GROUP => c_qemu_listed_cases(arceos, selected_case)
                .ok()
                .filter(|v| !v.is_empty()),
            ARCEOS_AXTEST_GROUP => {
                let dir = arceos_test_group_dir(arceos.app.workspace_root(), &group);
                match qemu_test::discover_all_qemu_cases_with_archs(
                    &dir,
                    selected_case,
                    "ArceOS",
                    &group,
                ) {
                    Ok(cases) if !cases.is_empty() => Some(cases),
                    Ok(_) => None,
                    Err(err) if qemu_list_error_is_ignorable(err.kind()) => None,
                    Err(err) => return Err(anyhow::Error::new(err)),
                }
            }
            _ => {
                let dir = arceos_test_group_dir(arceos.app.workspace_root(), &group);
                match qemu_test::discover_all_qemu_cases_with_archs(
                    &dir,
                    selected_case,
                    "ArceOS",
                    &group,
                ) {
                    Ok(cases) if !cases.is_empty() => Some(cases),
                    Ok(_) => None,
                    Err(err) if qemu_list_error_is_ignorable(err.kind()) => None,
                    Err(err) => return Err(anyhow::Error::new(err)),
                }
            }
        };
        if let Some(cases) = cases {
            groups.push((group, cases));
        }
    }
    if groups.is_empty()
        && let Some(case) = selected_case
    {
        bail!("unknown ArceOS qemu test case `{case}`");
    }
    Ok(groups)
}

fn qemu_list_error_is_ignorable(kind: qemu_test::ListQemuCasesErrorKind) -> bool {
    matches!(
        kind,
        qemu_test::ListQemuCasesErrorKind::EmptyGroup
            | qemu_test::ListQemuCasesErrorKind::UnknownSelectedCase
    )
}

fn rust_qemu_listed_cases(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<qemu_test::ListedQemuCase>> {
    let root = arceos_rust_test_dir(arceos);
    let archs = arceos_test_suit_qemu_archs(&root)?;
    if archs.is_empty() {
        bail!("no ArceOS rust qemu configs found under {}", root.display());
    }
    Ok(rust_qemu_features_for_list(selected_case, false)?
        .into_iter()
        .map(|feature| qemu_test::ListedQemuCase {
            name: feature.to_string(),
            archs: archs.clone(),
        })
        .collect())
}

fn rust_qemu_case_names(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<String>> {
    match target {
        Some((arch, target)) => {
            let root = arceos_rust_test_dir(arceos);
            arceos_test_suit_build_config_path(&root, target)?;
            arceos_test_suit_qemu_config_path(&root, arch)?;
            Ok(
                rust_qemu_features_for_list(selected_case, allow_missing_selected_case)?
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )
        }
        None => Ok(
            rust_qemu_features_for_list(selected_case, allow_missing_selected_case)?
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
    }
}

fn c_qemu_case_names(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    if let Some((arch, target)) = target {
        let root = arceos_c_test_dir(arceos);
        arceos_c_test_suit_build_config_path(&root, target)?;
        arceos_c_test_suit_qemu_config_path(&root, arch)?;
    }

    Ok(c_qemu_features_for_list(selected_case)?
        .into_iter()
        .map(str::to_string)
        .collect())
}

fn c_qemu_listed_cases(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> qemu_test::ListQemuCasesResult<Vec<qemu_test::ListedQemuCase>> {
    let root = arceos_c_test_dir(arceos);
    let archs = arceos_test_suit_qemu_archs(&root).map_err(qemu_test::ListQemuCasesError::from)?;
    if archs.is_empty() {
        return Ok(Vec::new());
    }
    let Ok(features) = c_qemu_features_for_list(selected_case) else {
        return Ok(Vec::new());
    };
    Ok(features
        .into_iter()
        .map(|feature| qemu_test::ListedQemuCase {
            name: feature.to_string(),
            archs: archs.clone(),
        })
        .collect())
}
