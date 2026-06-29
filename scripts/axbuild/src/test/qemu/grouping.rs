use super::*;

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
    group_cases_by_build_config(cases)
        .into_iter()
        .map(|group| {
            let (request, cargo) = prepare_context(group.build_config_path)?;
            Ok(QemuCaseBuildGroup {
                group,
                request,
                cargo,
            })
        })
        .collect()
}
