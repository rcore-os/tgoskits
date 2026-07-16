use super::*;

#[test]
fn starry_grouped_cases_install_profile_autorun() {
    let config = starry_case_asset_config();

    assert_eq!(
        config.grouped_runner.autorun_profile_script.as_deref(),
        Some("99-starry-run-case-tests.sh")
    );
}

#[test]
fn discovers_only_cases_with_matching_qemu_config() {
    let root = tempdir().unwrap();
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    write_qemu_test_config(root.path(), "normal", "default", "smoke", "x86_64");
    fs::create_dir_all(root.path().join("test-suit/starryos/default/usb")).unwrap();

    let cases = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

    assert_eq!(cases.len(), 1);
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

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.name, "bugfix");
    assert_eq!(
        cases[0].case.test_commands,
        vec!["/usr/bin/beta".to_string(), "/usr/bin/alpha".to_string()]
    );
    assert_eq!(
        cases[0]
            .case
            .subcases
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    assert!(
        cases[0]
            .case
            .subcases
            .iter()
            .all(|subcase| subcase.kind == TestQemuSubcaseKind::C)
    );
}

#[test]
fn discovers_flat_qemu_wrapper_case_with_subcases() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    let case_dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(case_dir.join("smoke/c")).unwrap();
    fs::create_dir_all(case_dir.join("usb-storage/c")).unwrap();

    let cases = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.name, "system");
    assert_eq!(cases[0].case.display_name, "qemu/system");
    assert_eq!(cases[0].build_group, "qemu");
    assert_eq!(
        cases[0]
            .case
            .subcases
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["smoke", "usb-storage"]
    );

    let selected = discover_qemu_cases(
        root.path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("qemu/system"),
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].case.display_name, "qemu/system");

    let listed = discover_all_qemu_cases_with_archs(root.path(), None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "qemu/system");
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

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.display_name, "qemu/system");
    assert_eq!(
        cases[0].case.grouped_subcase_filter,
        Some(BTreeSet::from(["alpha".to_string()]))
    );
}

#[test]
fn starry_qemu_subcase_selector_accepts_installed_binary_name() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    let case_dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(case_dir.join("syscall-test-uid-gid-re-setters/src")).unwrap();
    fs::write(
        case_dir.join("syscall-test-uid-gid-re-setters/CMakeLists.txt"),
        r#"
add_executable(test-uid-gid-re-setters src/main.c)
install(TARGETS test-uid-gid-re-setters RUNTIME DESTINATION usr/bin/starry-test-suit)
"#,
    )
    .unwrap();

    let cases = discover_qemu_cases(
        root.path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("qemu/test-uid-gid-re-setters"),
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.display_name, "qemu/system");
    assert_eq!(
        cases[0].case.grouped_subcase_filter,
        Some(BTreeSet::from([
            "syscall-test-uid-gid-re-setters".to_string()
        ]))
    );
}

#[test]
fn starry_qemu_system_subcase_selector_sets_filter() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    let case_dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(case_dir.join("test-futex-race/src")).unwrap();
    fs::write(
        case_dir.join("test-futex-race/CMakeLists.txt"),
        "add_executable(test-futex-race src/main.c)\n",
    )
    .unwrap();

    let cases = discover_qemu_cases(
        root.path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("qemu/system/test-futex-race"),
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.display_name, "qemu/system");
    assert_eq!(
        cases[0].case.grouped_subcase_filter,
        Some(BTreeSet::from(["test-futex-race".to_string()]))
    );
}

#[test]
fn starry_qemu_system_selector_keeps_full_group() {
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
        Some("qemu/system"),
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.display_name, "qemu/system");
    assert_eq!(cases[0].case.grouped_subcase_filter, None);
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
fn starry_qemu_subcase_selector_prefers_existing_direct_case() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    write_qemu_test_config(root.path(), "normal", "qemu", "alpha", "x86_64");
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

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.display_name, "qemu/alpha");
    assert_eq!(cases[0].case.grouped_subcase_filter, None);
}

#[test]
fn starry_qemu_list_accepts_subcase_selector() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");

    let listed = discover_all_qemu_cases_with_archs(root.path(), Some("qemu/alpha")).unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "qemu/system");
}

