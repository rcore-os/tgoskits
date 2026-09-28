use super::*;

#[test]
fn grouped_qemu_profiles_partition_execution_and_preserve_direct_selection() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    let dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(dir.join("timing/c")).unwrap();
    fs::create_dir_all(dir.join("counter-read/c")).unwrap();
    fs::create_dir_all(dir.join("counter-ring/c")).unwrap();
    fs::write(
        dir.join("qemu-x86_64.toml"),
        r#"
test_commands = ["exec runner"]
[[grouped_qemu_profiles]]
name = "counters"
subcase_prefix = "counter-"
config = "counters.toml"
"#,
    )
    .unwrap();
    fs::write(
        dir.join("counters.toml"),
        "test_commands = [\"exec runner\"]\n",
    )
    .unwrap();
    let cases = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(
        cases[0].case.grouped_subcase_filter,
        Some(BTreeSet::from(["timing".into()]))
    );
    assert_eq!(
        cases[1].case.grouped_subcase_filter,
        Some(BTreeSet::from([
            "counter-read".into(),
            "counter-ring".into()
        ]))
    );
    assert_eq!(cases[1].case.qemu_config_path, dir.join("counters.toml"));
    assert_ne!(cases[0].case.name, cases[1].case.name);
    let direct = discover_qemu_cases(
        root.path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("qemu/system/counter-read"),
    )
    .unwrap();
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].case.qemu_config_path, dir.join("counters.toml"));
    assert_eq!(
        direct[0].case.grouped_subcase_filter,
        Some(BTreeSet::from(["counter-read".into()]))
    );
    fs::write(dir.join("counters.toml"), "test_commands = []\n").unwrap();
    assert!(discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).is_err());
}

#[test]
fn grouped_qemu_profiles_reject_overlap_and_unmatched_configuration() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    let dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(dir.join("counter-read/c")).unwrap();
    fs::write(
        dir.join("profile.toml"),
        "test_commands = [\"exec runner\"]\n",
    )
    .unwrap();
    for (prefix, expected) in [
        ("counter-", "multiple grouped QEMU profiles"),
        ("missing-", "matches no subcases"),
    ] {
        fs::write(
            dir.join("qemu-x86_64.toml"),
            format!(
                r#"
test_commands = ["exec runner"]
[[grouped_qemu_profiles]]
name = "first"
subcase_prefix = "counter-"
config = "profile.toml"
[[grouped_qemu_profiles]]
name = "second"
subcase_prefix = "{prefix}"
config = "profile.toml"
"#
            ),
        )
        .unwrap();
        let error =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[test]
fn discovers_only_cases_with_matching_qemu_config() {
    let root = tempdir().unwrap();
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    write_qemu_test_config(root.path(), "normal", "default", "smoke", "x86_64");
    fs::create_dir_all(root.path().join("test-suit/starryos/default/usb")).unwrap();

    let cases = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

    assert_eq!(cases[0].case.name, "smoke");
    assert!(cases[0].case.test_commands.is_empty());
    assert!(cases[0].case.subcases.is_empty());
    assert_eq!(
        cases[0].case.case_dir,
        root.path().join("test-suit/starryos/default/smoke")
    );
}

#[test]
fn discovers_grouped_case_commands_and_sorted_subcases() {
    let root = tempdir().unwrap();
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    write_grouped_qemu_test_config(root.path(), "normal", "default", "bugfix", "x86_64");
    fs::create_dir_all(root.path().join("test-suit/starryos/default/bugfix/beta/c")).unwrap();
    fs::create_dir_all(
        root.path()
            .join("test-suit/starryos/default/bugfix/alpha/c"),
    )
    .unwrap();

    let cases = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

    assert_eq!(cases[0].case.name, "bugfix");
    assert_eq!(
        cases[0].case.test_commands,
        vec!["/usr/bin/beta".to_string(), "/usr/bin/alpha".to_string()]
    );
    let subcase_names = cases[0]
        .case
        .subcases
        .iter()
        .map(|subcase| subcase.name.as_str())
        .collect::<Vec<_>>();
    assert!(subcase_names.contains(&"alpha"));
    assert!(subcase_names.contains(&"beta"));
    assert!(subcase_names.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(
        cases[0]
            .case
            .subcases
            .iter()
            .all(|subcase| subcase.kind == TestQemuSubcaseKind::C)
    );
}

#[test]
fn starry_qemu_subcase_selector_maps_to_system_parent() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    let case_dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(case_dir.join("alpha/src")).unwrap();
    fs::write(
        case_dir.join("alpha/CMakeLists.txt"),
        "add_executable(alpha src/main.c)\n",
    )
    .unwrap();

    let cases = discover_qemu_cases(
        root.path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("qemu/alpha"),
    )
    .unwrap();

    assert_eq!(cases[0].case.display_name, "qemu/system");
    assert_eq!(
        cases[0].case.grouped_subcase_filter,
        Some(BTreeSet::from(["alpha".to_string()]))
    );
}

#[test]
fn starry_qemu_ltp_selection_uses_architecture_manifest() {
    let root = tempdir().unwrap();
    let ltp = root
        .path()
        .join("test-suit/starryos/qemu/system/ltp-syscalls");
    fs::create_dir_all(&ltp).unwrap();
    fs::write(ltp.join("CMakeLists.txt"), "project(ltp-syscalls C)\n").unwrap();
    fs::write(ltp.join("cases.txt"), "common01\n").unwrap();
    fs::write(ltp.join("cases-x86_64.txt"), "arch01\n").unwrap();

    for arch in ["x86_64", "aarch64"] {
        let target = format!("{arch}-unknown-none");
        write_flat_qemu_build_config(root.path(), "qemu", &target);
        write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", arch);
        let select = |id: &str| {
            discover_qemu_cases(
                root.path(),
                arch,
                &target,
                Some(&format!("qemu/system/ltp-syscalls/{id}")),
            )
        };

        assert_eq!(select("common01").unwrap().len(), 1);
        if arch == "x86_64" {
            assert_eq!(select("arch01").unwrap().len(), 1);
        } else {
            assert!(select("arch01").is_err());
        }
        assert!(select("missing01").is_err());
        assert!(select("../common01").is_err());
    }
}

#[test]
fn starry_qemu_subcase_selector_reports_unknown_subcase() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    let case_dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(case_dir.join("alpha/src")).unwrap();
    fs::write(
        case_dir.join("alpha/CMakeLists.txt"),
        "add_executable(alpha src/main.c)\n",
    )
    .unwrap();

    let err = discover_qemu_cases(
        root.path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("qemu/missing"),
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("qemu/system"));
    assert!(err.contains("missing"));
}

