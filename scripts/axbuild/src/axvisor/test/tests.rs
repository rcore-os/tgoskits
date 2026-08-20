use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use ostool::run::qemu::QemuConfig;
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
    #[serde(default)]
    cmdline: String,
}

#[derive(Debug, Eq, PartialEq, serde::Deserialize)]
struct TestOvmfBuildConfig {
    #[serde(default)]
    features: Vec<String>,
    vm_configs: Vec<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct TestOvmfGuestConfig {
    kernel: TestOvmfGuestKernel,
}

#[derive(serde::Deserialize)]
struct TestOvmfGuestKernel {
    uefi_firmware_path: PathBuf,
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
fn orangepi_guest_board_cases_use_matching_vm_configs() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for (board_name, expected_vm_config) in [
        (
            "orangepi-5-plus-linux",
            "os/axvisor/configs/vms/orangepi-5-plus/linux-smp1.toml",
        ),
        (
            "orangepi-5-plus-starry",
            "os/axvisor/configs/vms/orangepi-5-plus/starry-smp1.toml",
        ),
    ] {
        let groups =
            discover_board_test_groups(&workspace_root, "normal", None, Some(board_name)).unwrap();
        assert_eq!(groups.len(), 1, "expected one case for {board_name}");

        let build_config = fs::read_to_string(&groups[0].build_config).unwrap();
        let build_config: TestBuildConfigVmConfigs = toml::from_str(&build_config).unwrap();
        assert_eq!(
            build_config.vm_configs,
            [PathBuf::from(expected_vm_config)],
            "{board_name} should select its matching guest VM config"
        );
    }
}

#[test]
fn rock4d_board_build_selects_linux_guest() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let build_path = "os/axvisor/configs/board/rock-4d.toml";
    let build_config = fs::read_to_string(workspace_root.join(build_path)).unwrap();
    let build_config: TestBuildConfigVmConfigs = toml::from_str(&build_config).unwrap();

    assert_eq!(
        build_config.vm_configs,
        [PathBuf::from(
            "os/axvisor/configs/vms/rock-4d/linux-smp1.toml"
        )],
        "{build_path} should embed the default ROCK 4D Linux guest config"
    );
}

#[test]
fn orangepi_linux_guest_does_not_use_uart_clock_workaround() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = "os/axvisor/configs/vms/orangepi-5-plus/linux-smp1.toml";
    let content = fs::read_to_string(workspace_root.join(path)).unwrap();
    let config: TestVmKernelConfig = toml::from_str(&content).unwrap();

    // The Rockchip assignment and AxVM shared-MMIO tests pin the protection itself. This
    // board-level contract prevents the guest config from silently bypassing that path.
    assert!(
        !config.kernel.cmdline.contains("clk_ignore_unused"),
        "{path} must protect the host-owned UART clock through shared-provider mediation"
    );
    assert!(
        config.kernel.cmdline.contains("console=ttyS2,1500000"),
        "{path} must route the guest console through the machine-owned virtual UART"
    );
}

#[test]
fn rk3568_linux_guest_uses_the_virtual_16550_console() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = "os/axvisor/configs/vms/roc-rk3568-pc/linux-smp1.toml";
    let content = fs::read_to_string(workspace_root.join(path)).unwrap();
    let config: TestVmKernelConfig = toml::from_str(&content).unwrap();

    assert!(
        config.kernel.cmdline.contains("console=ttyS2,1500000"),
        "{path} must route the login console through the machine-owned virtual 16550"
    );
    assert!(
        !config.kernel.cmdline.contains("console=ttyFIQ0"),
        "{path} must not route the login console through the removed physical FIQ debugger"
    );
}

