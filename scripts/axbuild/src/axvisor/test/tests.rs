use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::tempdir;

use super::*;
use crate::{axvisor::build, context::ResolvedAxvisorRequest};

const X86_LINUX_DIRECT_BOOT_CMDLINE_LIMIT: usize = 231;

#[derive(serde::Deserialize)]
struct TestBuildConfigVmConfigs {
    #[serde(default)]
    vm_configs: Vec<PathBuf>,
}

#[derive(serde::Deserialize)]
struct TestVmKernelConfig {
    kernel: TestVmKernel,
}

#[derive(serde::Deserialize)]
struct TestVmKernel {
    cmdline: String,
}

fn write_qemu_config(root: &Path, case: &str, arch: &str, body: &str) -> PathBuf {
    write_qemu_config_in_group(root, "normal", "default", case, arch, body)
}

fn write_qemu_config_in_group(
    root: &Path,
    group: &str,
    build_group: &str,
    case: &str,
    arch: &str,
    body: &str,
) -> PathBuf {
    let dir = root
        .join("test-suit/axvisor")
        .join(group)
        .join(build_group)
        .join(case);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("qemu-{arch}.toml"));
    fs::write(&path, body).unwrap();
    path
}

fn write_qemu_build_config(root: &Path, group: &str, build_group: &str, target: &str) -> PathBuf {
    let dir = root.join("test-suit/axvisor").join(group).join(build_group);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("build-{target}.toml"));
    fs::write(
        &path,
        format!("target = \"{target}\"\nfeatures = []\nlog = \"Info\"\nvm_configs = []\n"),
    )
    .unwrap();
    path
}

fn write_board_build_config(root: &Path, build_group: &str) -> PathBuf {
    write_qemu_build_config(
        root,
        "normal",
        build_group,
        "aarch64-unknown-none-softfloat",
    )
}

fn write_board_config(root: &Path, case: &str, name: &str, body: &str) -> PathBuf {
    write_board_config_in_group(root, "normal", "default", case, name, body)
}

fn write_board_config_in_group(
    root: &Path,
    group: &str,
    build_group: &str,
    case: &str,
    name: &str,
    body: &str,
) -> PathBuf {
    let dir = root
        .join("test-suit/axvisor")
        .join(group)
        .join(build_group)
        .join(case);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("board-{name}.toml"));
    fs::write(&path, body).unwrap();
    path
}

fn axvisor_request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
    ResolvedAxvisorRequest {
        package: build::AXVISOR_PACKAGE.to_string(),
        axvisor_dir: PathBuf::from("/tmp/os/axvisor"),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        build_info_path: path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: Vec::new(),
    }
}

#[test]
fn checked_in_test_build_vmconfigs_exist() {
    let workspace_root = std::env::current_dir().unwrap();
    let axvisor_suite = workspace_root.join("test-suit/axvisor");
    if !axvisor_suite.is_dir() {
        return;
    }

    let mut stack = vec![axvisor_suite];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_name.starts_with("build-")
                || path.extension().and_then(|ext| ext.to_str()) != Some("toml")
            {
                continue;
            }

            let content = fs::read_to_string(&path).unwrap();
            let config: TestBuildConfigVmConfigs = toml::from_str(&content).unwrap();
            for vm_config in config.vm_configs {
                if vm_config.starts_with("os/axvisor/tmp/vmconfigs") {
                    continue;
                }
                checked += 1;
                let vm_config_path = if vm_config.is_absolute() {
                    vm_config
                } else {
                    workspace_root.join(vm_config)
                };
                assert!(
                    vm_config_path.is_file(),
                    "{} references missing vm_config {}",
                    path.display(),
                    vm_config_path.display()
                );
            }
        }
    }

    assert!(checked > 0);
}

