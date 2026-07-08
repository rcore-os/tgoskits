use super::*;

#[test]
fn apk_add_fs_equivalence_qemu_case_covers_package_fs_ops() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("apps/starry/qemu/apk-add-fs-equivalence");
    let cmake_path = case_dir.join("c/CMakeLists.txt");
    let source_path = case_dir.join("c/src/main.c");

    assert!(
        cmake_path.is_file(),
        "{} must build the filesystem equivalence probe through the C pipeline",
        cmake_path.display()
    );
    assert!(
        source_path.is_file(),
        "{} must contain the filesystem equivalence probe source",
        source_path.display()
    );
    assert!(
        !case_dir.join("sh").exists() && !case_dir.join("python").exists(),
        "{} must stay a single C pipeline case",
        case_dir.display()
    );

    let source = fs::read_to_string(&source_path).unwrap();
    for forbidden in [
        "apk update",
        "apk add",
        "apt update",
        "apt install",
        "curl ",
    ] {
        assert!(
            !source.contains(forbidden),
            "{} must not depend on package managers or network clients",
            source_path.display()
        );
    }
    for forbidden in ["http://", "https://"] {
        assert!(
            !source.contains(forbidden),
            "{} must not access external network resources",
            source_path.display()
        );
    }
    for required in [
        "mkdir(",
        "mkdirat(",
        "stat(",
        "lstat(",
        "fstatat(",
        "opendir(",
        "readdir(",
        "open(",
        "O_CREAT",
        "O_TRUNC",
        "O_EXCL",
        "write(",
        "read(",
        "pread(",
        "pwrite(",
        "payload_checksum_update(",
        "rename(",
        "unlink(",
        "chmod(",
        "fchmod(",
        "chown(",
        "fchown(",
        "lchown(",
        "truncate(",
        "ftruncate(",
        "utimensat(",
        "symlink(",
        "readlink(",
        "link(",
        "fsync(",
        "fdatasync(",
        "sync()",
        "syncfs(",
        "APK_ADD_FS_EQUIV_LARGE_PAYLOAD_WRITE_BYTES",
        "APK_ADD_FS_EQUIV_LARGE_PAYLOAD_READ_BYTES",
        "read_checksum == write_checksum",
        "APK_ADD_FS_EQUIV_TEST_PASSED",
        "APK_ADD_FS_EQUIV_TEST_FAILED",
    ] {
        assert!(
            source.contains(required),
            "{} must cover `{required}`",
            source_path.display()
        );
    }
    for simulated_path in ["/usr/bin", "/usr/lib", "/lib/apk/db", "/var/lib/dpkg"] {
        assert!(
            source.contains(simulated_path),
            "{} must simulate package install path `{simulated_path}`",
            source_path.display()
        );
    }

    let cmake = fs::read_to_string(&cmake_path).unwrap();
    assert!(
        cmake.contains("project(apk-add-fs-equivalence C)")
            && cmake.contains("add_executable(apk-add-fs-equivalence")
            && cmake.contains("install(TARGETS apk-add-fs-equivalence")
            && cmake.contains("DESTINATION usr/bin"),
        "{} must install the C probe into the guest image",
        cmake_path.display()
    );

    for arch in ["x86_64", "riscv64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        assert!(
            config_path.is_file(),
            "{} must exist after local validation for {arch}",
            config_path.display()
        );
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let args = config.get("args").and_then(toml::Value::as_array).unwrap();
        let args = args
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();

        assert!(
            args.iter()
                .any(|arg| arg.contains("virtio-blk-pci,drive=disk0")),
            "{} must exercise virtio-blk",
            config_path.display()
        );
        assert!(
            args.iter().any(|arg| {
                arg.contains(&format!("rootfs-{arch}-alpine.img")) && arg.contains(".tgos-images")
            }),
            "{} must use the managed Alpine rootfs for {arch}",
            config_path.display()
        );

        assert_eq!(
            config
                .get("shell_init_cmd")
                .and_then(toml::Value::as_str)
                .unwrap(),
            "/usr/bin/apk-add-fs-equivalence"
        );
        let success_regex = config
            .get("success_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            success_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("APK_ADD_FS_EQUIV_TEST_PASSED")),
            "{} must require the pass marker",
            config_path.display()
        );
        assert!(
            success_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .all(|regex| !regex.contains("APK_ADD_FS_EQUIV_LARGE_PAYLOAD")),
            "{} must not use intermediate payload diagnostics as success markers",
            config_path.display()
        );
        let fail_regex = config
            .get("fail_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            fail_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("APK_ADD_FS_EQUIV_TEST_FAILED")),
            "{} must fail on the probe failure marker",
            config_path.display()
        );
        let timeout = config
            .get("timeout")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        assert!(
            timeout <= 180,
            "{} must stay a focused diagnostic case",
            config_path.display()
        );
    }
}