#[test]
fn x86_hypervisor_backend_cases_request_raw_bin_artifacts() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for (backend, cpu_features) in [
        (
            "vmx",
            &["+vmx-ept", "+vmx-unrestricted-guest", "+vmx-flexpriority"],
        ),
        ("svm", &["+svm", "+npt", "+nrip-save"]),
    ] {
        let path = workspace_root.join(format!(
            "test-suit/axvisor/normal/qemu/smoke/qemu-x86_64-{backend}.toml"
        ));
        let config: QemuConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        assert!(
            config.uefi,
            "{backend} smoke must boot the dynamic x86 host through UEFI"
        );
        assert!(
            config.to_bin,
            "{backend} smoke must provide a raw BIN for the UEFI ESP"
        );
        assert!(
            !config.args.iter().any(|arg| arg == "-nodefaults"),
            "{backend} UEFI smoke needs QEMU's default firmware devices"
        );

        let machine = qemu_argument_value(&config.args, "-machine");
        assert!(
            !machine.contains("sata=off") && !machine.contains("i8042=off"),
            "{backend} UEFI smoke must keep the firmware boot bus available"
        );

        let cpu = qemu_argument_value(&config.args, "-cpu");
        assert!(cpu.contains("-la57"));
        for feature in cpu_features {
            assert!(
                cpu.contains(feature),
                "{backend} smoke must enable the required CPU feature {feature}"
            );
        }
    }
}

#[test]
fn x86_ovmf_acpi_cases_share_one_backend_neutral_guest_contract() {
    const BUILD_OUTPUT_ENV: &str = "AXVISOR_TEST_X86_OVMF_OUTPUT";

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = discover_qemu_cases(
        &workspace_root,
        "normal",
        "x86_64",
        "x86_64-unknown-none",
        None,
    )
    .unwrap();
    let vmx_case = cases
        .iter()
        .find(|case| case.case.name == "ovmf-acpi-vmx")
        .expect("runner should discover the VMX OVMF ACPI case");
    let svm_case = cases
        .iter()
        .find(|case| case.case.name == "ovmf-acpi-svm")
        .expect("runner should discover the SVM OVMF ACPI case");

    let vmx_build = load_ovmf_build_config(&vmx_case.build_config_path);
    let svm_build = load_ovmf_build_config(&svm_case.build_config_path);
    assert_eq!(vmx_build, svm_build);
    assert_eq!(vmx_build.vm_configs.len(), 1);
    assert!(
        vmx_build
            .features
            .iter()
            .all(|feature| feature != "vmx" && feature != "svm"),
        "OVMF ACPI build configs must leave backend selection to runtime CPUID"
    );

    let guest_path = workspace_root.join(&vmx_build.vm_configs[0]);
    let guest: TestOvmfGuestConfig =
        toml::from_str(&fs::read_to_string(&guest_path).unwrap()).unwrap();
    let build_output = vmx_build
        .env
        .get(BUILD_OUTPUT_ENV)
        .expect("OVMF build config should prepare the guest firmware image");
    assert_eq!(
        guest.kernel.uefi_firmware_path,
        PathBuf::from(format!("${{workspace}}/{build_output}")),
        "the prepared image must be the image loaded by the guest configuration"
    );

    let mut vmx_qemu = load_qemu_config(&vmx_case.case.qemu_config_path);
    let mut svm_qemu = load_qemu_config(&svm_case.case.qemu_config_path);
    assert_eq!(
        replace_qemu_argument(&mut vmx_qemu.args, "-cpu", "<backend>"),
        "host,-la57,+vmx-ept,+vmx-unrestricted-guest,+vmx-flexpriority"
    );
    assert_eq!(
        replace_qemu_argument(&mut svm_qemu.args, "-cpu", "<backend>"),
        "host,-la57,+svm,+npt,+nrip-save"
    );
    assert_eq!(
        vmx_qemu, svm_qemu,
        "VMX and SVM OVMF ACPI cases may differ only in outer QEMU CPU capabilities"
    );
}

