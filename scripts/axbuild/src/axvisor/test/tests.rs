use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::tempdir;

use super::*;

/// Suite roots, mirroring the split used in production: regular functional
/// cases under `test-suit/axvisor`, migrated nightly cases under
/// `apps/axvisor`.
const TEST_SUIT_ROOT: &str = "test-suit/axvisor";
const MIGRATED_SUITE_ROOT: &str = "apps/axvisor";

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
    write_qemu_config_in_suite_root(root, TEST_SUIT_ROOT, group, build_group, case, arch, body)
}

fn write_qemu_config_in_suite_root(
    root: &Path,
    suite_root: &str,
    group: &str,
    build_group: &str,
    case: &str,
    arch: &str,
    body: &str,
) -> PathBuf {
    let dir = root
        .join(suite_root)
        .join(group)
        .join(build_group)
        .join(case);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("qemu-{arch}.toml"));
    fs::write(&path, body).unwrap();
    path
}

fn write_qemu_build_config(root: &Path, group: &str, build_group: &str, target: &str) -> PathBuf {
    write_qemu_build_config_in_suite_root(root, TEST_SUIT_ROOT, group, build_group, target)
}

fn write_qemu_build_config_in_suite_root(
    root: &Path,
    suite_root: &str,
    group: &str,
    build_group: &str,
    target: &str,
) -> PathBuf {
    let dir = root.join(suite_root).join(group).join(build_group);
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
    write_qemu_build_config_in_suite_root(
        root,
        TEST_SUIT_ROOT,
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
    write_board_config_in_suite_root(root, TEST_SUIT_ROOT, group, build_group, case, name, body)
}

fn write_board_config_in_suite_root(
    root: &Path,
    suite_root: &str,
    group: &str,
    build_group: &str,
    case: &str,
    name: &str,
    body: &str,
) -> PathBuf {
    let dir = root
        .join(suite_root)
        .join(group)
        .join(build_group)
        .join(case);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("board-{name}.toml"));
    fs::write(&path, body).unwrap();
    path
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
        "shell_check_steps = [{ shell_prefix = \"~ #\", shell_cmd = \"pwd\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
    );
    write_qemu_config(
        root.path(),
        "x86-only",
        "x86_64",
        "shell_check_steps = [{ shell_prefix = \">>\", shell_cmd = \"hello_world\" }]\nfail_regex \
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
fn rejects_test_commands_during_axvisor_case_discovery() {
    for steps in [
        "",
        "[[shell_check_steps]]\nshell_prefix = \"guest#\"\nshell_cmd = \
         \"run-tests\"\nsuccess_regex = [\"PASSED\"]\n",
    ] {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        let path = write_qemu_config(
            root.path(),
            "unsupported",
            "aarch64",
            &format!("test_commands = [\"/usr/bin/test-a\"]\n{steps}"),
        );
        let error = discover_qemu_cases(
            root.path(),
            "normal",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("test_commands"), "{error}");
        assert!(error.contains("shell_check_steps"), "{error}");
        assert!(error.contains(&path.display().to_string()), "{error}");
    }
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
        "shell_check_steps = [{ shell_prefix = \">>\", shell_cmd = \"hello_world\" }]\nfail_regex \
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
        "shell_check_steps = [{ shell_prefix = \"~ #\", shell_cmd = \"pwd\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
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
        "shell_check_steps = [{ shell_prefix = \">>\", shell_cmd = \"normal\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
    );
    write_qemu_config_in_group(
        root.path(),
        "stress",
        "stress-default",
        "load",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \">>\", shell_cmd = \"stress\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
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
        "shell_check_steps = [{ shell_prefix = \">>\", shell_cmd = \"hello_world\" }]\nfail_regex \
         = []\n",
    );
    write_qemu_build_config(root.path(), "custom", "firmware", "x86_64-unknown-none");
    write_qemu_config_in_group(
        root.path(),
        "custom",
        "firmware",
        "smoke",
        "x86_64",
        "shell_check_steps = [{ shell_prefix = \">>\", shell_cmd = \"hello_world\" }]\nfail_regex \
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
        "shell_check_steps = [{ shell_prefix = \">>\", shell_cmd = \"normal\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
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

    let groups = discover_board_test_groups(root.path(), "normal", &[], &[]).unwrap();

    assert_eq!(
        groups
            .iter()
            .map(|group| format!("{}/{}", group.name, group.board_name))
            .collect::<Vec<_>>(),
        vec!["smoke/orangepi-5-plus-linux", "smoke/phytiumpi-linux"]
    );
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

    let groups = discover_board_test_groups(root.path(), "normal", &[], &[]).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "custom");
    assert_eq!(groups[0].build_config, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn merges_benchmark_suite_root_into_board_discovery() {
    let root = tempdir().unwrap();
    let regular_build_config = write_board_build_config(root.path(), "board-orangepi-5-plus");
    let regular_board_config = write_board_config_in_group(
        root.path(),
        "normal",
        "board-orangepi-5-plus",
        "smoke",
        "orangepi-5-plus-linux",
        "board_type = \"OrangePi-5-Plus\"\n",
    );

    let benchmark_wrapper = root
        .path()
        .join("benchmarks/axvisor/board-orangepi-5-plus/vcpu-perf");
    let benchmark_case = benchmark_wrapper.join("performance");
    fs::create_dir_all(&benchmark_case).unwrap();
    let benchmark_build_config =
        benchmark_wrapper.join("build-aarch64-unknown-none-softfloat.toml");
    fs::write(
        &benchmark_build_config,
        "target = \"aarch64-unknown-none-softfloat\"\n",
    )
    .unwrap();
    let benchmark_board_config = benchmark_case.join("board-orangepi-5-plus-vcpu-perf.toml");
    fs::write(
        &benchmark_board_config,
        "board_type = \"OrangePi-5-Plus\"\n",
    )
    .unwrap();

    let groups = discover_board_test_groups(root.path(), "normal", &[], &[]).unwrap();

    assert_eq!(groups.len(), 2);
    let benchmark = groups
        .iter()
        .find(|group| group.board_name == "orangepi-5-plus-vcpu-perf")
        .expect("benchmark board case must be discovered");
    assert_eq!(benchmark.name, "performance");
    assert_eq!(benchmark.build_config, benchmark_build_config);
    assert_eq!(benchmark.board_test_config_path, benchmark_board_config);
    let regular = groups
        .iter()
        .find(|group| group.board_name == "orangepi-5-plus-linux")
        .expect("regular board case must stay discoverable");
    assert_eq!(regular.name, "smoke");
    assert_eq!(regular.build_config, regular_build_config);
    assert_eq!(regular.board_test_config_path, regular_board_config);

    let selected = discover_board_test_groups(
        root.path(),
        "normal",
        &[],
        &["orangepi-5-plus-vcpu-perf".to_string()],
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "performance");
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

    let groups =
        discover_board_test_groups(root.path(), "normal", &["smoke".to_string()], &[]).unwrap();

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
        discover_board_test_groups(root.path(), "normal", &[], &["phytiumpi-linux".to_string()])
            .unwrap();

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
         avb_boot\"]\nfail_regex = [\"(?i)panic\"]\n\n[[shell_check_steps]]\nsuccess_regex = \
         [\"ubuntu login:\"]\n",
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
        "shell_check_steps = [{ shell_prefix = \"~ #\", shell_cmd = \"pwd\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
    );

    write_board_build_config(root.path(), "default");
    write_board_config(
        root.path(),
        "smoke",
        "orangepi-5-plus-linux",
        "board_type = \"OrangePi-5-Plus\"\n",
    );

    let groups = discover_board_test_groups(root.path(), "normal", &[], &[]).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "orangepi-5-plus-linux");
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

#[test]
fn merges_migrated_suite_root_into_qemu_discovery() {
    let root = tempdir().unwrap();
    let regular_build = write_qemu_build_config(
        root.path(),
        "normal",
        "qemu",
        "aarch64-unknown-none-softfloat",
    );
    let regular_config = write_qemu_config_in_group(
        root.path(),
        "normal",
        "qemu",
        "smoke",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \"~ #\", shell_cmd = \"pwd\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
    );
    let migrated_build = write_qemu_build_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress",
        "aarch64-unknown-none-softfloat",
    );
    let migrated_config = write_qemu_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress",
        "gicv3-timer-stress",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \"axvisor:/$\", shell_cmd = \"vm console 1\" \
         }]\nsuccess_regex = [\"AXVISOR_GICV3_TIMER_STRESS_PASSED\"]\nfail_regex = []\n",
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
        vec!["smoke", "gicv3-timer-stress"]
    );
    let regular = cases
        .iter()
        .find(|case| case.case.name == "smoke")
        .expect("functional case under test-suit must stay discoverable");
    assert_eq!(regular.build_config_path, regular_build);
    assert_eq!(regular.case.qemu_config_path, regular_config);
    let migrated = cases
        .iter()
        .find(|case| case.case.name == "gicv3-timer-stress")
        .expect("nightly case under apps must be discoverable");
    assert_eq!(migrated.build_config_path, migrated_build);
    assert_eq!(migrated.case.qemu_config_path, migrated_config);
}

