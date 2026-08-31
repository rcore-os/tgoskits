use super::*;
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
fn pagecache_cap_cleanup_uses_disposable_qemu_rootfs() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_path =
        workspace_root.join("test-suit/starryos/qemu/system/syscall-test-pagecache-cap/src/main.c");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));

    assert!(
        source.contains("g_maps[i] = m;")
            && source.contains("#define DIR \"/root/pgcachecap\"")
            && !source.contains("munmap(")
            && !source.contains("unlink(")
            && !source.contains("rmdir("),
        "{} must retain its mappings in a unique directory and avoid one cleanup syscall per \
         fixture",
        source_path.display()
    );

    let qemu_run_path = workspace_root.join("scripts/axbuild/src/starry/test/qemu_run.rs");
    let qemu_run = fs::read_to_string(&qemu_run_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", qemu_run_path.display()));
    assert!(
        qemu_run.contains("write_policy: rootfs::RootfsWritePolicy::Discard"),
        "{} must discard guest writes after each QEMU case so pagecache-cap can leave its unique \
         fixture directory to snapshot teardown",
        qemu_run_path.display()
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
        source.contains("poll(&pfd, 1, -1)") && source.contains("kill(child, SIGUSR1)"),
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
