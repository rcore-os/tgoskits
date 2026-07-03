use std::time::Instant;

use anyhow::{Context, bail};

use super::{
    ARCEOS_AXTEST_GROUP, ARCEOS_TEST_SUITE_OS,
    args::{ArgsTestQemu, reject_removed_rust_package_filter},
    axtest_qemu::test_axtest_qemu,
    c_qemu::test_c_qemu,
    discovery::selected_qemu_test_groups,
    generic_qemu::test_generic_qemu,
    listing::{
        all_qemu_case_groups, list_c_qemu_cases, list_generic_qemu_cases, list_rust_qemu_cases,
    },
    rust_qemu::{run_rust_qemu_case, test_rust_qemu},
    types::{
        ArceosQemuBuildGroup, GenericQemuRunOptions, PreparedArceosRustQemuCase, QemuTestFlow,
    },
};
use crate::{arceos::ArceOS, test::qemu as qemu_test};

pub(super) async fn test_qemu(arceos: &mut ArceOS, args: ArgsTestQemu) -> anyhow::Result<()> {
    reject_removed_rust_package_filter(&args)?;

    if args.list && args.arch.is_none() && args.target.is_none() && args.test_group.is_none() {
        let groups = all_qemu_case_groups(arceos, args.test_case.as_deref())?;
        if groups.is_empty() {
            bail!(
                "no ArceOS qemu test cases found under {}",
                arceos
                    .app
                    .workspace_root()
                    .join("test-suit")
                    .join(ARCEOS_TEST_SUITE_OS)
                    .display()
            );
        }
        println!("{}", qemu_test::render_qemu_case_forest("arceos", groups));
        return Ok(());
    }

    if args.list && args.arch.is_none() && args.target.is_none() {
        let groups = selected_qemu_test_groups(arceos.app.workspace_root(), &args)?;
        let allow_rust_case_miss = args.test_group.is_none() && !args.only_rust;
        let mut trees = Vec::new();
        for group in groups {
            match group {
                QemuTestFlow::Rust => trees.extend(list_rust_qemu_cases(
                    arceos,
                    None,
                    args.test_case.as_deref(),
                    allow_rust_case_miss,
                )?),
                QemuTestFlow::C => {
                    trees.extend(list_c_qemu_cases(arceos, None, args.test_case.as_deref())?)
                }
                QemuTestFlow::Axtest => trees.extend(list_generic_qemu_cases(
                    arceos,
                    None,
                    ARCEOS_AXTEST_GROUP,
                    args.test_case.as_deref(),
                )?),
                QemuTestFlow::Generic(ref group) => trees.extend(list_generic_qemu_cases(
                    arceos,
                    None,
                    group,
                    args.test_case.as_deref(),
                )?),
            }
        }
        if trees.is_empty() {
            bail!("no ArceOS qemu test cases found");
        }
        println!("{}", trees.join("\n"));
        return Ok(());
    }

    let selected_case = args.test_case.as_deref();
    let (arch, target) = qemu_test::parse_test_target(
        &args.arch,
        &args.target,
        "arceos qemu tests",
        &crate::context::supported_arches(),
        &crate::context::supported_targets(),
        crate::context::resolve_arceos_arch_and_target,
    )?;
    let groups = selected_qemu_test_groups(arceos.app.workspace_root(), &args)?;
    let allow_rust_case_miss = args.test_group.is_none() && !args.only_rust;
    if args.list {
        let mut trees = Vec::new();
        for group in groups {
            match group {
                QemuTestFlow::Rust => trees.extend(list_rust_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    selected_case,
                    allow_rust_case_miss,
                )?),
                QemuTestFlow::C => trees.extend(list_c_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    args.test_case.as_deref(),
                )?),
                QemuTestFlow::Axtest => trees.extend(list_generic_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    ARCEOS_AXTEST_GROUP,
                    selected_case,
                )?),
                QemuTestFlow::Generic(ref group) => trees.extend(list_generic_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    group,
                    selected_case,
                )?),
            }
        }
        if trees.is_empty() {
            bail!("no ArceOS qemu test cases found");
        }
        println!("{}", trees.join("\n"));
        return Ok(());
    }

    let symbolize_after = !args.no_symbolize;
    let keep_qemu_log = args.keep_qemu_log || crate::backtrace::keep_qemu_log_from_env();
    for flow in groups {
        match flow {
            QemuTestFlow::Rust => {
                test_rust_qemu(
                    arceos,
                    &arch,
                    &target,
                    selected_case,
                    allow_rust_case_miss,
                    symbolize_after,
                    keep_qemu_log,
                )
                .await?
            }
            QemuTestFlow::C => test_c_qemu(arceos, &target, args.test_case.as_deref()).await?,
            QemuTestFlow::Axtest => {
                test_axtest_qemu(
                    arceos,
                    &arch,
                    &target,
                    GenericQemuRunOptions {
                        selected_case,
                        symbolize_after,
                        keep_qemu_log,
                        allow_empty: args.test_group.is_none(),
                    },
                )
                .await?
            }
            QemuTestFlow::Generic(ref group) => {
                test_generic_qemu(
                    arceos,
                    &arch,
                    &target,
                    group,
                    GenericQemuRunOptions {
                        selected_case,
                        symbolize_after,
                        keep_qemu_log,
                        allow_empty: args.test_group.is_none(),
                    },
                )
                .await?
            }
        }
    }
    Ok(())
}

