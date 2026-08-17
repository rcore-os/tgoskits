use super::*;

fn assert_system_runner_config(path: &Path) {
    let content = fs::read_to_string(path).unwrap();
    let config: toml::Value = toml::from_str(&content).unwrap();
    let test_commands = config
        .get("test_commands")
        .and_then(toml::Value::as_array)
        .unwrap();
    assert_eq!(
        test_commands.len(),
        1,
        "{} must invoke the system runner exactly once",
        path.display()
    );
    let command = test_commands[0].as_str().unwrap();
    assert!(
        matches!(
            command,
            "exec /usr/bin/starry-run-system-tests"
                | "exec /usr/bin/starry-run-system-tests --capture-failures"
        ),
        "{} must delegate grouped execution to the isolated system runner",
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
        "{} must fail when a grouped system test fails",
        path.display()
    );
}

fn assert_system_runner_contract(system_dir: &Path) {
    let path = system_dir.join("common/starry_system_test_runner.c");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

    assert!(
        source.contains("#define TEST_DIRECTORY \"/usr/bin/starry-test-suit\"")
            && source.contains("collect_test_names(&names, &name_count)")
            && source.contains("qsort(names, *count, sizeof(*names), compare_names)"),
        "{} must scan and sort installed system test binaries",
        path.display()
    );
    for marker in [
        "STARRY_SYSTEM_TEST_BEGIN:",
        "STARRY_SYSTEM_TEST_PASSED:",
        "STARRY_SYSTEM_TEST_FAILED:",
        "STARRY_SYSTEM_TEST_SUMMARY:",
        "STARRY_GROUPED_TEST_FAILED:",
        "STARRY_GROUPED_TESTS_PASSED",
    ] {
        assert!(
            source.contains(marker),
            "{} must report {marker}",
            path.display()
        );
    }
    assert_eq!(
        source.matches("STARRY_SYSTEM_TEST_BEGIN:").count(),
        1,
        "{} must identify each test exactly once",
        path.display()
    );
    assert_eq!(
        source.matches("STARRY_SYSTEM_TEST_PASSED:").count(),
        1,
        "{} must report each passing test exactly once",
        path.display()
    );
    assert_eq!(
        source.matches("STARRY_SYSTEM_TEST_FAILED:").count(),
        1,
        "{} must report each failing test exactly once",
        path.display()
    );

    let status_position = source
        .find("int exit_status = run_isolated_case(")
        .expect("runner must preserve each isolated case status");
    let result_position = source
        .find("if (exit_status == 0)")
        .expect("runner must classify each isolated case status");
    assert!(
        status_position < result_position,
        "{} must save the isolated exit status before updating counters",
        path.display()
    );
    assert!(
        source.contains("if (total == 0)")
            && source.contains("if (failed != 0)")
            && source.contains("return 1;")
            && source.ends_with("    return 0;\n}\n"),
        "{} must fail empty or failed suites and succeed only after the pass marker",
        path.display()
    );
}