#[test]
fn parses_supported_arch_aliases() {
    assert_eq!(
        parse_target(&Some("aarch64".to_string()), &None).unwrap(),
        (
            "aarch64".to_string(),
            "aarch64-unknown-none-softfloat".to_string()
        )
    );
    assert_eq!(
        parse_target(&Some("x86_64".to_string()), &None).unwrap(),
        ("x86_64".to_string(), "x86_64-unknown-none".to_string())
    );
    assert_eq!(
        parse_target(&Some("loongarch64".to_string()), &None).unwrap(),
        (
            "loongarch64".to_string(),
            "loongarch64-unknown-none-softfloat".to_string()
        )
    );
    assert_eq!(
        parse_target(&Some("riscv64".to_string()), &None).unwrap(),
        (
            "riscv64".to_string(),
            "riscv64gc-unknown-none-elf".to_string()
        )
    );
}

#[test]
fn accepts_full_target_triples() {
    assert_eq!(
        parse_target(&None, &Some("aarch64-unknown-none-softfloat".to_string())).unwrap(),
        (
            "aarch64".to_string(),
            "aarch64-unknown-none-softfloat".to_string()
        )
    );
    assert_eq!(
        parse_target(&None, &Some("riscv64gc-unknown-none-elf".to_string())).unwrap(),
        (
            "riscv64".to_string(),
            "riscv64gc-unknown-none-elf".to_string()
        )
    );
    assert_eq!(
        parse_target(
            &None,
            &Some("loongarch64-unknown-none-softfloat".to_string())
        )
        .unwrap(),
        (
            "loongarch64".to_string(),
            "loongarch64-unknown-none-softfloat".to_string()
        )
    );
}

#[test]
fn rejects_unsupported_arches() {
    let err = parse_target(&Some("mips64".to_string()), &None).unwrap_err();
    let err = err.to_string();

    assert!(err.contains("mips64"));
    assert!(err.contains("aarch64"));
    assert!(err.contains("loongarch64"));
    assert!(err.contains("riscv64"));
    assert!(err.contains("x86_64"));
}

#[test]
fn qemu_test_request_ignores_inherited_smp() {
    let mut request = axvisor_request(
        PathBuf::from("/tmp/build-riscv64gc-unknown-none-elf.toml"),
        "riscv64",
        "riscv64gc-unknown-none-elf",
    );
    request.smp = Some(1);

    let request = Axvisor::qemu_test_request(request);

    assert_eq!(request.smp, None);
}

#[test]
fn qemu_test_request_ignores_inherited_vmconfigs() {
    let mut request = axvisor_request(
        PathBuf::from("/tmp/build-x86_64-unknown-none.toml"),
        "x86_64",
        "x86_64-unknown-none",
    );
    request
        .vmconfigs
        .push(PathBuf::from("tmp/old-axvisor-vm.toml"));

    let request = Axvisor::qemu_test_request(request);

    assert!(request.vmconfigs.is_empty());
}

#[test]
fn discovers_only_cases_with_matching_qemu_config() {
    let root = tempdir().unwrap();
    let build_config = write_qemu_build_config(
        root.path(),
        "normal",
        "default",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    write_qemu_config(
        root.path(),
        "smoke",
        "aarch64",
        "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = []\n",
    );
    write_qemu_config(
        root.path(),
        "x86-only",
        "x86_64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = []\nfail_regex \
         = []\n",
    );

    let cases = discover_qemu_cases(
        root.path(),
        "normal",
        "aarch64",
        "aarch64-unknown-none-softfloat",
        None,
    )
    .unwrap();

    assert_eq!(
        cases
            .iter()
            .map(|case| case.case.name.as_str())
            .collect::<Vec<_>>(),
        vec!["smoke"]
    );
    assert_eq!(cases[0].build_config_path, build_config);
}

#[test]
fn selected_case_requires_matching_qemu_config() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "default",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    write_qemu_config(
        root.path(),
        "smoke",
        "x86_64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = []\nfail_regex \
         = []\n",
    );

    let err = discover_qemu_cases(
        root.path(),
        "normal",
        "aarch64",
        "aarch64-unknown-none-softfloat",
        Some("smoke"),
    )
    .unwrap_err();

    assert!(err.to_string().contains("none provide `qemu-aarch64.toml`"));
}

