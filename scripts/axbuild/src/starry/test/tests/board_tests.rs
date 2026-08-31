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

#[test]
fn sg2002_repository_dtbs_declare_noncoherent_dma() {
    // SG2002 peripherals are DMA non-coherent: mainline Linux declares
    // dma-noncoherent on the sg2002 soc node, while the vendor SDK device
    // trees never do. The kernel resolves coherency from firmware, so a
    // regenerated DTB that silently drops the property would make CV181x
    // engines read stale cached descriptors. Property names live in the
    // compiled DTB strings block, so a byte-level search is sufficient.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for dtb in [
        "os/StarryOS/configs/board/aka-00-sg2002.dtb",
        "os/StarryOS/configs/board/licheerv-nano-sg2002.dtb",
    ] {
        let path = workspace_root.join(dtb);
        let bytes = fs::read(&path)
            .unwrap_or_else(|err| panic!("failed to read repository DTB {dtb}: {err}"));
        assert!(
            bytes
                .windows(b"dma-noncoherent\0".len())
                .any(|window| window == b"dma-noncoherent\0"),
            "{dtb} must declare dma-noncoherent; SG2002 devices require it"
        );
    }
}

#[test]
fn sg2002_board_cases_select_the_repository_dtb() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let groups = discover_board_test_groups(&workspace_root, None, Some("aka-00-sg2002"))
        .expect("repository SG2002 board cases must be discoverable");
    let expected_dtb = "os/StarryOS/configs/board/aka-00-sg2002.dtb";

    let group_names = groups
        .iter()
        .map(|group| group.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        group_names,
        ["boot", "tennis-yolo", "usb2-lsusb", "wifi-iperf-smoke"]
            .into_iter()
            .collect(),
        "all public SG2002 board cases must run"
    );
    for group in &groups {
        let config = fs::read_to_string(&group.board_test_config_path).unwrap_or_else(|err| {
            panic!(
                "failed to read {}: {err}",
                group.board_test_config_path.display()
            )
        });
        let config: toml::Value = toml::from_str(&config).unwrap_or_else(|err| {
            panic!(
                "failed to parse {}: {err}",
                group.board_test_config_path.display()
            )
        });

        assert_eq!(
            config.get("dtb_file").and_then(toml::Value::as_str),
            Some(expected_dtb),
            "{}/{} must use the repository DTB",
            group.name,
            group.board_name
        );
    }
}
