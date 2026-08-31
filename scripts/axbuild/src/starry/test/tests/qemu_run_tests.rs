use super::*;

#[test]
fn qemu_case_requirements_read_smp_from_case_config() {
    let qemu = QemuConfig {
        args: vec![
            "-nographic".to_string(),
            "-smp".to_string(),
            "cpus=4".to_string(),
        ],
        ..Default::default()
    };

    let requirements = Starry::qemu_case_requirements(&qemu).unwrap();

    assert_eq!(requirements, StarryQemuCaseRequirements { smp: 4 });
}

#[test]
fn qemu_case_requirements_default_to_single_cpu() {
    let qemu = QemuConfig::default();

    let requirements = Starry::qemu_case_requirements(&qemu).unwrap();

    assert_eq!(requirements, StarryQemuCaseRequirements { smp: 1 });
}

#[test]
fn qemu_case_rootfs_uses_drive_file_arg() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let managed_rootfs = root.path().join(".tgos-images/rootfs-riscv64-debian.img");
    let qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "/tmp/not-disk0.img".to_string(),
            "-drive".to_string(),
            format!(
                "id=disk0,if=none,format=raw,file={}",
                managed_rootfs.display()
            ),
        ],
        ..Default::default()
    };

    let rootfs =
        Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img")).unwrap();

    assert_eq!(rootfs, managed_rootfs);
}

#[test]
fn qemu_case_rootfs_accepts_drive_file_with_additional_options() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let managed_rootfs = root.path().join(".tgos-images/rootfs-aarch64-busybox.img");
    let qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            format!(
                "id=usbdisk,if=none,format=raw,snapshot=on,file={}",
                managed_rootfs.display()
            ),
        ],
        ..Default::default()
    };

    let rootfs =
        Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img")).unwrap();

    assert_eq!(rootfs, managed_rootfs);
}

#[test]
fn qemu_case_rootfs_collects_all_managed_drive_files() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let boot_rootfs = root.path().join(".tgos-images/rootfs-aarch64-alpine.img");
    let usb_rootfs = root.path().join(".tgos-images/rootfs-aarch64-busybox.img");
    let qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={}", boot_rootfs.display()),
            "-drive".to_string(),
            format!(
                "id=usbdisk,if=none,format=raw,snapshot=on,file={}",
                usb_rootfs.display()
            ),
        ],
        ..Default::default()
    };

    let rootfs_paths = Starry::qemu_case_managed_rootfs_paths(root.path(), &qemu).unwrap();

    assert!(rootfs_paths.contains(&boot_rootfs));
    assert!(rootfs_paths.contains(&usb_rootfs));
}

#[test]
fn qemu_case_rewrites_default_rootfs_references() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let image_name = "rootfs-aarch64-busybox.img";
    let default_rootfs = root.path().join("tmp/axbuild/rootfs").join(image_name);
    let managed_rootfs = root.path().join(".tgos-images").join(image_name);
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            format!(
                "id=usbdisk,if=none,format=raw,snapshot=on,file={}",
                default_rootfs.display()
            ),
        ],
        ..Default::default()
    };

    Starry::rewrite_qemu_case_managed_rootfs_paths(root.path(), &mut qemu).unwrap();

    assert!(
        qemu.args
            .iter()
            .any(|arg| arg.contains(&managed_rootfs.display().to_string()))
    );
    assert!(
        !qemu
            .args
            .iter()
            .any(|arg| arg.contains(&default_rootfs.display().to_string()))
    );
    assert!(
        Starry::qemu_case_managed_rootfs_paths(root.path(), &qemu)
            .unwrap()
            .contains(&managed_rootfs)
    );
}

#[test]
fn qemu_case_rootfs_ignores_non_managed_drive_file_arg() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            format!(
                "id=disk0,if=none,format=raw,file={}",
                root.path()
                    .join("target/x86_64-unknown-none/rootfs-x86_64.img")
                    .display()
            ),
        ],
        ..Default::default()
    };

    let rootfs =
        Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img")).unwrap();

    assert_eq!(rootfs, PathBuf::from("/tmp/default.img"));
}

#[test]
fn qemu_case_rootfs_defaults_without_drive_file_arg() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let qemu = QemuConfig::default();

    let rootfs =
        Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img")).unwrap();

    assert_eq!(rootfs, PathBuf::from("/tmp/default.img"));
}