#[test]
fn selected_qemu_case_skips_non_qemu_case_with_same_name() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "board-orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_build_config(
        root.path(),
        "normal",
        "qemu",
        "aarch64-unknown-none-softfloat",
    );
    write_board_config_in_group(
        root.path(),
        "normal",
        "board-orangepi-5-plus",
        "smoke",
        "orangepi-5-plus-linux",
        "board_type = \"OrangePi-5-Plus\"\n",
    );
    write_qemu_config_in_group(
        root.path(),
        "normal",
        "qemu",
        "smoke",
        "aarch64",
        "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = []\n",
    );

    let cases = discover_qemu_cases(
        root.path(),
        "normal",
        "aarch64",
        "aarch64-unknown-none-softfloat",
        Some("smoke"),
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].build_group, "qemu");
    assert_eq!(cases[0].case.name, "smoke");
}

#[test]
fn discovers_qemu_cases_from_selected_group() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "default",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_build_config(
        root.path(),
        "stress",
        "stress-default",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_config(
        root.path(),
        "smoke",
        "aarch64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = []\n",
    );
    write_qemu_config_in_group(
        root.path(),
        "stress",
        "stress-default",
        "load",
        "aarch64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"stress\"\nsuccess_regex = []\nfail_regex = []\n",
    );

    let cases = discover_qemu_cases(
        root.path(),
        "stress",
        "aarch64",
        "aarch64-unknown-none-softfloat",
        None,
    )
    .unwrap();

    assert_eq!(
        cases
            .iter()
            .map(|case| case.case.name.as_str())
            .collect::<Vec<_>>(),
        vec!["load"]
    );
}

#[test]
fn discovers_qemu_cases_from_uefi_group_without_polluting_normal_group() {
    let root = tempdir().unwrap();
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    write_qemu_config_in_group(
        root.path(),
        "normal",
        "default",
        "baseline",
        "x86_64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = []\nfail_regex \
         = []\n",
    );
    write_qemu_build_config(root.path(), "uefi", "qemu-nimbos", "x86_64-unknown-none");
    write_qemu_config_in_group(
        root.path(),
        "uefi",
        "qemu-nimbos",
        "smoke",
        "x86_64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = []\nfail_regex \
         = []\n",
    );

    let normal_cases =
        discover_qemu_cases(root.path(), "normal", "x86_64", "x86_64-unknown-none", None).unwrap();
    assert_eq!(normal_cases.len(), 1);
    assert_eq!(normal_cases[0].case.name, "baseline");

    let uefi_cases =
        discover_qemu_cases(root.path(), "uefi", "x86_64", "x86_64-unknown-none", None).unwrap();
    assert_eq!(uefi_cases.len(), 1);
    assert_eq!(uefi_cases[0].case.name, "smoke");
    assert_eq!(uefi_cases[0].build_group, "qemu-nimbos");
}

#[test]
fn rejects_unknown_qemu_test_group() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "default",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_config(
        root.path(),
        "smoke",
        "aarch64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = []\n",
    );

    let err = discover_qemu_cases(
        root.path(),
        "unknown",
        "aarch64",
        "aarch64-unknown-none-softfloat",
        None,
    )
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("unsupported Axvisor test group `unknown`")
    );
    assert!(err.to_string().contains("normal"));
}

#[test]
fn returns_all_board_test_groups_when_no_filter_is_given() {
    let root = tempdir().unwrap();
    write_board_build_config(root.path(), "default");
    write_board_config(
        root.path(),
        "smoke",
        "phytiumpi-linux",
        "board_type = \"PhytiumPi\"\n",
    );
    write_board_config(
        root.path(),
        "smoke",
        "orangepi-5-plus-linux",
        "board_type = \"OrangePi-5-Plus\"\n",
    );

    let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

    assert_eq!(
        groups
            .iter()
            .map(|group| format!("{}/{}", group.name, group.board_name))
            .collect::<Vec<_>>(),
        vec!["smoke/orangepi-5-plus-linux", "smoke/phytiumpi-linux"]
    );
}

