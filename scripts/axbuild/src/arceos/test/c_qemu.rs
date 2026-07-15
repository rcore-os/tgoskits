use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use ostool::run::qemu::QemuConfig;

use super::{
    ARCEOS_C_ALL_FEATURE, ARCEOS_C_QEMU_FEATURES, ARCEOS_C_QEMU_LISTED_CASES,
    ARCEOS_C_TEST_BUILD_GROUP,
    assets::{arceos_c_test_dir, build_config_path, qemu_config_path},
    types::{CTestArtifactPaths, CTestDef},
};
use crate::{
    arceos::{ArceOS, build, cbuild, rootfs},
    context::{BuildCliArgs, SnapshotPersistence},
    test::{host_http::HostHttpServerGuard, qemu as qemu_test},
};

pub(super) async fn test_c_qemu(
    arceos: &mut ArceOS,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<()> {
    test_c_qemu_axbuild(arceos, target, selected_case).await
}

pub(super) fn c_qemu_features_for_run(
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<&'static str>> {
    match selected_case {
        Some(_) => c_qemu_features_for_list(selected_case),
        None => Ok(vec![ARCEOS_C_ALL_FEATURE]),
    }
}

pub(super) fn c_qemu_features_for_list(
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<&'static str>> {
    let Some(selected_case) = selected_case else {
        return Ok(ARCEOS_C_QEMU_LISTED_CASES.to_vec());
    };

    let features = ARCEOS_C_QEMU_FEATURES
        .iter()
        .copied()
        .filter(|feature| *feature == selected_case)
        .collect::<Vec<_>>();
    if features.is_empty() {
        bail!("unknown ArceOS c qemu test feature `{selected_case}`");
    }
    Ok(features)
}

pub(super) fn load_arceos_c_test_suit_qemu_case(
    root: &Path,
    arch: &str,
    target: &str,
    feature: &str,
) -> anyhow::Result<CTestDef> {
    Ok(CTestDef {
        name: feature.to_string(),
        build_group: ARCEOS_C_TEST_BUILD_GROUP.to_string(),
        build_config_path: arceos_c_test_suit_build_config_path(root, target)?,
        qemu_config_path: arceos_c_test_suit_qemu_config_path(root, arch)?,
    })
}

pub(super) fn arceos_c_test_suit_build_config_path(
    root: &Path,
    target: &str,
) -> anyhow::Result<PathBuf> {
    build_config_path(root, target, "ArceOS C test suite")
}

pub(super) fn arceos_c_test_suit_qemu_config_path(
    root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    qemu_config_path(root, arch, "ArceOS C test suite")
}

fn load_c_test_build_config(path: &Path) -> anyhow::Result<build::ArceosBuildConfig> {
    let config = build::load_arceos_build_config(path)
        .with_context(|| format!("failed to parse C build config {}", path.display()))?;
    if config.app_c.is_none() {
        bail!(
            "ArceOS C qemu test build config {} must set `app-c = \"c\"` or another C source \
             directory",
            path.display()
        );
    }
    Ok(config)
}

fn load_c_test_qemu_config(path: &Path) -> anyhow::Result<QemuConfig> {
    toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse C qemu config {}", path.display()))
}

fn c_test_artifact_index(test: &CTestDef) -> usize {
    let stem = test
        .qemu_config_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("qemu");
    match stem.strip_prefix("qemu-") {
        Some("x86_64") => 0,
        Some("aarch64") => 1,
        Some("riscv64") => 2,
        Some("loongarch64") => 3,
        Some(_) | None => 0,
    }
}

fn c_test_display_suffix(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("qemu-"))
        .map(|arch| format!(" ({arch})"))
        .unwrap_or_default()
}

