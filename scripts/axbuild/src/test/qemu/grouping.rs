use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

use super::*;

/// Preserve an executable before another build can reuse Cargo's artifact path.
///
/// QEMU and board runners both build several Cargo configurations in one
/// invocation. Keeping each ELF under its build-group directory lets the
/// runtime phase select the correct executable without rebuilding it.
pub(crate) fn preserve_build_artifact(
    source: &Path,
    artifact_directory: &Path,
    build_group_index: usize,
) -> anyhow::Result<PathBuf> {
    let file_name = source
        .file_name()
        .with_context(|| format!("build artifact {} has no file name", source.display()))?;
    let group_directory = artifact_directory.join(format!("group-{build_group_index}"));
    fs::create_dir_all(&group_directory).with_context(|| {
        format!(
            "failed to create build-group artifact directory {}",
            group_directory.display()
        )
    })?;
    let destination = group_directory.join(file_name);
    fs::copy(source, &destination).with_context(|| {
        format!(
            "failed to preserve build artifact {} at {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

pub(crate) fn group_cases_by_build_config<T: BuildConfigRef>(
    cases: &[T],
) -> Vec<QemuCaseGroup<'_, T>> {
    let mut groups: Vec<QemuCaseGroup<'_, T>> = Vec::new();
    let mut indexes = BTreeMap::<&Path, usize>::new();
    for case in cases {
        if let Some(index) = indexes.get(case.build_config_path()).copied() {
            groups[index].cases.push(case);
            continue;
        }

        let index = groups.len();
        indexes.insert(case.build_config_path(), index);
        groups.push(QemuCaseGroup {
            build_group: case.build_group(),
            build_config_path: case.build_config_path(),
            cases: vec![case],
        });
    }

    groups
}

pub(crate) fn prepare_case_build_groups<T, R>(
    cases: &[T],
    mut prepare_context: impl FnMut(&Path) -> anyhow::Result<(R, Cargo)>,
) -> anyhow::Result<Vec<QemuCaseBuildGroup<'_, T, R>>>
where
    T: BuildConfigRef,
{
    let mut prepared: Vec<QemuCaseBuildGroup<'_, T, R>> = Vec::new();
    for group in group_cases_by_build_config(cases) {
        let (request, cargo) = prepare_context(group.build_config_path)?;
        if let Some(existing) = prepared.iter_mut().find(|existing| existing.cargo == cargo) {
            // Build TOMLs may differ only in runtime inputs such as AxVisor
            // vm_configs. Cargo identity is the compilation boundary, so
            // reuse the prepared executable while each case keeps its own
            // runtime configuration.
            existing.group.cases.extend(group.cases);
        } else {
            prepared.push(QemuCaseBuildGroup {
                group,
                request,
                cargo,
            });
        }
    }
    Ok(prepared)
}