fn assert_inline_grouped_runner_reports_each_result_once(path: &Path) {
    let content = fs::read_to_string(path).unwrap();
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
        command.contains("STARRY_SYSTEM_TEST_BEGIN: $bin"),
        "{} must identify each test before it starts",
        path.display()
    );
    assert!(
        command.contains("STARRY_SYSTEM_TEST_PASSED: $bin elapsed_s=$elapsed_s"),
        "{} must report one traceable duration for each passing test",
        path.display()
    );
    assert!(
        command.contains("$system_fail_marker: $bin status=$exit_status elapsed_s=$elapsed_s"),
        "{} must report the status and duration of each failing test",
        path.display()
    );
    assert!(
        command.contains(
            "STARRY_SYSTEM_TEST_SUMMARY: total=$total passed=$passed failed=$failed \
             elapsed_s=$suite_elapsed_s"
        ),
        "{} must report one compact suite timing summary",
        path.display()
    );
    assert!(
        !command.contains("STARRY_SYSTEM_TEST_TIMING") && !command.contains("timing_file="),
        "{} must not duplicate per-test durations in a trailing timing block",
        path.display()
    );
    let failure_branch = command.find("else\n").unwrap_or_else(|| {
        panic!(
            "{} must contain a failure branch for grouped subcases",
            path.display()
        )
    });
    let failure_command = &command[failure_branch..];
    let exit_status_position = failure_command.find("exit_status=$?").unwrap_or_else(|| {
        panic!(
            "{} must preserve grouped subcase exit status",
            path.display()
        )
    });
    let failed_count_position = failure_command
        .find("failed=$((failed + 1))")
        .unwrap_or_else(|| panic!("{} must mark failed grouped subcases", path.display()));
    assert!(
        exit_status_position < failed_count_position,
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

#[test]
fn bug_ext4_dir_ops_is_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu/system");
    let case_dir = system_dir.join("bugfix-bug-ext4-dir-ops");
    assert!(
        case_dir.join("CMakeLists.txt").is_file(),
        "{} must remain a system grouped C subcase",
        case_dir.display()
    );

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let path = system_dir.join(format!("qemu-{arch}.toml"));
        assert_system_runner_config(&path);
    }
    assert_system_runner_contract(&system_dir);
}

#[test]
fn starry_system_grouped_qemu_configs_report_each_result_once() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu/system");
    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        assert_system_runner_config(&system_dir.join(format!("qemu-{arch}.toml")));
    }
    assert_system_runner_contract(&system_dir);

    let rga_path = workspace_root.join("test-suit/starryos/qemu-rga/system/qemu-aarch64.toml");
    assert_inline_grouped_runner_reports_each_result_once(&rga_path);
}

#[test]
fn starry_system_runner_keeps_slow_case_timeouts_explicit() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_path =
        workspace_root.join("test-suit/starryos/qemu/system/common/starry_system_test_runner.c");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));

    assert!(
        source.contains("#define DEFAULT_CASE_TIMEOUT_SECONDS 120")
            && source.contains("#define EXT4_INODE_UNIQUE_TIMEOUT_SECONDS 240")
            && source.contains("#define PAGECACHE_CAP_TIMEOUT_SECONDS 240")
            && source.contains("strcmp(name, \"test-ext4-inode-unique\") == 0")
            && source.contains("strcmp(name, \"test-pagecache-cap\") == 0"),
        "{} must keep sync-heavy case exceptions explicit without relaxing the default timeout",
        source_path.display()
    );
    let timeout_log = source
        .find("STARRY_SYSTEM_TEST_TIMEOUT: %s timeout_s=%u")
        .expect("runner must report the selected timeout");
    let timeout_cleanup = source[timeout_log..]
        .find("kill_and_reap_namespace_init(namespace_init)")
        .expect("runner must clean up a timed-out namespace");
    let timeout_branch = &source[timeout_log..timeout_log + timeout_cleanup];
    assert!(
        source.contains("unsigned timeout_seconds = case_timeout_seconds(names[index]);")
            && source
                .contains("wait_for_namespace_init(namespace_init, &status, timeout_seconds);")
            && timeout_branch.contains("timeout_seconds"),
        "{} must select one timeout per binary and carry it through supervision and diagnostics",
        source_path.display()
    );
    assert!(
        !source.contains("#define CASE_TIMEOUT_SECONDS")
            && !source.contains("deadline.tv_sec += CASE_TIMEOUT_SECONDS"),
        "{} must not retain a single global timeout for every system binary",
        source_path.display()
    );
}

