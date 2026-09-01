use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::tempdir;

use super::*;
use crate::{axvisor::build, context::ResolvedAxvisorRequest};

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

    let case = cases
        .iter()
        .find(|case| case.case.name == "smoke")
        .expect("matching qemu case should be discovered");
    assert_eq!(case.build_config_path, build_config);
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

    assert!(cases.iter().any(|case| case.case.name == "load"));
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
    assert_eq!(normal_cases[0].case.name, "baseline");

    let custom_cases =
        discover_qemu_cases(root.path(), "custom", "x86_64", "x86_64-unknown-none", None).unwrap();
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

    assert!(
        groups
            .iter()
            .any(|group| { group.name == "smoke" && group.board_name == "orangepi-5-plus-linux" })
    );
    assert!(
        groups
            .iter()
            .any(|group| { group.name == "smoke" && group.board_name == "phytiumpi-linux" })
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

    assert!(
        groups
            .iter()
            .all(|group| group.board_name == "phytiumpi-linux")
    );
    assert!(groups.iter().any(|group| group.name == "smoke"));
    assert!(groups.iter().any(|group| group.name == "syscall"));
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
