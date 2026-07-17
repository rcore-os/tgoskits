use std::{
    collections::BTreeSet,
    env,
    ffi::{OsStr, OsString},
    fs,
    path::Path,
    sync::{LazyLock, Mutex},
};

use ostool::run::qemu::QemuConfig;
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

fn fake_config() -> CaseAssetConfig {
    CaseAssetConfig {
        grouped_runner: GroupedCaseRunnerConfig {
            runner_name: "suite-run-case-tests".to_string(),
            runner_path: "/usr/bin/suite-run-case-tests".to_string(),
            autorun_profile_script: None,
            begin_marker: "SUITE_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "SUITE_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "SUITE_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "SUITE_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "SUITE_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^SUITE_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^SUITE_GROUPED_TEST_FAILED:".to_string(),
        },
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

fn fake_case(root: &Path, name: &str) -> TestQemuCase {
    let case_dir = root.join("test-suite/example/default").join(name);
    fs::create_dir_all(&case_dir).unwrap();
    TestQemuCase {
        name: name.to_string(),
        display_name: name.to_string(),
        case_dir: case_dir.clone(),
        qemu_config_path: case_dir.join("qemu-aarch64.toml"),
        test_commands: Vec::new(),
        host_symbolize_success_regex: Vec::new(),
        host_http_server: None,
        subcases: Vec::new(),
        grouped_subcase_filter: None,
    }
}

#[test]
fn resolve_target_dir_uses_workspace_target_directory() {
    let root = tempdir().unwrap();
    let dir = resolve_target_dir(root.path(), "x86_64-unknown-none").unwrap();

    assert_eq!(dir, root.path().join("target/x86_64-unknown-none"));
}

#[tokio::test]
async fn prepare_case_assets_plain_case_uses_shared_rootfs_with_snapshot() {
    let root = tempdir().unwrap();
    let target_dir = root.path().join("target/x86_64-unknown-none");
    let rootfs_dir = root.path().join("tmp/axbuild/rootfs");
    fs::create_dir_all(&target_dir).unwrap();
    fs::create_dir_all(&rootfs_dir).unwrap();
    let shared_img = rootfs_dir.join("rootfs-x86_64-alpine.img");
    fs::write(&shared_img, b"rootfs").unwrap();
    let case = fake_case(root.path(), "smoke");

    let assets = prepare_case_assets(
        root.path(),
        "x86_64",
        "x86_64-unknown-none",
        &case,
        shared_img.clone(),
        fake_config(),
    )
    .await
    .unwrap();

    // Plain case (no pipeline): rootfs_path must point to the shared image
    // directly -- no per-case copy is created.
    assert_eq!(assets.rootfs_path, shared_img);
    assert!(assets.rootfs_copy_to_remove.is_none());
    // -snapshot must always be present so QEMU guest writes never dirty the
    // shared image.
    assert!(assets.extra_qemu_args.contains(&"-snapshot".to_string()));
    // The shared image must be unmodified.
    assert_eq!(fs::read(&shared_img).unwrap(), b"rootfs");
}

#[test]
fn grouped_runner_script_runs_all_commands_and_reports_summary() {
    let root = tempdir().unwrap();
    let overlay = root.path().join("overlay");
    let commands = vec![
        "/usr/bin/alpha".to_string(),
        "/usr/bin/beta --flag".to_string(),
    ];

    let config = fake_config();
    write_grouped_case_runner_script(&overlay, &commands, &config.grouped_runner).unwrap();

    let runner = overlay.join("usr/bin/suite-run-case-tests");
    let content = fs::read_to_string(&runner).unwrap();
    assert!(content.contains("total=2"));
    assert!(content.contains("step=$((step + 1))"));
    assert!(content.contains("'SUITE_GROUPED_TEST_BEGIN'"));
    assert!(content.contains("'SUITE_GROUPED_TEST_PASSED'"));
    assert!(content.contains("'SUITE_GROUPED_TEST_FAILED'"));
    assert!(content.contains("'/usr/bin/alpha'"));
    assert!(content.contains("'/usr/bin/beta --flag'"));
    assert!(content.contains("SUITE_GROUPED_TESTS_PASSED"));
}

#[test]
fn grouped_runner_script_hashes_multiline_command_labels() {
    let root = tempdir().unwrap();
    let overlay = root.path().join("overlay");
    let commands = vec![
        "failed=0\nif [ \"$failed\" -ne 0 ]; then\n    echo \"SUITE_GROUPED_TEST_FAILED: \
         nested\"\nfi"
            .to_string(),
    ];

    let config = fake_config();
    write_grouped_case_runner_script(&overlay, &commands, &config.grouped_runner).unwrap();

    let runner = overlay.join("usr/bin/suite-run-case-tests");
    let content = fs::read_to_string(&runner).unwrap();
    assert!(content.contains("sh -c 'failed=0"));
    assert!(content.contains("'inline-command:"));
    assert!(content.contains("'SUITE_GROUPED_TEST_FAILED'"));
    assert!(!content.contains("command=failed=0"));
    assert!(!content.contains("command=echo"));
}

#[test]
fn grouped_runner_can_install_profile_autorun_without_interactive_guard() {
    let root = tempdir().unwrap();
    let overlay = root.path().join("overlay");
    let commands = vec!["/usr/bin/alpha".to_string()];
    let mut config = fake_config();
    config.grouped_runner.autorun_profile_script = Some("99-suite-run-case-tests.sh".into());

    write_grouped_case_runner_script(&overlay, &commands, &config.grouped_runner).unwrap();

    let profile = overlay.join("etc/profile.d/99-suite-run-case-tests.sh");
    let content = fs::read_to_string(&profile).unwrap();
    assert!(content.contains("AXBUILD_GROUPED_AUTORUN_DONE"));
    assert!(content.contains("/usr/bin/suite-run-case-tests"));
    assert!(!content.contains("case \"$-\" in"));
    assert!(!content.contains("set -u"));
}

#[test]
fn grouped_runner_profile_autorun_skips_shell_init() {
    let mut config = fake_config();
    config.grouped_runner.autorun_profile_script = Some("99-suite-run-case-tests.sh".into());
    let mut qemu = QemuConfig::default();
    let mut case = fake_case(tempdir().unwrap().path(), "grouped");
    case.test_commands = vec!["/usr/bin/alpha".to_string()];

    apply_grouped_qemu_config(&mut qemu, &case, &config.grouped_runner);

    assert!(qemu.shell_init_cmd.is_none());
}

#[test]
fn grouped_runner_shell_init_uses_short_exec_command_without_autorun() {
    let config = fake_config();
    let mut qemu = QemuConfig::default();
    let mut case = fake_case(tempdir().unwrap().path(), "grouped");
    case.test_commands = vec!["/usr/bin/alpha".to_string()];

    apply_grouped_qemu_config(&mut qemu, &case, &config.grouped_runner);

    let command = qemu.shell_init_cmd.as_deref().unwrap();
    assert_eq!(command, "exec /usr/bin/suite-run-case-tests");
    assert!(
        command.len() < 80,
        "Starry canonical TTY input buffer is 80 bytes"
    );
}

#[test]
fn required_success_match_prevents_guest_shutdown_from_passing() {
    let mut qemu = QemuConfig {
        success_regex: vec!["SUITE_PASSED".to_string()],
        ..Default::default()
    };

    require_qemu_success_match_before_guest_shutdown(&mut qemu);

    assert!(qemu.args.iter().any(|arg| arg == "-no-shutdown"));
    assert!(
        qemu.fail_regex
            .iter()
            .any(|pattern| pattern.contains("terminating on signal")),
        "an externally terminated QEMU process must not pass without the required marker"
    );
    let termination = regex::Regex::new(
        qemu.fail_regex
            .iter()
            .find(|pattern| pattern.contains("terminating on signal"))
            .unwrap(),
    )
    .unwrap();
    assert!(
        termination
            .is_match("qemu-system-x86_64: terminating on signal 15 from pid 271828 (timeout)\n")
    );
}

#[test]
fn qemu_without_required_success_keeps_normal_shutdown_semantics() {
    let mut qemu = QemuConfig::default();

    require_qemu_success_match_before_guest_shutdown(&mut qemu);

    assert!(!qemu.args.iter().any(|arg| arg == "-no-shutdown"));
    assert!(qemu.fail_regex.is_empty());
}

#[test]
fn required_success_guard_is_idempotent_with_existing_no_shutdown() {
    let mut qemu = QemuConfig {
        args: vec!["-no-shutdown".to_string()],
        success_regex: vec!["SUITE_PASSED".to_string()],
        ..Default::default()
    };

    require_qemu_success_match_before_guest_shutdown(&mut qemu);
    require_qemu_success_match_before_guest_shutdown(&mut qemu);

    assert_eq!(
        qemu.args
            .iter()
            .filter(|arg| arg.as_str() == "-no-shutdown")
            .count(),
        1
    );
    assert_eq!(
        qemu.fail_regex
            .iter()
            .filter(|pattern| pattern.contains("terminating on signal"))
            .count(),
        1
    );
}

#[test]
fn grouped_cache_key_tracks_runner_autorun_config() {
    let root = tempdir().unwrap();
    let shared_img = root.path().join("rootfs.img");
    fs::write(&shared_img, b"rootfs").unwrap();
    let case = fake_case(root.path(), "grouped");
    let mut config = fake_config();

    let without_autorun = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &case,
        &shared_img,
        &config,
    )
    .unwrap();

    config.grouped_runner.autorun_profile_script = Some("99-suite-run-case-tests.sh".into());
    let with_autorun = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &case,
        &shared_img,
        &config,
    )
    .unwrap();

    assert_ne!(without_autorun, with_autorun);
}