#[test]
fn selects_migrated_case_absent_from_test_suit() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "qemu",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_config_in_group(
        root.path(),
        "normal",
        "qemu",
        "smoke",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \"~ #\", shell_cmd = \"pwd\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
    );
    write_qemu_build_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress-v2",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress-v2",
        "gicv2-timer-stress",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \"axvisor:/$\", shell_cmd = \"vm console 1\" \
         }]\nsuccess_regex = [\"AXVISOR_GICV2_TIMER_STRESS_PASSED\"]\nfail_regex = []\n",
    );

    let cases = discover_qemu_cases(
        root.path(),
        "normal",
        "aarch64",
        "aarch64-unknown-none-softfloat",
        Some("gicv2-timer-stress"),
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.name, "gicv2-timer-stress");
}

#[test]
fn rejects_group_absent_from_every_suite_root() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "default",
        "aarch64-unknown-none-softfloat",
    );
    fs::create_dir_all(root.path().join(MIGRATED_SUITE_ROOT).join("nightly")).unwrap();

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
    assert!(err.to_string().contains("nightly"));
    assert!(err.to_string().contains("normal"));
}

#[test]
fn merges_migrated_suite_root_into_board_discovery() {
    let root = tempdir().unwrap();
    let regular_build = write_board_build_config(root.path(), "default");
    let regular_board = write_board_config(
        root.path(),
        "smoke",
        "orangepi-5-plus-linux",
        "board_type = \"OrangePi-5-Plus\"\n",
    );

    let migrated_wrapper = root
        .path()
        .join(MIGRATED_SUITE_ROOT)
        .join("normal/board-orangepi-5-plus/robot-real-starry");
    let migrated_case = migrated_wrapper.join("smoke");
    fs::create_dir_all(&migrated_case).unwrap();
    let migrated_build = migrated_wrapper.join("build-aarch64-unknown-none-softfloat.toml");
    fs::write(
        &migrated_build,
        "target = \"aarch64-unknown-none-softfloat\"\n",
    )
    .unwrap();
    let migrated_board = migrated_case.join("board-orangepi-5-plus-robot-real-starry.toml");
    fs::write(&migrated_board, "board_type = \"OrangePi-5-Plus-robot\"\n").unwrap();

    let groups = discover_board_test_groups(root.path(), "normal", &[], &[]).unwrap();
    let migrated = groups
        .iter()
        .find(|group| group.board_name == "orangepi-5-plus-robot-real-starry")
        .expect("nightly board case under apps must be discoverable");
    assert_eq!(migrated.name, "smoke");
    assert_eq!(migrated.build_config, migrated_build);
    assert_eq!(migrated.board_test_config_path, migrated_board);
    let regular = groups
        .iter()
        .find(|group| group.board_name == "orangepi-5-plus-linux")
        .expect("functional board case under test-suit must stay discoverable");
    assert_eq!(regular.name, "smoke");
    assert_eq!(regular.build_config, regular_build);
    assert_eq!(regular.board_test_config_path, regular_board);

    let selected = discover_board_test_groups(
        root.path(),
        "normal",
        &[],
        &["orangepi-5-plus-robot-real-starry".to_string()],
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "smoke");
}

