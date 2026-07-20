use crate::test::case as test_case;

pub(super) fn axvisor_case_asset_config() -> test_case::CaseAssetConfig {
    test_case::CaseAssetConfig {
        grouped_runner: test_case::GroupedCaseRunnerConfig {
            runner_name: "axvisor-run-case-tests".to_string(),
            runner_path: "/usr/bin/axvisor-run-case-tests".to_string(),
            autorun_profile_script: None,
            begin_marker: "AXVISOR_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "AXVISOR_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "AXVISOR_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "AXVISOR_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "AXVISOR_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^AXVISOR_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^AXVISOR_GROUPED_TEST_FAILED:".to_string(),
        },
        script_env: test_case::CaseScriptEnvConfig {
            staging_root: "AXVISOR_TEST_STAGING_ROOT".to_string(),
            case_dir: "AXVISOR_TEST_CASE_DIR".to_string(),
            case_c_dir: "AXVISOR_TEST_CASE_C_DIR".to_string(),
            case_work_dir: "AXVISOR_TEST_CASE_WORK_DIR".to_string(),
            case_build_dir: "AXVISOR_TEST_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "AXVISOR_TEST_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: Vec::new(),
        prepare_staging_root: |_| Ok(()),
        prepare_guest_package_env: None,
    }
}