pub(super) async fn run_prepared_qemu_groups(
    arceos: &mut ArceOS,
    build_subject: &str,
    group_label: &str,
    prepared: &[PreparedArceosRustQemuCase],
    symbolize_after: bool,
    keep_qemu_log: bool,
) -> anyhow::Result<()> {
    let total = prepared.len();
    let build_groups = group_arceos_qemu_cases_by_build_identity(prepared);
    let suite_started = Instant::now();
    let mut summary = qemu_test::QemuTestSummary::default();
    let mut completed = 0;
    for build_group in &build_groups {
        arceos
            .app
            .build(
                build_group.cargo.clone(),
                build_group.request.build_info_path.clone(),
            )
            .await
            .with_context(|| {
                let feature = build_group
                    .feature
                    .map(|feature| format!(" with feature `{feature}`"))
                    .unwrap_or_default();
                format!(
                    "failed to build ArceOS {build_subject} qemu test artifact for package \
                     `{}`{feature} in build group `{}` ({})",
                    build_group.package,
                    build_group.build_group,
                    build_group.build_config_path.display()
                )
            })?;

        for case in &build_group.cases {
            completed += 1;
            let case_name = &case.case.case.name;
            println!("[{completed}/{total}] {group_label} qemu {case_name}");
            let case_started = Instant::now();
            let result = run_rust_qemu_case(arceos, case, symbolize_after, keep_qemu_log)
                .await
                .with_context(|| format!("{group_label} qemu test failed for case `{case_name}`"));
            let duration = case_started.elapsed();
            match result {
                Ok(()) => {
                    println!("ok: {case_name} ({duration:.2?})");
                    summary.pass_with_detail(case_name, format!("{duration:.2?}"));
                }
                Err(err) => {
                    eprintln!("failed: {case_name}: {err:#}");
                    summary.fail_with_detail(case_name, format!("{duration:.2?}"));
                }
            }
        }
    }
    let total_duration = format!("{:.2?}", suite_started.elapsed());
    summary.finish_with_total_detail(group_label, "case", Some(total_duration.as_str()))
}

pub(super) fn group_arceos_qemu_cases_by_build_identity(
    cases: &[PreparedArceosRustQemuCase],
) -> Vec<ArceosQemuBuildGroup<'_>> {
    let mut groups = Vec::<ArceosQemuBuildGroup<'_>>::new();
    for case in cases {
        if let Some(group) = groups.iter_mut().find(|group| {
            group.build_config_path == case.case.build_config_path
                && group.package == case.case.package
                && group.feature == case.case.feature.as_deref()
        }) {
            group.cases.push(case);
            continue;
        }

        groups.push(ArceosQemuBuildGroup {
            build_group: &case.case.build_group,
            build_config_path: &case.case.build_config_path,
            package: &case.case.package,
            feature: case.case.feature.as_deref(),
            request: case.request.clone(),
            cargo: case.cargo.clone(),
            cases: vec![case],
        });
    }
    groups
}