#[test]
fn grouped_case_skips_arch_specific_subcases_for_other_arches() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "default",
        "riscv64gc-unknown-none-elf",
    );
    write_grouped_qemu_test_config(root.path(), "normal", "default", "syscall", "riscv64");

    let case_dir = root.path().join("test-suit/starryos/default/syscall");
    fs::create_dir_all(case_dir.join("alpha/c")).unwrap();
    fs::create_dir_all(case_dir.join("x86-only/c")).unwrap();
    fs::write(case_dir.join("x86-only/qemu-x86_64.toml"), "timeout = 1\n").unwrap();

    let cases =
        discover_qemu_cases(root.path(), "riscv64", "riscv64gc-unknown-none-elf", None).unwrap();

    assert!(
        cases[0]
            .case
            .subcases
            .iter()
            .any(|subcase| subcase.name == "alpha")
    );
}

#[test]
fn grouped_case_rejects_empty_test_command() {
    let root = tempdir().unwrap();
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    let path = root
        .path()
        .join("test-suit/starryos/default/bugfix/qemu-x86_64.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "test_commands = [\"/usr/bin/ok\", \"  \"]\n").unwrap();

    let err = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("bugfix"))
        .unwrap_err()
        .to_string();

    assert!(err.contains("contains an empty test command"));
}

#[test]
fn selected_case_requires_matching_qemu_config() {
    let root = tempdir().unwrap();
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    fs::create_dir_all(root.path().join("test-suit/starryos/default/usb")).unwrap();

    let err = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("usb"))
        .unwrap_err()
        .to_string();

    assert!(err.contains("none provide `qemu-x86_64.toml`"));
    assert!(err.contains("qemu-x86_64.toml"));
}

#[test]
fn selected_qemu_case_skips_non_qemu_case_with_same_name() {
    let root = tempdir().unwrap();
    write_qemu_build_config(
        root.path(),
        "normal",
        "board-orangepi-5-plus",
        "x86_64-unknown-none",
    );
    write_qemu_build_config(root.path(), "normal", "qemu", "x86_64-unknown-none");
    fs::create_dir_all(
        root.path()
            .join("test-suit/starryos/board-orangepi-5-plus/smoke"),
    )
    .unwrap();
    fs::write(
        root.path()
            .join("test-suit/starryos/board-orangepi-5-plus/smoke/board-orangepi-5-plus.toml"),
        "board_type = \"OrangePi-5-Plus\"\n",
    )
    .unwrap();
    write_qemu_test_config(root.path(), "normal", "qemu", "smoke", "x86_64");

    let cases =
        discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("smoke")).unwrap();

    assert_eq!(cases[0].build_group, "qemu");
    assert_eq!(cases[0].case.name, "smoke");
}
