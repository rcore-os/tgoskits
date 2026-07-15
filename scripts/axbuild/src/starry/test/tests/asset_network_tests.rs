use super::*;

#[test]
fn nix_sandbox_cases_keep_rootfs_apk_repositories() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("test-suit/starryos/qemu/nix-sandbox");

    for arch in ["aarch64", "x86_64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let command = config
            .get("test_commands")
            .and_then(toml::Value::as_array)
            .and_then(|commands| commands.first())
            .and_then(toml::Value::as_str)
            .unwrap();

        assert!(
            command.contains("apk update"),
            "{} must update the rootfs repositories",
            config_path.display()
        );
        assert!(
            !command.contains("/etc/apk/repositories")
                && !command.contains("mirrors.tuna.tsinghua.edu.cn"),
            "{} must retain the official repositories provided by the rootfs",
            config_path.display()
        );
    }
}

#[test]
fn nix_sandbox_cases_require_a_fresh_sandboxed_builder() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("test-suit/starryos/qemu/nix-sandbox");

    for arch in ["aarch64", "x86_64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let command = config
            .get("test_commands")
            .and_then(toml::Value::as_array)
            .and_then(|commands| commands.first())
            .and_then(toml::Value::as_str)
            .unwrap();

        for forbidden in [
            "--option sandbox false",
            "result-nosandbox",
            "NIX_NOSANDBOX_RESULT",
            "/proc/[0-9]*",
        ] {
            assert!(
                !command.contains(forbidden),
                "{} must not contain `{forbidden}`",
                config_path.display()
            );
        }
        for required in [
            "--option sandbox true",
            "NIX_SANDBOX_BUILD_TOKEN",
            "NIX_SANDBOX_BUILD_OK:$build_token",
            "grep -Fqx \"NIX_SANDBOX_BUILD_OK:$build_token\" ./result-sandbox",
            "grep -Fq \"building '\" /tmp/nix-sandbox/build.log",
        ] {
            assert!(
                command.contains(required),
                "{} must require fresh sandbox builder evidence `{required}`",
                config_path.display()
            );
        }
    }
}

#[test]
fn nix_sandbox_cases_enforce_bounded_truthful_completion() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("test-suit/starryos/qemu/nix-sandbox");

    for arch in ["aarch64", "x86_64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let command = config
            .get("test_commands")
            .and_then(toml::Value::as_array)
            .and_then(|commands| commands.first())
            .and_then(toml::Value::as_str)
            .unwrap();

        assert_eq!(
            config.get("timeout").and_then(toml::Value::as_integer),
            Some(600),
            "{} must cap the QEMU case at ten minutes",
            config_path.display()
        );
        for required in [
            "test_timeout=600",
            "diagnostic_reserve=20",
            "remaining_test_budget",
            "build_deadline=",
            "now=$(date +%s)",
            "-ge \"$build_deadline\"",
            "build_pid=$!",
            "/tmp/nix-sandbox/build.log",
            "NIX_SANDBOX_BUILDER_STARTED:$build_token",
            "NIX_SANDBOX_BUILDER_DONE:$build_token",
            "NIX_SANDBOX_BUILD_OK:$build_token",
            "recovered nonzero nix-build exit",
            "NIX_SANDBOX_BUILD_EXIT=$build_rc",
        ] {
            assert!(
                command.contains(required),
                "{} must enforce bounded truthful completion via `{required}`",
                config_path.display()
            );
        }

        let success_regex = config
            .get("success_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert_eq!(
            success_regex.len(),
            1,
            "{} must have exactly one final success contract",
            config_path.display()
        );
        assert!(
            success_regex[0]
                .as_str()
                .is_some_and(|regex| regex.contains("NIX_SANDBOX_TEST_PASSED")),
            "{} must not accept a phase-only marker as success",
            config_path.display()
        );

        let fail_regex = config
            .get("fail_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        for required in [
            "panic",
            "lockdep fatal violation",
            "NIX_SANDBOX_TEST_FAILED",
            "NIX_SANDBOX_ERROR:",
        ] {
            assert!(
                fail_regex
                    .iter()
                    .any(|regex| { regex.as_str().is_some_and(|regex| regex.contains(required)) }),
                "{} must retain fail regex `{required}`",
                config_path.display()
            );
        }
    }
}

