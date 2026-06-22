pub(crate) fn starry_case_asset_config() -> case::CaseAssetConfig {
    case::CaseAssetConfig {
        grouped_runner: case::GroupedCaseRunnerConfig {
            runner_name: "starry-run-case-tests".to_string(),
            runner_path: "/usr/bin/starry-run-case-tests".to_string(),
            autorun_profile_script: Some("99-starry-run-case-tests.sh".to_string()),
            begin_marker: "STARRY_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "STARRY_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "STARRY_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "STARRY_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "STARRY_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^STARRY_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^STARRY_GROUPED_TEST_FAILED:".to_string(),
        },
        script_env: case::CaseScriptEnvConfig {
            staging_root: "STARRY_STAGING_ROOT".to_string(),
            case_dir: "STARRY_CASE_DIR".to_string(),
            case_c_dir: "STARRY_CASE_C_DIR".to_string(),
            case_work_dir: "STARRY_CASE_WORK_DIR".to_string(),
            case_build_dir: "STARRY_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "STARRY_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: vec![crate::starry::apk::STARRY_APK_REGION_VAR.to_string()],
        prepare_staging_root: crate::starry::resolver::write_host_resolver_config,
        prepare_guest_package_env: Some(starry_guest_package_env),
    }
}

fn starry_guest_package_env(staging_root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let region = crate::starry::apk::apk_region_from_env()?;
    crate::starry::apk::rewrite_apk_repositories_for_region(staging_root, region)?;
    log_starry_apk_prebuild_context(staging_root, region)?;
    Ok(vec![(
        crate::starry::apk::STARRY_APK_REGION_VAR.to_string(),
        region.canonical_name().to_string(),
    )])
}

fn log_starry_apk_prebuild_context(
    staging_root: &Path,
    region: crate::starry::apk::ApkRegion,
) -> anyhow::Result<()> {
    let repositories_path = staging_root.join("etc/apk/repositories");
    let repositories = fs::read_to_string(&repositories_path)
        .with_context(|| format!("failed to read {}", repositories_path.display()))?;

    println!("STARRY_APK_REGION={}", region.canonical_name());
    println!("apk repositories:");
    print!("{repositories}");
    if !repositories.ends_with('\n') {
        println!();
    }

    Ok(())
}
use std::{fs, path::Path};

use anyhow::Context;

use crate::test::case;
