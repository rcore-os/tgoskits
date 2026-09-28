use std::{
    collections::BTreeSet,
    env,
    ffi::{OsStr, OsString},
    fs,
    path::Path,
    sync::{LazyLock, Mutex},
};

use tempfile::tempdir;

use super::*;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct TempEnvVar {
    key: &'static str,
    original: Option<OsString>,
}

impl TempEnvVar {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    fn unset(key: &'static str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for TempEnvVar {
    fn drop(&mut self) {
        match self.original.as_ref() {
            Some(value) => unsafe {
                env::set_var(self.key, value);
            },
            None => unsafe {
                env::remove_var(self.key);
            },
        }
    }
}

pub(super) fn fake_config() -> CaseAssetConfig {
    CaseAssetConfig {
        grouped_execution: GroupedCaseExecution::GuestInit(Box::new(GroupedCaseRunnerConfig {
            runner_name: "suite-run-case-tests".to_string(),
            runner_path: "/usr/bin/suite-run-case-tests".to_string(),
            begin_marker: "SUITE_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "SUITE_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "SUITE_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "SUITE_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "SUITE_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^SUITE_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^SUITE_GROUPED_TEST_FAILED:".to_string(),
        })),
        script_env: CaseScriptEnvConfig {
            staging_root: "SUITE_STAGING_ROOT".to_string(),
            case_dir: "SUITE_CASE_DIR".to_string(),
            case_c_dir: "SUITE_CASE_C_DIR".to_string(),
            case_work_dir: "SUITE_CASE_WORK_DIR".to_string(),
            case_build_dir: "SUITE_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "SUITE_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: Vec::new(),
        prepare_staging_root: |_| Ok(()),
        prepare_guest_package_env: None,
    }
}

pub(super) fn fake_case(root: &Path, name: &str) -> TestQemuCase {
    let case_dir = root.join("test-suite/example/default").join(name);
    fs::create_dir_all(&case_dir).unwrap();
    TestQemuCase {
        name: name.to_string(),
        display_name: name.to_string(),
        case_dir: case_dir.clone(),
        qemu_config_path: case_dir.join("qemu-aarch64.toml"),
        test_commands: Vec::new(),
        grouped_command_selection: Default::default(),
        host_symbolize_success_regex: Vec::new(),
        host_http_server: None,
        subcases: Vec::new(),
        grouped_subcase_filter: None,
        ltp_case_id: None,
    }
}

#[test]
fn grouped_cache_key_tracks_effective_execution_inputs() {
    let root = tempdir().unwrap();
    let shared_img = root.path().join("rootfs.img");
    fs::write(&shared_img, b"rootfs").unwrap();
    let case = fake_case(root.path(), "grouped");
    let mut config = fake_config();

    let guest_init = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &case,
        &shared_img,
        &config,
    )
    .unwrap();

    config.grouped_execution = GroupedCaseExecution::External;
    let external = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &case,
        &shared_img,
        &config,
    )
    .unwrap();

    assert_ne!(guest_init, external);

    let mut filtered_case = case.clone();
    filtered_case.grouped_subcase_filter = Some(BTreeSet::from(["alpha".to_string()]));
    config.grouped_execution = fake_config().grouped_execution;
    let single_subcase = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &filtered_case,
        &shared_img,
        &config,
    )
    .unwrap();

    assert_ne!(guest_init, single_subcase);

    let mut ltp = filtered_case.clone();
    ltp.ltp_case_id = Some("execve03".to_string());
    let execve_key = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &ltp,
        &shared_img,
        &config,
    )
    .unwrap();
    ltp.ltp_case_id = Some("futex_wait01".to_string());
    let futex_key = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &ltp,
        &shared_img,
        &config,
    )
    .unwrap();
    assert_ne!(execve_key, futex_key);
    assert_ne!(execve_key, single_subcase);
}

#[test]
fn save_rootfs_cache_image_respects_ci_isolation() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _ci = TempEnvVar::set("CI", "1");
    let _disable = TempEnvVar::unset("AXBUILD_DISABLE_ROOTFS_CACHE");

    let root = tempdir().unwrap();
    let src = root.path().join("src.img");
    let dst = root.path().join("cache/rootfs.img");
    fs::write(&src, vec![0_u8; 1024 * 1024]).unwrap();

    save_rootfs_cache_image(&src, &dst).unwrap();
    assert!(!dst.exists());

    let _ci_off = TempEnvVar::unset("CI");
    save_rootfs_cache_image(&src, &dst).unwrap();
    assert!(dst.is_file());
}