#[test]
fn discovers_board_case_when_case_dir_contains_build_config() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("test-suit/axvisor/normal/smoke");
    fs::create_dir_all(&case_dir).unwrap();
    let build_config = case_dir.join("build-aarch64-unknown-none-softfloat.toml");
    fs::write(
        &build_config,
        "target = \"aarch64-unknown-none-softfloat\"\n",
    )
    .unwrap();
    let board_test_config = case_dir.join("board-phytiumpi-linux.toml");
    fs::write(&board_test_config, "board_type = \"PhytiumPi\"\n").unwrap();

    let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "phytiumpi-linux");
    assert_eq!(groups[0].build_config, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn board_case_uses_unique_nearest_build_config_without_target_assumption() {
    let root = tempdir().unwrap();
    let wrapper_dir = root.path().join("test-suit/axvisor/normal/board-custom");
    let case_dir = wrapper_dir.join("smoke");
    fs::create_dir_all(&case_dir).unwrap();
    let build_config = wrapper_dir.join("build-riscv64gc-unknown-none-elf.toml");
    fs::write(&build_config, "target = \"riscv64gc-unknown-none-elf\"\n").unwrap();
    let board_test_config = case_dir.join("board-custom.toml");
    fs::write(&board_test_config, "board_type = \"Custom\"\n").unwrap();

    let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "custom");
    assert_eq!(groups[0].build_config, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn filters_board_test_group_by_case() {
    let root = tempdir().unwrap();
    let build_config = write_board_build_config(root.path(), "default");
    let board_test_config = write_board_config(
        root.path(),
        "smoke",
        "phytiumpi-linux",
        "board_type = \"PhytiumPi\"\n",
    );

    let groups = discover_board_test_groups(root.path(), "normal", Some("smoke"), None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "phytiumpi-linux");
    assert_eq!(groups[0].build_config, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn filters_board_test_groups_by_board() {
    let root = tempdir().unwrap();
    write_board_build_config(root.path(), "default");
    write_board_config(
        root.path(),
        "smoke",
        "phytiumpi-linux",
        "board_type = \"PhytiumPi\"\n",
    );
    write_board_config(
        root.path(),
        "syscall",
        "phytiumpi-linux",
        "board_type = \"PhytiumPi\"\n",
    );
    write_board_config(
        root.path(),
        "smoke",
        "orangepi-5-plus-linux",
        "board_type = \"OrangePi-5-Plus\"\n",
    );

    let groups =
        discover_board_test_groups(root.path(), "normal", None, Some("phytiumpi-linux")).unwrap();

    assert_eq!(
        groups
            .iter()
            .map(|group| format!("{}/{}", group.name, group.board_name))
            .collect::<Vec<_>>(),
        vec!["smoke/phytiumpi-linux", "syscall/phytiumpi-linux"]
    );
}

#[test]
fn discovers_uboot_test_group_from_board_cases() {
    let root = tempdir().unwrap();
    let build_config = write_board_build_config(root.path(), "board-rdk-s100");
    let board_test_config = write_board_config_in_group(
        root.path(),
        "normal",
        "board-rdk-s100",
        "smoke",
        "rdk-s100-linux",
        "board_type = \"RDK-S100\"\nuboot_cmd = [\"run ab_select_cmd\", \"run \
         avb_boot\"]\nsuccess_regex = [\"ubuntu login:\"]\nfail_regex = [\"(?i)panic\"]\n",
    );

    let group = discovery::discover_uboot_test_group(root.path(), "rdk-s100", "linux").unwrap();

    assert_eq!(group.name, "smoke");
    assert_eq!(group.board_name, "rdk-s100-linux");
    assert_eq!(group.build_config, build_config);
    assert_eq!(group.board_test_config_path, board_test_config);
}

#[test]
fn x86_linux_direct_boot_configs_keep_timer_calibration_bypass() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in [
        "os/axvisor/configs/vms/qemu/x86_64/linux-vmx-smp1.toml",
        "os/axvisor/configs/vms/qemu/x86_64/linux-svm-smp1.toml",
    ] {
        let content = fs::read_to_string(workspace_root.join(path)).unwrap();
        let config: TestVmKernelConfig = toml::from_str(&content).unwrap();
        let cmdline = config.kernel.cmdline;

        assert!(
            cmdline.contains("no_timer_check"),
            "{path} should keep no_timer_check to avoid x86 Linux guest timer calibration stalls"
        );
        assert!(
            cmdline.len() <= X86_LINUX_DIRECT_BOOT_CMDLINE_LIMIT,
            "{path} cmdline length {} exceeds the currently verified x86 direct-boot limit of {} \
             bytes and can truncate getty arguments",
            cmdline.len(),
            X86_LINUX_DIRECT_BOOT_CMDLINE_LIMIT
        );
        assert!(
            cmdline.contains("-- -n -l /bin/sh -L 115200 ttyS0"),
            "{path} should keep complete getty arguments after `--` so init does not exit"
        );
    }
}

#[test]
fn ignores_qemu_only_build_groups_when_discovering_board_tests() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "qemu",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_build_config(root.path(), "normal", "qemu", "x86_64-unknown-none");
    write_qemu_config(
        root.path(),
        "smoke",
        "aarch64",
        "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = []\n",
    );

    write_board_build_config(root.path(), "default");
    write_board_config(
        root.path(),
        "smoke",
        "orangepi-5-plus-linux",
        "board_type = \"OrangePi-5-Plus\"\n",
    );

    let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "orangepi-5-plus-linux");
}

#[test]
fn rejects_unknown_board_test_board() {
    let root = tempdir().unwrap();
    write_board_build_config(root.path(), "default");
    write_board_config(
        root.path(),
        "smoke",
        "phytiumpi-linux",
        "board_type = \"PhytiumPi\"\n",
    );

    let err = discover_board_test_groups(root.path(), "normal", None, Some("unknown")).unwrap_err();

    assert!(
        err.to_string()
            .contains("unsupported axvisor board test board `unknown`")
    );
    assert!(err.to_string().contains("phytiumpi-linux"));
}

#[test]
fn rejects_unknown_board_test_case() {
    let root = tempdir().unwrap();
    write_board_build_config(root.path(), "default");
    write_board_config(
        root.path(),
        "smoke",
        "phytiumpi-linux",
        "board_type = \"PhytiumPi\"\n",
    );

    let err = discover_board_test_groups(root.path(), "normal", Some("unknown"), None).unwrap_err();

    assert!(
        err.to_string()
            .contains("unsupported axvisor board test case `unknown`")
    );
    assert!(err.to_string().contains("smoke"));
}

#[test]
fn rejects_empty_board_test_group() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("test-suit/axvisor/empty")).unwrap();

    let err = discover_board_test_groups(root.path(), "empty", None, None).unwrap_err();

    assert!(
        err.to_string()
            .contains("no Axvisor board test groups found under")
    );
}

