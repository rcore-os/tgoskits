use super::*;

#[test]
fn bug_ext4_dir_ops_is_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
    let case_dir = system_dir.join("bugfix-bug-ext4-dir-ops");
    assert!(
        case_dir.join("CMakeLists.txt").is_file(),
        "{} must remain a system grouped C subcase",
        case_dir.display()
    );

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let path = system_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let test_commands = config
            .get("test_commands")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            test_commands
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|command| command.contains("/usr/bin/starry-test-suit/*")),
            "{} must scan installed system test binaries",
            path.display()
        );
        let success_regex = config
            .get("success_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            success_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("STARRY_GROUPED_TESTS_PASSED")),
            "{} must require the system grouped success marker",
            path.display()
        );
        let fail_regex = config
            .get("fail_regex")
            .and_then(toml::Value::as_array)
            .unwrap();

        assert!(
            fail_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("STARRY_GROUPED_TEST_FAILED")),
            "{} must fail when a grouped bugfix command fails",
            path.display()
        );
    }
}

#[test]
fn starry_system_grouped_qemu_configs_report_subcase_timing() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for group in ["qemu-smp1", "qemu-smp4"] {
        let system_dir = workspace_root.join(format!("test-suit/starryos/{group}/system"));
        for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
            let path = system_dir.join(format!("qemu-{arch}.toml"));
            let content = fs::read_to_string(&path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let test_commands = config
                .get("test_commands")
                .and_then(toml::Value::as_array)
                .unwrap();
            let command = test_commands
                .iter()
                .filter_map(toml::Value::as_str)
                .next()
                .unwrap_or_default();

            assert!(
                command.contains("STARRY_SYSTEM_TEST_TIMING_BEGIN"),
                "{} must start a grouped subcase timing section",
                path.display()
            );
            assert!(
                command.contains("STARRY_SYSTEM_TEST_TIMING: elapsed_s="),
                "{} must report per-subcase elapsed seconds",
                path.display()
            );
            assert!(
                command.contains("status=passed bin=") && command.contains("status=failed bin="),
                "{} must include pass/fail status in timing lines",
                path.display()
            );
            assert!(
                command.contains("STARRY_SYSTEM_TEST_TIMING_END"),
                "{} must end a grouped subcase timing section",
                path.display()
            );
            assert!(
                !command.contains("sort -nr") && !command.contains("head -n"),
                "{} must not depend on external sort/head pipelines in the final timing summary",
                path.display()
            );
            assert!(
                command.contains("done < \"$timing_file\""),
                "{} must read grouped subcase timing from the timing file, not from stdin",
                path.display()
            );
            let failure_branch = command.find("else\n").unwrap_or_else(|| {
                panic!(
                    "{} must contain a failure branch for grouped subcases",
                    path.display()
                )
            });
            let failure_command = &command[failure_branch..];
            let exit_status_position =
                failure_command.find("exit_status=$?").unwrap_or_else(|| {
                    panic!(
                        "{} must preserve grouped subcase exit status",
                        path.display()
                    )
                });
            let status_failed_position = failure_command
                .find("status=failed")
                .unwrap_or_else(|| panic!("{} must mark failed grouped subcases", path.display()));
            assert!(
                exit_status_position < status_failed_position,
                "{} must capture `$?` before assigning shell variables in the failure branch",
                path.display()
            );
            assert!(
                command.contains("STARRY_GROUPED_TESTS_PASSED")
                    && command.contains("STARRY_GROUPED_TEST_FAILED"),
                "{} must keep existing grouped success/fail markers",
                path.display()
            );
        }
    }
}

#[test]
fn zombie_bugfix_commands_are_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
    let zombie_commands = [
        "/usr/bin/bug-kill-zombie-esrch",
        "/usr/bin/bug-kill-zombie-perm",
        "/usr/bin/bug-zombie-syscalls",
        "/usr/bin/bug-waitid-basic",
    ];

    for command in zombie_commands {
        let name = command.trim_start_matches("/usr/bin/");
        assert!(
            system_dir
                .join(format!("zombie-bugfix-{name}"))
                .join("CMakeLists.txt")
                .is_file(),
            "{} must be built in the system grouped case",
            command
        );
    }

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let system_path = system_dir.join(format!("qemu-{arch}.toml"));
        let system_content = fs::read_to_string(&system_path).unwrap();
        let system_config: toml::Value = toml::from_str(&system_content).unwrap();
        let system_commands = system_config
            .get("test_commands")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            system_commands
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|command| command.contains("/usr/bin/starry-test-suit/*")),
            "{} must scan installed system test binaries",
            system_path.display()
        );
    }
}