async fn test_c_qemu_axbuild(
    arceos: &mut ArceOS,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<()> {
    let arch = crate::context::arch_for_target_checked(target)?;
    let c_test_root = arceos_c_test_dir(arceos);
    let c_tests = c_qemu_features_for_run(selected_case)?
        .into_iter()
        .map(|feature| load_arceos_c_test_suit_qemu_case(&c_test_root, arch, target, feature))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if c_tests.is_empty() {
        println!("no C tests found in {}", c_test_root.display());
        return Ok(());
    }

    println!(
        "running arceos C qemu tests for {} test(s) on target: {} (arch: {})",
        c_tests.len(),
        target,
        arch
    );

    let mut summary = qemu_test::QemuTestSummary::default();
    let total = c_tests.len();
    let suite_started = Instant::now();
    for (index, c_test) in c_tests.into_iter().enumerate() {
        println!("[{}/{}] arceos c qemu {}", index + 1, total, c_test.name);
        let case_started = Instant::now();
        let result = build_and_run_c_test(arceos, target, arch, &c_test)
            .await
            .with_context(|| {
                format!(
                    "c test `{}` failed{}",
                    c_test.name,
                    c_test_display_suffix(&c_test.qemu_config_path)
                )
            });
        let duration = case_started.elapsed();
        if let Err(err) = result {
            eprintln!("failed: c/{}: {err:#}", c_test.name);
            summary.fail_with_detail(format!("c/{}", c_test.name), format!("{duration:.2?}"));
        } else {
            println!("ok: c/{} ({duration:.2?})", c_test.name);
            summary.pass_with_detail(format!("c/{}", c_test.name), format!("{duration:.2?}"));
        }
    }

    let total_duration = format!("{:.2?}", suite_started.elapsed());
    summary.finish_with_total_detail("arceos c", "test", Some(total_duration.as_str()))
}

async fn build_and_run_c_test(
    arceos: &mut ArceOS,
    target: &str,
    _arch: &str,
    test: &CTestDef,
) -> anyhow::Result<()> {
    let workspace_root = arceos.app.workspace_root().to_path_buf();
    let build_config = load_c_test_build_config(&test.build_config_path)?;
    let qemu_config = load_c_test_qemu_config(&test.qemu_config_path)?;
    let mode = build::load_arceos_build_mode(&test.build_config_path)?;
    let build::ArceosBuildMode::AppC { app_dir, app_name } = mode else {
        bail!(
            "ArceOS C qemu test build config {} must set `app-c = \"c\"` or another C source \
             directory",
            test.build_config_path.display()
        );
    };
    let artifacts = c_test_artifact_paths(
        &workspace_root,
        &test.build_group,
        &test.name,
        c_test_artifact_index(test),
    );

    let request = arceos.prepare_request(
        BuildCliArgs {
            config: Some(test.build_config_path.clone()),
            package: Some("ax-libc".to_string()),
            arch: None,
            target: Some(target.to_string()),
            smp: None,
            debug: false,
        },
        None,
        None,
        SnapshotPersistence::Discard,
    )?;
    let cargo = build::load_c_app_cargo_config(&request)?;
    let input = c_test_build_input(
        app_dir,
        app_name,
        artifacts.target_dir,
        artifacts.out_dir,
        &test.name,
        build_config.build_info.features.clone(),
    );
    let output = cbuild::build_c_app(&workspace_root, &request, &input)?;
    let mut qemu = qemu_config;
    qemu_test::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
    rootfs::prepare_default_qemu_fat32_rootfs(arceos.app.workspace_root(), &qemu)?;
    let _host_http_server = qemu_test::load_qemu_case_host_http_server(&test.qemu_config_path)?
        .as_ref()
        .map(|config| HostHttpServerGuard::start(config, &test.name))
        .transpose()?;
    arceos
        .app
        .prepare_elf_artifact(output.elf_path, qemu.to_bin)
        .await?;
    arceos.app.run_prepared_qemu(qemu, None).await
}

fn c_test_build_input(
    app_dir: PathBuf,
    app_name: String,
    target_dir: PathBuf,
    out_dir: PathBuf,
    feature: &str,
    mut features: Vec<String>,
) -> cbuild::ArceosCBuildInput {
    features.push(format!("c-define:{}", c_test_feature_define(feature)));
    cbuild::ArceosCBuildInput {
        app_dir,
        app_name,
        target_dir,
        out_dir,
        features,
    }
}

fn c_test_feature_define(feature: &str) -> String {
    format!(
        "ARCEOS_C_TEST_CASE_{}",
        feature.replace('-', "_").to_ascii_uppercase()
    )
}

/// Returns isolated artifact paths for a single C test invocation.
///
/// Cases under the same build wrapper share a Cargo target dir and therefore
/// reuse the same ax-libc static library. QEMU output stays isolated per case
/// and per invocation so generated ELF files do not overwrite each other.
fn c_test_artifact_paths(
    workspace_root: &Path,
    build_group: &str,
    test_name: &str,
    invocation_index: usize,
) -> CTestArtifactPaths {
    let root = crate::context::axbuild_tmp_dir(workspace_root)
        .join("arceos-c")
        .join(test_name.replace('/', "-"));
    CTestArtifactPaths {
        target_dir: crate::context::axbuild_tmp_dir(workspace_root)
            .join("arceos-c")
            .join(build_group.replace('/', "-"))
            .join("cargo"),
        out_dir: root.join(format!("out-{invocation_index}")),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn arceos_c_default_run_selects_all_feature_only() {
        let features = c_qemu_features_for_run(None).unwrap();
        assert_eq!(features, vec![ARCEOS_C_ALL_FEATURE]);
    }

    #[test]
    fn arceos_c_selected_case_is_exact_feature_name() {
        let features = c_qemu_features_for_list(Some("pthread-basic")).unwrap();
        assert_eq!(features, vec!["pthread-basic"]);
    }

    #[test]
    fn arceos_c_default_list_hides_all_feature() {
        let features = c_qemu_features_for_list(None).unwrap();

        assert_eq!(features, ARCEOS_C_QEMU_LISTED_CASES);
        assert!(!features.contains(&ARCEOS_C_ALL_FEATURE));
    }

    #[test]
    fn arceos_c_feature_define_names_are_stable() {
        assert_eq!(
            c_test_feature_define("pthread-basic"),
            "ARCEOS_C_TEST_CASE_PTHREAD_BASIC"
        );
        assert_eq!(
            c_test_feature_define(ARCEOS_C_ALL_FEATURE),
            "ARCEOS_C_TEST_CASE_ALL"
        );
    }

    #[test]
    fn arceos_c_build_input_adds_selected_feature_define() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("c");
        let input = c_test_build_input(
            app_dir.clone(),
            ARCEOS_C_TEST_BUILD_GROUP.to_string(),
            PathBuf::from("/tmp/target"),
            PathBuf::from("/tmp/out"),
            "pthread-basic",
            vec!["alloc".to_string()],
        );

        assert_eq!(input.app_name, ARCEOS_C_TEST_BUILD_GROUP);
        assert_eq!(input.app_dir, app_dir);
        assert!(
            input
                .features
                .iter()
                .any(|feature| feature == "c-define:ARCEOS_C_TEST_CASE_PTHREAD_BASIC")
        );
    }

    #[test]
    fn arceos_c_qemu_case_uses_single_test_suite_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("c")).unwrap();
        fs::write(root.join("c/main.c"), "int main(void) { return 0; }\n").unwrap();
        fs::write(
            root.join("build-x86_64-unknown-none.toml"),
            "features = []\n",
        )
        .unwrap();
        fs::write(
            root.join("qemu-x86_64.toml"),
            "args = [\"-nographic\"]\nuefi = false\nto_bin = false\nsuccess_regex = \
             [\"PASS\"]\nfail_regex = [\"panic\"]\n",
        )
        .unwrap();

        let case = load_arceos_c_test_suit_qemu_case(root, "x86_64", "x86_64-unknown-none", "mem")
            .unwrap();

        assert_eq!(case.name, "mem");
        assert_eq!(case.build_group, ARCEOS_C_TEST_BUILD_GROUP);
        assert!(
            case.build_config_path
                .ends_with("build-x86_64-unknown-none.toml")
        );
        assert!(case.qemu_config_path.ends_with("qemu-x86_64.toml"));
    }

    #[test]
    fn load_c_test_build_config_reads_build_info() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("build-x86_64-unknown-none.toml");
        fs::write(
            &path,
            "app-c = \"c\"\nfeatures = [\"alloc\", \"paging\"]\nlog = \"Trace\"\nmax_cpu_num = \
             4\n\n[env]\n",
        )
        .unwrap();

        let config = load_c_test_build_config(&path).unwrap();
        assert_eq!(config.app_c, Some(PathBuf::from("c")));
        assert_eq!(config.build_info.features, vec!["alloc", "paging"]);
        assert_eq!(config.build_info.log, build::LogLevel::Trace);
        assert_eq!(config.build_info.max_cpu_num, Some(4));
    }

    #[test]
    fn load_c_test_build_config_rejects_missing_app_c() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("build-x86_64-unknown-none.toml");
        fs::write(&path, "features = [\"alloc\"]\nlog = \"Info\"\n\n[env]\n").unwrap();

        let err = load_c_test_build_config(&path).unwrap_err();
        assert!(err.to_string().contains("must set `app-c = \"c\"`"));
    }

    #[test]
    fn load_c_test_qemu_config_reads_standard_qemu_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("qemu-x86_64.toml");
        fs::write(
            &path,
            "args = [\"-nographic\"]\nuefi = false\nto_bin = false\nsuccess_regex = \
             [\"PASS\"]\nfail_regex = [\"panic\"]\ntimeout = 120\n",
        )
        .unwrap();

        let config = load_c_test_qemu_config(&path).unwrap();
        assert_eq!(config.args, vec!["-nographic"]);
        assert_eq!(config.success_regex, vec!["PASS"]);
        assert_eq!(config.fail_regex, vec!["panic"]);
        assert_eq!(config.timeout, Some(120));
    }
}