#[test]
fn merges_group_names_from_every_suite_root() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join(TEST_SUIT_ROOT).join("normal")).unwrap();
    fs::create_dir_all(root.path().join(MIGRATED_SUITE_ROOT).join("nightly")).unwrap();

    let groups = discovery::discover_test_group_names(root.path()).unwrap();

    assert_eq!(groups, vec!["nightly".to_string(), "normal".to_string()]);
}

fn write_healthy_aarch64_qemu_case(root: &Path) {
    write_qemu_build_config(root, "normal", "qemu", "aarch64-unknown-none-softfloat");
    write_qemu_config_in_group(
        root,
        "normal",
        "qemu",
        "smoke",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \"~ #\", shell_cmd = \"pwd\" }]\nsuccess_regex = \
         []\nfail_regex = []\n",
    );
}

/// A suite root with an unexpected config error must not be hidden behind a
/// sibling root that already produced cases, which would silently shrink the
/// discovered coverage.
#[test]
fn unexpected_root_error_is_not_swallowed_by_a_healthy_root() {
    let root = tempdir().unwrap();
    write_healthy_aarch64_qemu_case(root.path());

    // The migrated root ships a legacy `build-<arch>.toml` instead of the
    // required `build-<target>.toml`, so scanning it is an unexpected config
    // error rather than an empty selection.
    let faulty_wrapper = root.path().join(MIGRATED_SUITE_ROOT).join("normal/legacy");
    fs::create_dir_all(&faulty_wrapper).unwrap();
    fs::write(
        faulty_wrapper.join("build-aarch64.toml"),
        "target = \"aarch64-unknown-none-softfloat\"\n",
    )
    .unwrap();

    // The error propagates with and without a selection that the healthy root
    // can satisfy.
    for selected_case in [None, Some("smoke")] {
        let err = discover_qemu_cases(
            root.path(),
            "normal",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            selected_case,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unsupported legacy build config"),
            "{err}"
        );
    }

    // Dropping the faulty wrapper proves the error came from it and that the
    // healthy root discovers its case on its own.
    fs::remove_dir_all(&faulty_wrapper).unwrap();
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
}