#[test]
fn x86_64_nix_sandbox_diagnostic_is_bounded_without_proc_status() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config_path = workspace_root.join("test-suit/starryos/qemu/nix-sandbox/qemu-x86_64.toml");
    let content = fs::read_to_string(&config_path).unwrap();
    let config: toml::Value = toml::from_str(&content).unwrap();
    let command = config
        .get("test_commands")
        .and_then(toml::Value::as_array)
        .and_then(|commands| commands.first())
        .and_then(toml::Value::as_str)
        .unwrap();

    assert_eq!(
        config.get("timeout").and_then(toml::Value::as_integer),
        Some(120),
        "{} must cap the ordinary QEMU test at two minutes",
        config_path.display()
    );
    for required in [
        "test_started=$(date +%s)",
        "remaining_test_budget",
        "nix-build -vvvvv --no-substitute --option sandbox true",
        "NIX_SANDBOX_BUILDER_STARTED:$build_token",
        "NIX_SANDBOX_BUILDER_DONE:$build_token",
    ] {
        assert!(
            command.contains(required),
            "{} must enforce the shared runtime budget and reap zombies via `{required}`",
            config_path.display()
        );
    }
    assert!(
        !command.contains("/proc/$pid/status")
            && !command.contains("build_is_running")
            && !command.contains("watchdog_pid")
            && !command.contains("timeout -k"),
        "{} must rely on the bounded runner without proc status or nested watchdogs",
        config_path.display()
    );
}

#[test]
fn apk_curl_qemu_case_tries_cernet_before_upstream() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("apps/starry/qemu/apk-curl");
    let script_path = case_dir.join("sh/apk-curl-tests.sh");
    let script = fs::read_to_string(&script_path).unwrap();

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let shell_init_cmd = config
            .get("shell_init_cmd")
            .and_then(toml::Value::as_str)
            .unwrap();

        assert!(
            shell_init_cmd == "/usr/bin/apk-curl-tests.sh",
            "{} must run the injected apk-curl script instead of pasting a long shell body",
            config_path.display()
        );
    }

    assert!(
        script.contains("apk --timeout \"$fetch_timeout\" add curl"),
        "{} must install curl dynamically to exercise the apk add path",
        script_path.display()
    );
    assert!(
        script.contains("mirrors.cernet.edu.cn") && script.contains("dl-cdn.alpinelinux.org"),
        "{} must provide Cernet first and upstream as a fallback",
        script_path.display()
    );
    let cernet_index = script.find("mirrors.cernet.edu.cn").unwrap();
    let upstream_index = script.find("dl-cdn.alpinelinux.org").unwrap();
    assert!(
        cernet_index < upstream_index,
        "{} must try Cernet before upstream",
        script_path.display()
    );
    assert!(
        !script.contains("mirrors.aliyun.com")
            && !script.contains("mirrors.tuna.tsinghua.edu.cn")
            && !script.contains("mirrors.ustc.edu.cn"),
        "{} must avoid mirrors that repeatedly timeout in QEMU",
        script_path.display()
    );
    assert!(
        !script.contains("__original__"),
        "{} must use explicit mirror attempts so the selected repository is diagnosable",
        script_path.display()
    );
    assert!(
        script.contains("APK_CURL_REPO_$label")
            && script.contains("APK_CURL_TEST_PASSED")
            && script.contains("APK_CURL_TEST_FAILED"),
        "{} must keep clear pass/fail diagnostics",
        script_path.display()
    );
}

#[test]
fn dhcp_qemu_case_checks_local_dhcp_state_without_external_apk_fetch() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let build_group_dir = workspace_root.join("apps/starry/qemu");
    let case_dir = workspace_root.join("apps/starry/qemu/dhcp");

    for (arch, target) in [
        ("aarch64", "aarch64-unknown-none-softfloat"),
        ("loongarch64", "loongarch64-unknown-none-softfloat"),
        ("riscv64", "riscv64gc-unknown-none-elf"),
        ("x86_64", "x86_64-unknown-none"),
    ] {
        let build_config_path = build_group_dir.join(format!("build-{target}.toml"));
        let build_content = fs::read_to_string(&build_config_path).unwrap();
        let build_config: toml::Value = toml::from_str(&build_content).unwrap();
        assert_eq!(
            build_config.get("target").and_then(toml::Value::as_str),
            Some(target),
            "{} must target {target}",
            build_config_path.display()
        );

        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let script = config
            .get("shell_init_cmd")
            .and_then(toml::Value::as_str)
            .unwrap();

        assert!(
            !script.contains("apk update")
                && !script.contains("apk --timeout")
                && !script.contains("http://")
                && !script.contains("https://"),
            "{} must not depend on external APK repositories; DHCP is already local to the QEMU \
             user network",
            config_path.display()
        );
        for marker in [
            "DHCP_PROBE_BEGIN",
            "DHCP_ADDR_OK",
            "DHCP_RESOLVER_OK",
            "DHCP_TEST_DONE",
            "DHCP_TEST_FAILED",
        ] {
            assert!(
                script.contains(marker),
                "{} must print the diagnostic marker `{marker}`",
                config_path.display()
            );
        }
        for expected in [
            "ifconfig eth0",
            "ip addr show",
            "/etc/resolv.conf",
            "10.0.2.15",
            "10.0.2.3",
        ] {
            assert!(
                script.contains(expected),
                "{} must check `{expected}` in the local QEMU DHCP state",
                config_path.display()
            );
        }

        let fail_regex = config
            .get("fail_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            fail_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("DHCP_TEST_FAILED")),
            "{} must fail explicitly on the DHCP probe failure marker",
            config_path.display()
        );
        let timeout = config
            .get("timeout")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        assert!(
            timeout <= 120,
            "{} must stay a focused local DHCP diagnostic case",
            config_path.display()
        );
    }
}

