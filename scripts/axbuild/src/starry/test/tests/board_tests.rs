use super::*;

#[test]
fn discovers_board_test_group_and_build_mapping() {
    let root = tempdir().unwrap();
    let build_config = write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    let board_test_config =
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let groups = discover_board_test_groups(root.path(), None, None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "orangepi-5-plus");
    assert_eq!(groups[0].arch, "aarch64");
    assert_eq!(groups[0].target, "aarch64-unknown-none-softfloat");
    assert_eq!(groups[0].build_config_path, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn discovers_board_case_when_case_dir_contains_build_config() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("test-suit/starryos/smoke");
    fs::create_dir_all(&case_dir).unwrap();
    let build_config = case_dir.join("build-aarch64-unknown-none-softfloat.toml");
    fs::write(
        &build_config,
        "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = [\"qemu\"]\nlog = \
         \"Info\"\n",
    )
    .unwrap();
    let board_test_config = case_dir.join("board-orangepi-5-plus.toml");
    fs::write(
        &board_test_config,
        "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \
         \"orangepi@orangepi5plus:~\"\nshell_init_cmd = \"pwd && echo 'test \
         pass'\"\nsuccess_regex = [\"(?m)^test pass\\\\s*$\"]\nfail_regex = []\ntimeout = 300\n",
    )
    .unwrap();

    let groups = discover_board_test_groups(root.path(), None, None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "orangepi-5-plus");
    assert_eq!(groups[0].build_config_path, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn filters_board_test_group_by_case() {
    let root = tempdir().unwrap();
    write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_starry_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");
    write_board_test_config(root.path(), "vision-five2", "smoke", "vision-five2");

    let groups = discover_board_test_groups(root.path(), Some("smoke"), None).unwrap();

    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups
            .iter()
            .map(|group| format!("{}/{}", group.name, group.board_name))
            .collect::<Vec<_>>(),
        vec!["smoke/orangepi-5-plus", "smoke/vision-five2"]
    );
}

#[test]
fn filters_board_test_groups_by_board() {
    let root = tempdir().unwrap();
    write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_starry_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");
    write_board_test_config(root.path(), "orangepi-5-plus", "syscall", "orangepi-5-plus");
    write_board_test_config(root.path(), "vision-five2", "smoke", "vision-five2");

    let groups = discover_board_test_groups(root.path(), None, Some("orangepi-5-plus")).unwrap();

    assert_eq!(
        groups
            .iter()
            .map(|group| format!("{}/{}", group.name, group.board_name))
            .collect::<Vec<_>>(),
        vec!["smoke/orangepi-5-plus", "syscall/orangepi-5-plus"]
    );
}

#[test]
fn rejects_unknown_board_test_board() {
    let root = tempdir().unwrap();
    write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let err = discover_board_test_groups(root.path(), None, Some("unknown")).unwrap_err();

    assert!(
        err.to_string()
            .contains("unsupported Starry board test board `unknown`")
    );
    assert!(err.to_string().contains("orangepi-5-plus"));
}

#[test]
fn rejects_missing_mapped_board_build_config() {
    let root = tempdir().unwrap();
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let err = discover_board_test_groups(root.path(), None, None)
        .unwrap_err()
        .to_string();

    assert!(err.contains("not under a build wrapper"));
    assert!(err.contains("smoke"));
}