#[test]
fn apk_net_equivalence_qemu_case_covers_apk_like_network_ops() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("apps/starry/qemu/apk-net-equivalence");
    let cmake_path = case_dir.join("c/CMakeLists.txt");
    let source_path = case_dir.join("c/src/main.c");

    assert!(
        cmake_path.is_file(),
        "{} must build the network equivalence probe through the C pipeline",
        cmake_path.display()
    );
    assert!(
        source_path.is_file(),
        "{} must contain the network equivalence probe source",
        source_path.display()
    );
    assert!(
        !case_dir.join("sh").exists() && !case_dir.join("python").exists(),
        "{} must stay a single C pipeline case",
        case_dir.display()
    );

    let source = fs::read_to_string(&source_path).unwrap();
    for forbidden in ["apk update", "apk add", "curl ", "http://", "https://"] {
        assert!(
            !source.contains(forbidden),
            "{} must not depend on package managers, curl, or external URLs",
            source_path.display()
        );
    }
    for required in [
        "socket(",
        "bind(",
        "getsockname(",
        "sendto(",
        "recvfrom(",
        "listen(",
        "accept(",
        "connect(",
        "send(",
        "recv(",
        "GET /alpine/APKINDEX.tar.gz",
        "GET /alpine/main/x86_64/fake-package.apk",
        "Host: apk.local",
        "Content-Length:",
        "APK_NET_EQUIV_TEST_PASSED",
        "APK_NET_EQUIV_TEST_FAILED",
    ] {
        assert!(
            source.contains(required),
            "{} must cover `{required}`",
            source_path.display()
        );
    }

    let cmake = fs::read_to_string(&cmake_path).unwrap();
    assert!(
        cmake.contains("project(apk-net-equivalence C)")
            && cmake.contains("add_executable(apk-net-equivalence")
            && cmake.contains("install(TARGETS apk-net-equivalence")
            && cmake.contains("DESTINATION usr/bin"),
        "{} must install the C probe into the guest image",
        cmake_path.display()
    );

    for arch in ["x86_64", "riscv64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        assert!(
            config_path.is_file(),
            "{} must exist after local validation for {arch}",
            config_path.display()
        );
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let args = config.get("args").and_then(toml::Value::as_array).unwrap();
        let args = args
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();

        assert!(
            args.iter()
                .any(|arg| arg.contains("virtio-blk-pci,drive=disk0")),
            "{} must exercise virtio-blk",
            config_path.display()
        );
        assert!(
            args.iter()
                .any(|arg| arg.contains("virtio-net-pci,netdev=net0")),
            "{} must exercise virtio-net",
            config_path.display()
        );
        assert!(
            args.iter().any(|arg| {
                arg.contains(&format!("rootfs-{arch}-alpine.img")) && arg.contains(".tgos-images")
            }),
            "{} must use the managed Alpine rootfs for {arch}",
            config_path.display()
        );

        assert_eq!(
            config
                .get("shell_init_cmd")
                .and_then(toml::Value::as_str)
                .unwrap(),
            "/usr/bin/apk-net-equivalence"
        );
        let success_regex = config
            .get("success_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            success_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("APK_NET_EQUIV_TEST_PASSED")),
            "{} must require the pass marker",
            config_path.display()
        );
        let fail_regex = config
            .get("fail_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            fail_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("APK_NET_EQUIV_TEST_FAILED")),
            "{} must fail on the probe failure marker",
            config_path.display()
        );
        let timeout = config
            .get("timeout")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        assert!(
            timeout <= 180,
            "{} must stay a focused diagnostic case",
            config_path.display()
        );
    }
}