#[test]
fn dual_net_qemu_case_exercises_two_interfaces_and_parallel_fetches() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("apps/starry/qemu/dual-net");
    let script_path = case_dir.join("c/dual-net-tests.sh");
    let prebuild_path = case_dir.join("c/prebuild.sh");
    let cmake_path = case_dir.join("c/CMakeLists.txt");

    assert!(
        script_path.is_file(),
        "{} must contain the guest dual-net probe",
        script_path.display()
    );
    assert!(
        prebuild_path.is_file() && cmake_path.is_file(),
        "{} must use the C pipeline so curl is installed before boot",
        case_dir.display()
    );

    for arch in ["x86_64", "aarch64", "riscv64", "loongarch64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        assert!(
            config_path.is_file(),
            "{} must provide a QEMU runtime config",
            config_path.display()
        );

        let config: toml::Value =
            toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        let args = config
            .get("args")
            .and_then(toml::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();
        for expected in [
            "virtio-net-pci,netdev=net0",
            "virtio-net-pci,netdev=net1",
            "user,id=net0,net=10.0.2.0/24,dhcpstart=10.0.2.15",
            "user,id=net1,net=10.0.3.0/24,dhcpstart=10.0.3.15",
        ] {
            assert!(
                args.iter().any(|arg| arg.contains(expected)),
                "{} must include `{expected}`",
                config_path.display()
            );
        }
        assert_eq!(
            config.get("shell_init_cmd").and_then(toml::Value::as_str),
            Some("/usr/bin/dual-net-tests.sh")
        );
        let http = config
            .get("host_http_server")
            .and_then(toml::Value::as_table)
            .expect("dual-net case must start a host HTTP fixture");
        assert_eq!(
            http.get("port").and_then(toml::Value::as_integer),
            Some(18382)
        );
        assert!(
            http.get("body_size")
                .and_then(toml::Value::as_integer)
                .is_some_and(|size| size >= 1024 * 1024),
            "dual-net case must fetch a payload large enough to expose obvious regressions"
        );
        assert!(
            config
                .get("timeout")
                .and_then(toml::Value::as_integer)
                .is_some_and(|timeout| timeout >= 360),
            "{} must leave enough time for the apk package download stability probe",
            config_path.display()
        );
    }

    let script = fs::read_to_string(&script_path).unwrap();
    for expected in [
        "now_ms()",
        "iface_addr_contains eth0 10.0.2.15",
        "iface_addr_contains eth1 10.0.3.15",
        "curl --interface \"$iface\"",
        "fetch_with_iface eth0 10.0.2.2",
        "fetch_with_iface eth1 10.0.3.2",
        "DUAL_NET_FETCH_PARALLEL_MS",
        "apk fetch -R",
        "APK_STRESS_MIN_BYTES",
        "APK_STRESS_RETRIES",
        "DUAL_NET_RETRY",
        "apk verify",
        "sha256sum -c",
        "DUAL_NET_APK_FETCH_MS",
        "DUAL_NET_TEST_PASSED",
        "DUAL_NET_TEST_FAILED",
    ] {
        assert!(
            script.contains(expected),
            "{} must contain `{expected}`",
            script_path.display()
        );
    }

    let prebuild = fs::read_to_string(&prebuild_path).unwrap();
    assert!(
        prebuild.contains("apk add curl"),
        "{} must install curl during asset preparation, not at guest runtime",
        prebuild_path.display()
    );
}