#[test]
fn tty_bugfix_commands_are_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
    let tty_commands = [
        "/usr/bin/bug-raw-terminal-polling",
        "/usr/bin/bug-tty-cursor-report",
    ];

    for command in tty_commands {
        let name = command.trim_start_matches("/usr/bin/");
        assert!(
            system_dir
                .join(format!("tty-bugfix-{name}"))
                .join("CMakeLists.txt")
                .is_file(),
            "{} must be built in the system grouped case",
            command
        );
    }

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let system_path = system_dir.join(format!("qemu-{arch}.toml"));
        let system_content = fs::read_to_string(&system_path).unwrap();
        let system_config: toml::Value = toml::from_str(&system_content).unwrap();
        let system_commands = system_config
            .get("test_commands")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            system_commands
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|command| command.contains("/usr/bin/starry-test-suit/*")),
            "{} must scan installed system test binaries",
            system_path.display()
        );
    }
}

#[test]
fn apk_curl_equivalence_is_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
    let subcase_dir = system_dir.join("apk-curl-equivalence");
    let cmake_path = subcase_dir.join("CMakeLists.txt");
    let prebuild_path = system_dir.join("prebuild.sh");
    let script_path = subcase_dir.join("src/apk-curl-equivalence.sh");

    let cmake = fs::read_to_string(&cmake_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", cmake_path.display()));
    let prebuild = fs::read_to_string(&prebuild_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", prebuild_path.display()));
    let script = fs::read_to_string(&script_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", script_path.display()));

    assert!(
        cmake.contains("set(CURL_BIN")
            && cmake.contains("install(PROGRAMS \"${CURL_BIN}\"")
            && cmake.contains("DESTINATION usr/bin/starry-test-suit")
            && cmake.contains("RENAME apk-curl-equivalence"),
        "{} must install curl and the apk-curl equivalence script into the grouped runner",
        cmake_path.display()
    );
    assert!(
        prebuild.contains("apk add") && prebuild.contains("curl"),
        "{} must install curl into the staging rootfs",
        prebuild_path.display()
    );
    assert!(
        !subcase_dir.join("qemu-x86_64.toml").exists(),
        "{} must not carry its own qemu config; qemu-smp1/system owns runtime config",
        subcase_dir.display()
    );
    assert!(
        script.contains("APK_CURL_EQUIVALENCE_TEST_PASSED")
            && script.contains("APK_CURL_EQUIVALENCE_TEST_FAILED")
            && script.contains("curl --connect-timeout")
            && script.contains("10.0.2.2")
            && script.contains("20971520")
            && script.contains("sha256sum -c")
            && script.contains("48b6fb8f1c2fec38d030604889d674722c4af237733c913b698400b59c9294b4"),
        "{} must download the local 20MiB HTTP fixture, write it to disk, then read it back and \
         compare sha256",
        script_path.display()
    );

    for (arch, port) in [
        ("x86_64", 18380_i64),
        ("aarch64", 18381_i64),
        ("riscv64", 18382_i64),
        ("loongarch64", 18383_i64),
    ] {
        let config_path = system_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let host_http_server = config
            .get("host_http_server")
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| {
                panic!(
                    "{} must start a local host HTTP fixture for apk-curl-equivalence",
                    config_path.display()
                )
            });

        assert_eq!(
            host_http_server.get("bind").and_then(toml::Value::as_str),
            Some("127.0.0.1")
        );
        assert_eq!(
            host_http_server
                .get("port")
                .and_then(toml::Value::as_integer),
            Some(port)
        );
        assert_eq!(
            host_http_server
                .get("body_size")
                .and_then(toml::Value::as_integer),
            Some(20 * 1024 * 1024)
        );
        assert_eq!(
            host_http_server
                .get("body_byte")
                .and_then(toml::Value::as_integer),
            Some(i64::from(b'a'))
        );
    }
}