#[test]
fn qemu_cases_are_grouped_by_build_config() {
    let default_build_config = PathBuf::from("/tmp/default/build-x86_64-unknown-none.toml");
    let qemu_build_config = PathBuf::from("/tmp/qemu/build-x86_64-unknown-none.toml");
    let cases = vec![
        prepared_qemu_case("smoke", default_build_config.clone()),
        prepared_qemu_case("qemu/system", qemu_build_config.clone()),
        prepared_qemu_case("syscall", default_build_config.clone()),
    ];

    let groups = qemu_test::group_cases_by_build_config(&cases);

    let default_group = groups
        .iter()
        .find(|group| group.build_config_path == default_build_config)
        .expect("default build config group");
    assert!(
        default_group
            .cases
            .iter()
            .any(|case| case.case.name == "smoke")
    );
    assert!(
        default_group
            .cases
            .iter()
            .any(|case| case.case.name == "syscall")
    );
    let qemu_group = groups
        .iter()
        .find(|group| group.build_config_path == qemu_build_config)
        .expect("qemu build config group");
    assert!(
        qemu_group
            .cases
            .iter()
            .any(|case| case.case.name == "qemu/system")
    );
}

#[test]
fn qemu_test_request_ignores_inherited_smp() {
    let mut request = starry_request(
        PathBuf::from("/tmp/build-riscv64gc-unknown-none-elf.toml"),
        "riscv64",
        "riscv64gc-unknown-none-elf",
    );
    request.smp = Some(1);

    let request = Starry::qemu_test_request(request);

    assert_eq!(request.smp, None);
}

#[test]
fn qemu_group_build_context_uses_group_build_config_over_default_override() {
    let root = tempdir().unwrap();
    let build_config = write_qemu_build_config_with_max_cpu_num(
        root.path(),
        "normal",
        "qemu",
        "x86_64-unknown-none",
        4,
    );
    let mut request = starry_request(
        PathBuf::from("/tmp/default-build.toml"),
        "x86_64",
        "x86_64-unknown-none",
    );
    request.build_info_override = Some(crate::starry::build::StarryBuildInfo {
        max_cpu_num: Some(1),
        ..crate::starry::build::default_starry_build_info()
    });

    let (_group_request, cargo) =
        Starry::qemu_group_build_context(&request, &build_config).unwrap();

    assert_eq!(cargo.env.get("SMP").map(String::as_str), Some("4"));
    assert!(cargo.features.contains(&"smp".to_string()));
}

#[test]
fn qemu_group_build_context_uses_dynamic_group_platform_over_default_request() {
    let root = tempdir().unwrap();
    let build_config = root
        .path()
        .join("test-suit/starryos/qemu/build-aarch64-unknown-none-softfloat.toml");
    fs::create_dir_all(build_config.parent().unwrap()).unwrap();
    fs::write(
        &build_config,
        "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = [\"qemu\"]\nlog = \
         \"Warn\"\n",
    )
    .unwrap();
    let mut request = starry_request(
        PathBuf::from("/tmp/default-build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    request.build_info_override = Some(crate::starry::build::StarryBuildInfo {
        features: vec!["qemu".to_string()],
        ..crate::starry::build::default_starry_build_info()
    });

    let (_group_request, cargo) =
        Starry::qemu_group_build_context(&request, &build_config).unwrap();

    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(
        !cargo
            .features
            .contains(&"starry-kernel/plat-dyn".to_string())
    );
    assert!(cargo.features.contains(&"qemu".to_string()));
    assert_eq!(
        cargo.target,
        "scripts/targets/bare/aarch64-unknown-none-softfloat.json"
    );
    assert_eq!(
        cargo.env.get("AX_TARGET").map(String::as_str),
        Some("aarch64-unknown-none-softfloat")
    );
    assert!(
        cargo
            .args
            .windows(2)
            .any(|pair| pair == ["-Z", "json-target-spec"])
    );
    assert!(
        cargo
            .args
            .windows(2)
            .any(|pair| pair == ["-Z", "build-std=core,alloc"])
    );
}

#[test]
fn board_test_group_prefers_case_target_build_config() {
    let root = tempdir().unwrap();
    let build = write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let groups = discover_board_test_groups(root.path(), None, None).unwrap();

    assert!(groups.iter().any(|group| group.build_config_path == build));
}

#[test]
fn board_test_group_rejects_legacy_case_build_config() {
    let root = tempdir().unwrap();
    write_board_test_config(root.path(), "smoke", "smoke", "orangepi-5-plus");
    let legacy = root
        .path()
        .join("test-suit/starryos/smoke/.build-aarch64-unknown-none-softfloat.toml");
    fs::write(&legacy, "").unwrap();

    let err = discover_board_test_groups(root.path(), None, None)
        .unwrap_err()
        .to_string();

    assert!(err.contains("not under a build wrapper"));
}

#[test]
fn board_test_group_falls_back_to_mapped_board_build_config() {
    let root = tempdir().unwrap();
    let build = write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let groups = discover_board_test_groups(root.path(), None, None).unwrap();

    assert!(groups.iter().any(|group| group.build_config_path == build));
}
