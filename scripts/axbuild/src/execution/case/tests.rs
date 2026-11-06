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

#[test]
fn failed_case_preparation_removes_only_its_run_directory() {
    let root = tempdir().unwrap();
    let target_dir = root.path().join("target");
    let shared = root.path().join("shared.img");
    fs::write(&shared, b"shared rootfs").unwrap();
    let case = fake_case(root.path(), "compiled");
    fs::create_dir(case.case_dir.join("c")).unwrap();
    let layout = case_asset_layout(&target_dir, "test-target", &case.display_name).unwrap();
    fs::create_dir_all(&layout.cache_dir).unwrap();
    let cache_marker = layout.cache_dir.join("retained");
    fs::write(&cache_marker, b"shared cache").unwrap();
    let other_run = layout.run_dir.parent().unwrap().join("other-run");
    fs::create_dir_all(&other_run).unwrap();
    fs::write(other_run.join("retained"), b"another run").unwrap();

    // The real preparation copies the image before rejecting the missing
    // CMake entry. Its failure must reclaim that image and all partial work.
    let error = prepare_case_assets_sync(
        &target_dir,
        "x86_64",
        "test-target",
        &case,
        &shared,
        &fake_config(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("missing case CMake project entry")
    );
    let runs = fs::read_dir(other_run.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(runs, vec![other_run.clone()]);
    assert_eq!(
        fs::read(other_run.join("retained")).unwrap(),
        b"another run"
    );
    assert_eq!(fs::read(cache_marker).unwrap(), b"shared cache");
    assert_eq!(fs::read(shared).unwrap(), b"shared rootfs");
}

#[tokio::test]
async fn prepared_case_assets_reclaim_after_successful_or_failed_setup() {
    use crate::rootfs::qemu::{
        RootfsPatchMode, RootfsPatchOptions, RootfsWritePolicy, patch_rootfs,
    };

    let root = tempdir().unwrap();
    let target_dir = root.path().join("target");
    let shared = root.path().join("shared.img");
    fs::write(&shared, b"shared rootfs").unwrap();
    let case = fake_case(root.path(), "scripted");
    fs::create_dir(case.case_dir.join("sh")).unwrap();
    let config = fake_config();
    let layout = case_asset_layout(&target_dir, "test-target", &case.display_name).unwrap();
    let cache_image = cache::rootfs_cache_image_path(
        &layout,
        "x86_64",
        "test-target",
        CasePipeline::Sh,
        &case,
        &shared,
        &config,
    )
    .unwrap();
    fs::create_dir_all(cache_image.parent().unwrap()).unwrap();
    fs::write(&cache_image, vec![0x5a; 1024 * 1024]).unwrap();

    for invalid_qemu in [false, true] {
        let mut run_dir = None;
        let result: anyhow::Result<()> = async {
            let assets = prepare_case_assets(
                &target_dir,
                "x86_64",
                "test-target",
                &case,
                shared.clone(),
                config.clone(),
            )
            .await?;
            assert!(assets.cache_hit);
            assert_eq!(
                assets.disposable_rootfs_path(),
                Some(assets.rootfs_path.as_path())
            );
            assert_eq!(fs::metadata(&assets.rootfs_path)?.len(), 1024 * 1024);
            run_dir = assets.rootfs_path.parent().map(Path::to_path_buf);
            let mut qemu = ostool::run::qemu::QemuConfig::default();
            if invalid_qemu {
                qemu.args = vec!["-blockdev".into(), "driver=file,filename=rootfs.img".into()];
            }
            patch_rootfs(
                &mut qemu,
                &assets.rootfs_path,
                RootfsPatchOptions {
                    mode: RootfsPatchMode::EnsureDiskBootNet,
                    write_policy: RootfsWritePolicy::Discard,
                },
            )?;
            Ok(())
        }
        .await;
        if invalid_qemu {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("QEMU host rootfs -blockdev cannot be patched")
            );
        } else {
            result.unwrap();
        }
        assert!(!run_dir.unwrap().exists());
        assert_eq!(fs::metadata(&cache_image).unwrap().len(), 1024 * 1024);
        assert_eq!(fs::read(&shared).unwrap(), b"shared rootfs");
    }
}