#[test]
fn starry_system_runner_bounds_namespace_cleanup_and_covers_raw_waiters() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let runner_path =
        workspace_root.join("test-suit/starryos/qemu/system/common/starry_system_test_runner.c");
    let runner = fs::read_to_string(&runner_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", runner_path.display()));
    assert!(
        runner.contains("#define NAMESPACE_CLEANUP_TIMEOUT_SECONDS 30")
            && runner.contains("wait_for_namespace_init(namespace_init, &status,")
            && runner.contains("NAMESPACE_CLEANUP_TIMEOUT_SECONDS);")
            && runner.contains("STARRY_SYSTEM_TEST_CLEANUP_TIMEOUT")
            && runner.contains("if (exit_status == RUNNER_ERROR_STATUS)"),
        "{} must bound namespace reap and abort before starting another case after cleanup failure",
        runner_path.display()
    );

    let leak_path =
        workspace_root.join("test-suit/starryos/qemu/system/test-case-task-isolation/src/leak.c");
    let leak = fs::read_to_string(&leak_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", leak_path.display()));
    assert!(
        leak.contains("int blocker[2]")
            && leak.contains("read(blocker[0], &never, sizeof(never))")
            && !leak.contains("pause()"),
        "{} must leave its descendant on a raw blocking wait so namespace shutdown proves forced \
         wakeup",
        leak_path.display()
    );
}

#[test]
fn stat_family_fixture_cleanup_does_not_spawn_shell_children() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_path =
        workspace_root.join("test-suit/starryos/qemu/system/syscall-test-stat-family/src/main.c");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
    assert!(
        source.contains("static int cleanup_fixture(void)")
            && source.contains("CHECK(cleanup_fixture() == 0")
            && !source.contains("system(cmd)"),
        "{} must clean its known fixture with direct syscalls instead of an unbounded shell wait",
        source_path.display()
    );
}

#[test]
fn signal_interrupt_eintr_subcase_bounds_child_wait() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_path = workspace_root
        .join("test-suit/starryos/qemu/system/test-signal-interrupt-eintr/src/main.c");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));

    assert!(
        source.contains("poll(&pfd, 1, -1)") && source.matches("kill(child, SIGUSR1)").count() >= 2,
        "{} must preserve the poll EINTR check and retry SIGUSR1 while the child is still running",
        source_path.display()
    );
    assert!(
        source.contains("TEST_TIMEOUT_MS") && source.contains("WNOHANG"),
        "{} must bound the parent wait for the interruptible child",
        source_path.display()
    );
    assert!(
        source.contains("read_ready_byte(")
            && source.contains("errno == EINTR")
            && source.contains("parent read child ready pipe"),
        "{} must retry the parent ready-pipe read on EINTR and report why it failed",
        source_path.display()
    );
    assert!(
        !source.contains("waitpid(child, &status, 0)"),
        "{} must not let a stuck child consume the whole grouped QEMU timeout",
        source_path.display()
    );
}

#[test]
fn tty_console_input_burst_uses_injected_guest_script() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("test-suit/starryos/qemu/tty-console-input-burst");
    let script_path = case_dir.join("sh/tty-input-burst.sh");
    assert!(
        script_path.is_file(),
        "{} must inject the burst script through the rootfs instead of pasting it over the console",
        script_path.display()
    );

    let script = fs::read_to_string(&script_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", script_path.display()));
    assert!(
        script.contains("while [ \"$i\" -lt 120 ]")
            && script.contains("STARRY_TTY_INPUT_BURST_PASSED"),
        "{} must preserve the burst payload checks and success marker",
        script_path.display()
    );

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let command = config
            .get("shell_init_cmd")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();

        assert_eq!(
            command,
            "/usr/bin/tty-input-burst.sh",
            "{} must only send a short command through the console",
            path.display()
        );
        assert!(
            !content.contains("cat > /tmp/tty-input-burst.sh"),
            "{} must not paste a long heredoc through the console",
            path.display()
        );
    }
}

#[test]
fn qemu_system_case_has_riscv64_runtime_config() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = workspace_root.join("test-suit/starryos/qemu/system/qemu-riscv64.toml");

    assert!(
        config.is_file(),
        "{} must keep riscv64 coverage in the unified SMP4 qemu/system case",
        config.display()
    );
}