#[test]
fn board_case_config_is_also_valid_board_run_config() {
    let config: ostool::board::config::BoardRunConfig = toml::from_str(
        "board_type = \"PhytiumPi\"\nshell_prefix = \"login:\"\nshell_init_cmd = \
         \"root\"\nsuccess_regex = [\"(?m)^root@.*#\\\\s*$\"]\n",
    )
    .unwrap();

    assert_eq!(config.board_type, "PhytiumPi");
    assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
}

#[test]
fn orangepi_linux_board_gate_bounds_slow_storage_and_rejects_its_ownership_faults() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = workspace_root.join(
        "test-suit/axvisor/normal/board-orangepi-5-plus/smoke/board-orangepi-5-plus-linux.toml",
    );
    assert!(
        path.is_file(),
        "missing board gate config {}",
        path.display()
    );
    let config: ostool::board::config::BoardRunConfig =
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();

    assert!(
        config.timeout.is_some_and(|timeout| timeout >= 600),
        "the slow OrangePi-5-Plus storage path needs a bounded 600 second board budget"
    );
    for diagnostic in [
        "ITS queue timeout",
        "Booted with LPIs enabled, memory probably corrupted",
        "Failed to disable LPIs",
    ] {
        assert!(
            config
                .fail_regex
                .iter()
                .any(|pattern| pattern.contains(diagnostic)),
            "the board gate must fail on the ITS ownership diagnostic `{diagnostic}`"
        );
    }
}