#[test]
fn grouped_cache_key_tracks_subcase_filter() {
    let root = tempdir().unwrap();
    let shared_img = root.path().join("rootfs.img");
    fs::write(&shared_img, b"rootfs").unwrap();
    let case = fake_case(root.path(), "grouped");
    let config = fake_config();

    let full_group = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &case,
        &shared_img,
        &config,
    )
    .unwrap();

    let mut filtered_case = case.clone();
    filtered_case.grouped_subcase_filter = Some(BTreeSet::from(["alpha".to_string()]));
    let single_subcase = case_asset_cache_key(
        "x86_64",
        "x86_64-unknown-none",
        CasePipeline::Grouped,
        &filtered_case,
        &shared_img,
        &config,
    )
    .unwrap();

    assert_ne!(full_group, single_subcase);
}

#[test]
fn save_rootfs_cache_image_is_noop_in_ci() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _ci = TempEnvVar::set("CI", "1");
    let _disable = TempEnvVar::unset("AXBUILD_DISABLE_ROOTFS_CACHE");

    let root = tempdir().unwrap();
    let src = root.path().join("src.img");
    let dst = root.path().join("cache/rootfs.img");
    fs::write(&src, vec![0_u8; 1024 * 1024]).unwrap();

    save_rootfs_cache_image(&src, &dst).unwrap();
    assert!(!dst.exists());
}

#[test]
fn save_rootfs_cache_image_writes_when_enabled() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _ci = TempEnvVar::unset("CI");
    let _disable = TempEnvVar::unset("AXBUILD_DISABLE_ROOTFS_CACHE");

    let root = tempdir().unwrap();
    let src = root.path().join("src.img");
    let dst = root.path().join("cache/rootfs.img");
    fs::write(&src, vec![1_u8; 1024 * 1024]).unwrap();

    save_rootfs_cache_image(&src, &dst).unwrap();
    assert!(dst.is_file());
    assert_eq!(fs::read(&dst).unwrap().len(), 1024 * 1024);
}