#[test]
fn mountinfo_root_source_tracks_the_nvme_qemu_root_disk() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu/system");
    let source_path = system_dir.join("syscall-test-mountinfo/src/main.c");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));

    assert!(
        source.contains("#define ROOT_MOUNT_SOURCE \"/dev/nvme0n1\""),
        "{} must expect the NVMe root device exposed by every Starry QEMU system config",
        source_path.display()
    );

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let config_path = system_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(
            content.contains("\"nvme,") && !content.contains("virtio-blk"),
            "{} must attach the Starry rootfs through NVMe",
            config_path.display()
        );
    }
}

#[test]
fn qemu_affinity_flaky_arches_are_filtered() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = [
        (
            "affinity-bug-sched-affinity-migrate",
            "^(aarch64|x86_64)",
            "bug-sched-affinity-migrate skipped on loongarch64/riscv64 qemu",
        ),
        (
            "affinity-bug-sched-affinity-pid",
            "^(aarch64|x86_64)",
            "bug-sched-affinity-pid skipped on loongarch64/riscv64 qemu",
        ),
    ];

    for (case, arch_regex, skip_message) in cases {
        let cmake_path = workspace_root
            .join("test-suit/starryos/qemu/system")
            .join(case)
            .join("CMakeLists.txt");
        let cmake = fs::read_to_string(&cmake_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", cmake_path.display()));

        assert!(
            cmake.contains("starry_arch_filtered_executable")
                && cmake.contains(arch_regex)
                && cmake.contains(skip_message),
            "{} must skip flaky qemu affinity probes instead of letting them consume the grouped \
             QEMU timeout",
            cmake_path.display()
        );
    }
}

#[test]
fn zombie_bugfix_commands_are_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu/system");
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
        assert_system_runner_config(&system_path);
    }
    assert_system_runner_contract(&system_dir);
}

#[test]
fn tty_regressions_are_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu/system");
    let tty_cases = [
        (
            "tty-bugfix-bug-raw-terminal-polling",
            "bug-raw-terminal-polling",
        ),
        ("tty-bugfix-bug-tty-cursor-report", "bug-tty-cursor-report"),
        ("test-tty-flush", "test-tty-flush"),
        (
            "test-tty-termios-transaction",
            "test-tty-termios-transaction",
        ),
    ];

    for (directory, binary) in tty_cases {
        let cmake_path = system_dir.join(directory).join("CMakeLists.txt");
        let cmake = fs::read_to_string(&cmake_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", cmake_path.display()));
        assert!(
            cmake.contains(&format!("install(TARGETS {binary}"))
                && cmake.contains("RUNTIME DESTINATION usr/bin/starry-test-suit"),
            "{} must install {binary} into the grouped runner directory",
            cmake_path.display()
        );
    }

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let system_path = system_dir.join(format!("qemu-{arch}.toml"));
        assert_system_runner_config(&system_path);
    }
    assert_system_runner_contract(&system_dir);
}

#[test]
fn serial_mailbox_kernel_tests_keep_four_cpu_qemu_configs() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config_dir = workspace_root.join("os/StarryOS/kernel/tests");

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let path = config_dir.join(format!("qemu-{arch}-smp.toml"));
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let config: toml::Value = toml::from_str(&content).unwrap();
        let args = config
            .get("args")
            .and_then(toml::Value::as_array)
            .unwrap_or_else(|| panic!("{} must define QEMU args", path.display()));
        let smp_index = args
            .iter()
            .position(|arg| arg.as_str() == Some("-smp"))
            .unwrap_or_else(|| panic!("{} must select SMP explicitly", path.display()));
        assert_eq!(
            args.get(smp_index + 1).and_then(toml::Value::as_str),
            Some("4"),
            "{} must exercise all four mailbox producer CPUs",
            path.display()
        );
    }
}

#[test]
fn apk_curl_equivalence_is_in_system_grouped_qemu_case() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system_dir = workspace_root.join("test-suit/starryos/qemu/system");
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
        "{} must not carry its own qemu config; qemu/system owns runtime config",
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