#[cfg(test)]
pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    qemu_test::parse_test_target(
        arch,
        target,
        "arceos qemu tests",
        &crate::context::supported_arches(),
        &crate::context::supported_targets(),
        crate::context::resolve_arceos_arch_and_target,
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use ostool::{build::config::Cargo, run::qemu::QemuConfig};

    use super::*;
    use crate::{
        arceos::test::{ARCEOS_RUST_TEST_PACKAGE, types::ArceosRustQemuCase},
        context::ResolvedBuildRequest,
        test::case::TestQemuCase,
    };

    #[test]
    fn accepts_supported_targets() {
        assert_eq!(
            parse_target(&None, &Some("x86_64-unknown-none".to_string())).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&None, &Some("aarch64-unknown-none-softfloat".to_string())).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn accepts_supported_arch_aliases() {
        assert_eq!(
            parse_target(&Some("x86_64".to_string()), &None).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&Some("aarch64".to_string()), &None).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn rejects_unsupported_targets() {
        let rejected_target = "mips64-unknown-none".to_string();
        let err = parse_target(&None, &Some(rejected_target.clone())).unwrap_err();
        assert!(err.to_string().contains(&rejected_target));
    }

    #[test]
    fn arceos_qemu_build_identity_includes_feature() {
        let build_config = PathBuf::from("/tmp/arceos/build-x86_64-unknown-none.toml");
        let cases = vec![
            prepared_arceos_qemu_case("one", "feature-one", &build_config),
            prepared_arceos_qemu_case("two", "feature-two", &build_config),
            prepared_arceos_qemu_case("one/again", "feature-one", &build_config),
        ];

        let groups = group_arceos_qemu_cases_by_build_identity(&cases);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].package, ARCEOS_RUST_TEST_PACKAGE);
        assert_eq!(groups[0].feature, Some("feature-one"));
        assert_eq!(groups[0].cases.len(), 2);
        assert_eq!(groups[1].package, ARCEOS_RUST_TEST_PACKAGE);
        assert_eq!(groups[1].feature, Some("feature-two"));
        assert_eq!(groups[1].cases.len(), 1);
    }

    fn prepared_arceos_qemu_case(
        name: &str,
        feature: &str,
        build_config_path: &Path,
    ) -> PreparedArceosRustQemuCase {
        PreparedArceosRustQemuCase {
            case: ArceosRustQemuCase {
                case: TestQemuCase {
                    name: name.to_string(),
                    display_name: name.to_string(),
                    case_dir: PathBuf::from(format!("/tmp/{name}")),
                    qemu_config_path: PathBuf::from(format!("/tmp/{name}/qemu-x86_64.toml")),
                    test_commands: Vec::new(),
                    host_symbolize_success_regex: Vec::new(),
                    host_http_server: None,
                    subcases: Vec::new(),
                    grouped_subcase_filter: None,
                },
                build_group: "std".to_string(),
                build_config_path: build_config_path.to_path_buf(),
                package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
                feature: Some(feature.to_string()),
            },
            request: ResolvedBuildRequest {
                package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
                arch: "x86_64".to_string(),
                target: "x86_64-unknown-none".to_string(),
                smp: None,
                debug: false,
                build_info_path: build_config_path.to_path_buf(),
                qemu_config: None,
                uboot_config: None,
            },
            cargo: Cargo {
                env: Default::default(),
                target: "x86_64-unknown-none".to_string(),
                package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
                features: vec![feature.to_string()],
                log: None,
                extra_config: None,
                profile: None,
                disable_someboot_build_config: true,
                args: Vec::new(),
                pre_build_cmds: Vec::new(),
                post_build_cmds: Vec::new(),
                to_bin: false,
                bin: None,
                test: None,
            },
            qemu: QemuConfig::default(),
            host_symbolize_success_regex: Vec::new(),
        }
    }
}