/// A selected case that no suite root provides still fails instead of quietly
/// returning an empty set.
#[test]
fn selected_case_missing_from_every_suite_root_is_reported() {
    let root = tempdir().unwrap();
    write_healthy_aarch64_qemu_case(root.path());
    write_qemu_build_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress",
        "gicv3-timer-stress",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \"axvisor:/$\", shell_cmd = \"vm console 1\" \
         }]\nsuccess_regex = [\"AXVISOR_GICV3_TIMER_STRESS_PASSED\"]\nfail_regex = []\n",
    );

    let err = discover_qemu_cases(
        root.path(),
        "normal",
        "aarch64",
        "aarch64-unknown-none-softfloat",
        Some("ghost"),
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown"), "{err}");
    assert!(err.to_string().contains("ghost"), "{err}");
}

/// Listing merges the functional and nightly roots and tolerates the root that
/// lacks the selected case.
#[test]
fn lists_qemu_cases_from_every_suite_root_and_tolerates_missing_selection() {
    let root = tempdir().unwrap();
    write_healthy_aarch64_qemu_case(root.path());
    write_qemu_build_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress",
        "aarch64-unknown-none-softfloat",
    );
    write_qemu_config_in_suite_root(
        root.path(),
        MIGRATED_SUITE_ROOT,
        "normal",
        "qemu-timer-stress",
        "gicv3-timer-stress",
        "aarch64",
        "shell_check_steps = [{ shell_prefix = \"axvisor:/$\", shell_cmd = \"vm console 1\" \
         }]\nsuccess_regex = [\"AXVISOR_GICV3_TIMER_STRESS_PASSED\"]\nfail_regex = []\n",
    );

    let listed = discovery::list_all_qemu_cases(root.path(), "normal", None).unwrap();
    assert_eq!(
        listed,
        vec!["smoke".to_string(), "gicv3-timer-stress".to_string()]
    );

    // The functional root lacks the nightly case; that per-root miss stays
    // ignorable while the migrated root provides it.
    let selected =
        discovery::list_all_qemu_cases(root.path(), "normal", Some("gicv3-timer-stress")).unwrap();
    assert_eq!(selected, vec!["gicv3-timer-stress".to_string()]);
}

/// Only a missing group or a missing selected case is ignorable per root; an
/// unexpected error must surface so listing never hides part of the tree.
#[test]
fn list_qemu_case_error_classification_never_ignores_unexpected() {
    use crate::test::qemu::ListQemuCasesErrorKind;

    assert!(discovery::qemu_list_error_is_ignorable(
        ListQemuCasesErrorKind::EmptyGroup
    ));
    assert!(discovery::qemu_list_error_is_ignorable(
        ListQemuCasesErrorKind::UnknownSelectedCase
    ));
    assert!(!discovery::qemu_list_error_is_ignorable(
        ListQemuCasesErrorKind::Unexpected
    ));
}