fn load_ovmf_build_config(path: &Path) -> TestOvmfBuildConfig {
    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn load_qemu_config(path: &Path) -> QemuConfig {
    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn replace_qemu_argument(args: &mut [String], option: &str, replacement: &str) -> String {
    let index = args
        .iter()
        .position(|argument| argument == option)
        .unwrap_or_else(|| panic!("missing QEMU option {option}"));
    std::mem::replace(
        args.get_mut(index + 1)
            .unwrap_or_else(|| panic!("missing value for QEMU option {option}")),
        replacement.to_string(),
    )
}

fn qemu_argument_value<'a>(args: &'a [String], option: &str) -> &'a str {
    let index = args
        .iter()
        .position(|arg| arg == option)
        .unwrap_or_else(|| panic!("missing QEMU option {option}"));
    args.get(index + 1)
        .unwrap_or_else(|| panic!("missing value for QEMU option {option}"))
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
fn board_test_request_ignores_inherited_smp() {
    let mut request = axvisor_request(
        PathBuf::from("/tmp/build-aarch64-unknown-none-softfloat.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    request.smp = Some(2);

    let request = Axvisor::board_test_request(request);

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
fn discovers_qemu_cases_from_custom_group_without_polluting_normal_group() {
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
    write_qemu_build_config(root.path(), "custom", "firmware", "x86_64-unknown-none");
    write_qemu_config_in_group(
        root.path(),
        "custom",
        "firmware",
        "smoke",
        "x86_64",
        "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = []\nfail_regex \
         = []\n",
    );

    let normal_cases =
        discover_qemu_cases(root.path(), "normal", "x86_64", "x86_64-unknown-none", None).unwrap();
    assert_eq!(normal_cases.len(), 1);
    assert_eq!(normal_cases[0].case.name, "baseline");

    let custom_cases =
        discover_qemu_cases(root.path(), "custom", "x86_64", "x86_64-unknown-none", None).unwrap();
    assert_eq!(custom_cases.len(), 1);
    assert_eq!(custom_cases[0].case.name, "smoke");
    assert_eq!(custom_cases[0].build_group, "firmware");
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
fn x86_linux_direct_boot_config_keeps_shared_safety_options() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = "os/axvisor/configs/vms/qemu/x86_64/linux-smp1.toml";
    let content = fs::read_to_string(workspace_root.join(path)).unwrap();
    let config: TestVmKernelConfig = toml::from_str(&content).unwrap();
    let cmdline = config.kernel.cmdline;

    assert!(
        cmdline.contains("no_timer_check"),
        "{path} should keep no_timer_check to avoid x86 Linux guest timer calibration stalls"
    );
    for option in [
        "rootwait",
        "nox2apic",
        "tsc=unstable",
        "initcall_blacklist=ahci_pci_driver_init,i8042_init",
    ] {
        assert!(
            cmdline.contains(option),
            "{path} should retain the shared x86 direct-boot safety option {option}"
        );
    }
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
    assert!(
        !cmdline
            .split_ascii_whitespace()
            .any(|arg| arg == "acpi=off"),
        "{path} should exercise the default ACPI boot path; the MP-table fallback has a dedicated \
         test"
    );
}

#[test]
fn asus_nuc15crh_linux_limits_legacy_serial_probe() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = "os/axvisor/configs/vms/asus-nuc15crh/linux-smp1.toml";
    let content = fs::read_to_string(workspace_root.join(path)).unwrap();
    let config: TestVmKernelConfig = toml::from_str(&content).unwrap();
    let cmdline = config.kernel.cmdline;

    assert!(
        cmdline.contains("8250.nr_uarts=1"),
        "{path} should only probe the machine-owned COM1 UART on the ASUS NUC HTTP Boot board path"
    );
}

#[test]
fn nvme_smoke_keeps_storage_in_host_and_verifies_file_io() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for (name, build_path, qemu_path) in [
        (
            "aarch64",
            "test-suit/axvisor/normal/qemu/build-aarch64-unknown-none-softfloat.toml",
            "test-suit/axvisor/normal/qemu/smoke/qemu-aarch64.toml",
        ),
        (
            "riscv64",
            "test-suit/axvisor/normal/qemu/build-riscv64gc-unknown-none-elf.toml",
            "test-suit/axvisor/normal/qemu/smoke/qemu-riscv64.toml",
        ),
        (
            "loongarch64",
            "test-suit/axvisor/normal/qemu/build-loongarch64-unknown-none-softfloat.toml",
            "test-suit/axvisor/normal/qemu/smoke/qemu-loongarch64.toml",
        ),
        (
            "x86_64-svm",
            "test-suit/axvisor/normal/qemu/build-x86_64-unknown-none-svm.toml",
            "test-suit/axvisor/normal/qemu/smoke/qemu-x86_64-svm.toml",
        ),
        (
            "x86_64-vmx",
            "test-suit/axvisor/normal/qemu/build-x86_64-unknown-none-vmx.toml",
            "test-suit/axvisor/normal/qemu/smoke/qemu-x86_64-vmx.toml",
        ),
    ] {
        let build_content = fs::read_to_string(workspace_root.join(&build_path)).unwrap();
        let build: TestBuildConfigVmConfigs = toml::from_str(&build_content).unwrap();
        assert!(
            build.vm_configs.is_empty(),
            "{build_path} should keep the NVMe root filesystem owned by the Axvisor host; guest \
             block ABI validation is outside this migration"
        );

        let qemu_content = fs::read_to_string(workspace_root.join(&qemu_path)).unwrap();
        let qemu: QemuConfig = toml::from_str(&qemu_content).unwrap();
        let command = qemu
            .shell_init_cmd
            .unwrap_or_else(|| panic!("{name} NVMe smoke should inject a host file-I/O command"));

        for required_step in [
            "> /tmp/axvisor-nvme-rw",
            "\ncat /tmp/axvisor-nvme-rw",
            "rm -f /tmp/axvisor-nvme-rw",
            "AXVISOR_NVME_ROOTFS_RW_PASSED",
        ] {
            assert!(
                command.contains(required_step),
                "{qemu_path} should include `{required_step}` in its host file-I/O smoke command"
            );
        }
        assert_eq!(
            qemu.shell_prefix.as_deref(),
            Some("axvisor:/$"),
            "{qemu_path} should wait for the Axvisor host shell"
        );
        let expected_success_regex = if name == "aarch64" {
            vec![
                r"(?m)^AXVISOR SHLEX  EMPTY\s*$",
                r"(?m)^Command: vm start\s*$",
                r"(?m)^Error: Invalid command syntax\s*$",
                r"(?m)^AXVISOR_NVME_RW_PAYLOAD\s*$",
                r"(?m)^AXVISOR_NVME_ROOTFS_RW_PASSED\s*$",
            ]
        } else {
            vec![
                r"(?m)^AXVISOR_NVME_RW_PAYLOAD\s*$",
                r"(?m)^AXVISOR_NVME_ROOTFS_RW_PASSED\s*$",
            ]
        };
        assert_eq!(
            qemu.success_regex, expected_success_regex,
            "{qemu_path} should require all architecture-specific shell markers"
        );
    }
}

#[test]
fn shell_command_failure_regex_ignores_smp_log_interleaving() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for path in [
        "test-suit/axvisor/normal/qemu/smoke/qemu-aarch64.toml",
        "test-suit/axvisor/normal/qemu/smoke/qemu-loongarch64.toml",
        "test-suit/axvisor/normal/qemu/smoke/qemu-riscv64.toml",
        "test-suit/axvisor/normal/qemu/smoke/qemu-x86_64-svm.toml",
        "test-suit/axvisor/normal/qemu/smoke/qemu-x86_64-vmx.toml",
        "test-suit/axvisor/normal/qemu-riscv-ipi/smp-ipi/qemu-riscv64.toml",
    ] {
        let content = fs::read_to_string(workspace_root.join(path)).unwrap();
        let config: QemuConfig = toml::from_str(&content).unwrap();
        let pattern = config
            .fail_regex
            .iter()
            .find(|pattern| pattern.contains("echo|cat|rm"))
            .unwrap_or_else(|| panic!("{path} should reject shell command failures"));
        let regex = regex::Regex::new(pattern).unwrap();

        assert!(regex.is_match("cat: can't open '/missing': No such file or directory"));
        assert!(regex.is_match("rm: can't remove '/missing': No such file or directory"));
        assert!(
            !regex.is_match("rm:axvm::host::arceos:373] Hardware virtualization enabled"),
            "{path} must not interpret an SMP serial-log splice as a shell command failure"
        );
    }
}

#[test]
fn aarch64_nvme_smoke_fail_regex_ignores_interleaved_kernel_logs() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let qemu_path = "test-suit/axvisor/normal/qemu/smoke/qemu-aarch64.toml";
    let qemu = load_qemu_config(&workspace_root.join(qemu_path));
    let fail_regexes = qemu
        .fail_regex
        .iter()
        .map(|pattern| regex::Regex::new(pattern).unwrap())
        .collect::<Vec<_>>();
    let matches_failure = |output: &str| fail_regexes.iter().any(|regex| regex.is_match(output));

    for interleaved_output in [
        "rm:Core Waiting for all cores to enable hardware virtualization...",
        "rm:370329Initializing AxVM timer wheel...",
        "rm: Core Waiting for all cores to enable hardware virtualization...",
        "rm: 370329Initializing AxVM timer wheel...",
        "rm: s initializing hardware virtualization support...",
    ] {
        assert!(
            !matches_failure(interleaved_output),
            "{qemu_path} should not treat interleaved shell echo and kernel log as a failure: \
             {interleaved_output}"
        );
    }

    for shell_error in [
        "rm: cannot remove '/tmp/axvisor-nvme-rw': Read-only file system",
        "cat: /tmp/axvisor-nvme-rw: No such file or directory",
        "echo: /tmp/axvisor-nvme-rw: No space left on device",
    ] {
        assert!(
            matches_failure(shell_error),
            "{qemu_path} should still reject a real shell command error: {shell_error}"
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
fn qemu_build_groups_preserve_distinct_executable_artifacts() {
    let root = tempdir().unwrap();
    let build_output = root.path().join("target/release/axvisor");
    let artifact_directory = root.path().join("preserved");
    fs::create_dir_all(build_output.parent().unwrap()).unwrap();

    fs::write(&build_output, b"first VM config").unwrap();
    let first =
        super::qemu::preserve_qemu_build_artifact(&build_output, &artifact_directory, 0).unwrap();
    fs::write(&build_output, b"second VM config").unwrap();
    let second =
        super::qemu::preserve_qemu_build_artifact(&build_output, &artifact_directory, 1).unwrap();

    assert_ne!(first, second);
    assert_eq!(fs::read(first).unwrap(), b"first VM config");
    assert_eq!(fs::read(second).unwrap(), b"second VM config");
}

#[test]
fn qemu_cases_activate_their_build_group_artifact_and_conversion_mode() {
    let first = false;
    let second = true;
    let third = false;
    let first_group = [&first, &second];
    let second_group = [&third];
    let groups = [first_group.as_slice(), second_group.as_slice()];
    let artifacts = [
        PathBuf::from("group-0/axvisor"),
        PathBuf::from("group-1/axvisor"),
    ];

    let plan =
        super::qemu::plan_qemu_case_artifacts(&groups, &artifacts, |to_bin| *to_bin).unwrap();

    assert_eq!(plan.len(), 3);
    assert_eq!(plan[0].build_group_index, 0);
    assert_eq!(plan[0].build_artifact, artifacts[0]);
    assert!(!plan[0].to_bin);
    assert_eq!(plan[1].build_group_index, 0);
    assert_eq!(plan[1].build_artifact, artifacts[0]);
    assert!(plan[1].to_bin);
    assert_eq!(plan[2].build_group_index, 1);
    assert_eq!(plan[2].build_artifact, artifacts[1]);
    assert!(!plan[2].to_bin);

    let err = super::qemu::plan_qemu_case_artifacts(&groups, &artifacts[..1], |to_bin| *to_bin)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("does not match preserved artifact count")
    );
}