#[test]
fn starry_qemu_list_prefers_existing_direct_case() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    write_qemu_test_config(root.path(), "normal", "qemu", "alpha", "x86_64");

    let listed = discover_all_qemu_cases_with_archs(root.path(), Some("qemu/alpha")).unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "qemu/alpha");
}

#[test]
fn discovers_flat_qemu_wrapper_case_with_root_cmake_subcases() {
    let root = tempdir().unwrap();
    write_flat_qemu_build_config(root.path(), "qemu", "x86_64-unknown-none");
    write_flat_grouped_qemu_test_config(root.path(), "qemu", "system", "x86_64");
    let case_dir = root.path().join("test-suit/starryos/qemu/system");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(
        case_dir.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.20)\nproject(system C)\nadd_subdirectory(smoke)\n",
    )
    .unwrap();
    fs::create_dir_all(case_dir.join("smoke/src")).unwrap();
    fs::write(
        case_dir.join("smoke/CMakeLists.txt"),
        "add_executable(smoke src/main.c)\n",
    )
    .unwrap();

    let cases = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(
        cases[0]
            .case
            .subcases
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["smoke"]
    );
    assert!(
        cases[0]
            .case
            .subcases
            .iter()
            .all(|subcase| subcase.kind == TestQemuSubcaseKind::C)
    );
}

#[test]
fn starry_system_grouped_cases_use_root_cmake_layout() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu/system");

    let root_cmake = system_dir.join("CMakeLists.txt");
    assert!(
        root_cmake.is_file(),
        "{} must be the grouped system CMake project entry",
        root_cmake.display()
    );

    let mut subcase_count = 0;
    for entry in fs::read_dir(&system_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if !path.is_dir()
            || path.file_name().is_some_and(|name| name == "common")
            || !path.join("CMakeLists.txt").is_file()
        {
            continue;
        }
        subcase_count += 1;
        assert!(
            !path.join("c").exists(),
            "{} must keep CMakeLists.txt and src/ directly under the subcase",
            path.display()
        );
        assert!(
            path.join("src").is_dir() || path.join("CMakeLists.txt").is_file(),
            "{} must remain a buildable subcase directory",
            path.display()
        );
    }

    assert!(
        subcase_count > 0,
        "{} must contain grouped C subcases",
        system_dir.display()
    );
}

#[test]
fn nix_sandbox_debug_x86_64_uses_explicit_uefi_boot() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config_path =
        workspace_root.join("test-suit/starryos/qemu/nix-sandbox-debug/qemu-x86_64.toml");
    let config: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();

    assert_eq!(
        config.get("uefi").and_then(toml::Value::as_bool),
        Some(true),
        "{} must explicitly use UEFI because CI tests the PR merge ref where x86_64 dynamic boot \
         is no longer inferred",
        config_path.display()
    );
    assert_eq!(
        config.get("to_bin").and_then(toml::Value::as_bool),
        Some(true),
        "{} must explicitly boot the raw binary artifact under UEFI",
        config_path.display()
    );
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

    assert_eq!(cases.len(), 1);
    assert_eq!(
        cases[0]
            .case
            .subcases
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha"]
    );
}

#[test]
fn grouped_case_loads_with_both_shell_init_cmd_and_test_commands_present() {
    // The mutual-exclusion check has been moved from the initial TOML parse
    // (discover_qemu_cases) to prepare_qemu_cases so we only read each
    // file once.  Therefore, discovery itself should succeed here; the
    // conflict is detected later when QemuConfig is available.
    let root = tempdir().unwrap();
    write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
    let path = root
        .path()
        .join("test-suit/starryos/default/bugfix/qemu-x86_64.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "shell_prefix = \"root@starry:\"\nshell_init_cmd = \"/usr/bin/old\"\ntest_commands = \
         [\"/usr/bin/new\"]\n",
    )
    .unwrap();

    // Discovery no longer validates the shell_init_cmd / test_commands
    // conflict; it should succeed and leave a grouped case behind.
    let cases =
        discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("bugfix")).unwrap();
    assert_eq!(cases.len(), 1);
    assert!(!cases[0].case.test_commands.is_empty());
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

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].build_group, "qemu");
    assert_eq!(cases[0].case.name, "smoke");
}
