use super::*;

#[test]
fn inotifywait_qemu_case_installs_tool_before_boot() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("apps/starry/qemu/inotifywait");
    let config_path = case_dir.join("qemu-x86_64.toml");
    let cmake_path = case_dir.join("c/CMakeLists.txt");
    let script_path = case_dir.join("c/inotifywait-tests.sh");
    let prebuild_path = case_dir.join("c/prebuild.sh");

    assert!(
        script_path.is_file(),
        "{} must be installed through the case C pipeline",
        script_path.display()
    );
    assert!(
        cmake_path.is_file(),
        "{} must install inotifywait assets through the host CMake install phase",
        cmake_path.display()
    );
    assert!(
        prebuild_path.is_file(),
        "{} must install inotify-tools into the staging root before CMake install",
        prebuild_path.display()
    );

    let script = fs::read_to_string(&script_path).unwrap();
    for guest_apk_command in ["apk update", "apk add"] {
        assert!(
            !script.contains(guest_apk_command),
            "{} must not run `{guest_apk_command}` after StarryOS boots",
            script_path.display()
        );
    }
    assert!(
        script.contains("command -v inotifywait"),
        "{} must still exercise the inotifywait userspace tool",
        script_path.display()
    );

    let prebuild = fs::read_to_string(&prebuild_path).unwrap();
    assert!(
        prebuild.contains("apk add") && prebuild.contains("inotify-tools"),
        "{} must install the inotify-tools package during case asset preparation",
        prebuild_path.display()
    );
    for host_overlay_command in ["STARRY_CASE_OVERLAY_DIR", "cp ", "chmod ", "mkdir "] {
        assert!(
            !prebuild.contains(host_overlay_command),
            "{} must not manipulate host overlay paths from the guest prebuild shell",
            prebuild_path.display()
        );
    }
    assert!(
        prebuild.contains("STARRY_STAGING_ROOT/usr/bin/inotifywait"),
        "{} must verify that apk installed the inotifywait tool in the staging root",
        prebuild_path.display()
    );

    let cmake = fs::read_to_string(&cmake_path).unwrap();
    assert!(
        cmake.contains("install(PROGRAMS inotifywait-tests.sh")
            && cmake.contains("${STARRY_STAGING_ROOT}/usr/bin/inotifywait")
            && cmake.contains("DESTINATION usr/bin"),
        "{} must copy both test script and inotifywait through CMake install",
        cmake_path.display()
    );

    let content = fs::read_to_string(&config_path).unwrap();
    let config: toml::Value = toml::from_str(&content).unwrap();
    let timeout = config
        .get("timeout")
        .and_then(toml::Value::as_integer)
        .unwrap_or_default();
    assert!(
        timeout <= 180,
        "{} must fail quickly because apk setup happens before QEMU boot",
        config_path.display()
    );

    let success_regex = config
        .get("success_regex")
        .and_then(toml::Value::as_array)
        .unwrap();
    assert!(
        success_regex
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|regex| regex.contains("INOTIFYWAIT_TEST_PASSED")),
        "{} must require the inotifywait test pass marker",
        config_path.display()
    );
}

#[test]
fn procps_qemu_case_installs_tools_before_boot() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("apps/starry/qemu/procps");
    let cmake_path = case_dir.join("c/CMakeLists.txt");
    let script_path = case_dir.join("c/procps-test.sh");
    let prebuild_path = case_dir.join("c/prebuild.sh");

    assert!(
        script_path.is_file(),
        "{} must be installed through the case C pipeline",
        script_path.display()
    );
    assert!(
        cmake_path.is_file(),
        "{} must install procps assets through the host CMake install phase",
        cmake_path.display()
    );
    assert!(
        prebuild_path.is_file(),
        "{} must install procps into the staging root before CMake install",
        prebuild_path.display()
    );
    assert!(
        !case_dir.join("sh").exists(),
        "{} must not keep the old shell pipeline that cannot prebuild packages",
        case_dir.join("sh").display()
    );

    let script = fs::read_to_string(&script_path).unwrap();
    for guest_apk_command in ["apk update", "apk add", "apk info"] {
        assert!(
            !script.contains(guest_apk_command),
            "{} must not run `{guest_apk_command}` after StarryOS boots",
            script_path.display()
        );
    }
    assert!(
        script.contains("PROCPS_TEST_PASSED") && script.contains("command -v pmap"),
        "{} must still exercise the installed procps tools",
        script_path.display()
    );

    let prebuild = fs::read_to_string(&prebuild_path).unwrap();
    assert!(
        prebuild.contains("apk add") && prebuild.contains("procps"),
        "{} must install procps during case asset preparation",
        prebuild_path.display()
    );
    for tool in ["ps", "free", "uptime", "pgrep", "pmap"] {
        assert!(
            prebuild.contains(tool),
            "{} must verify that apk installed the {tool} tool in the staging root",
            prebuild_path.display()
        );
    }

    let cmake = fs::read_to_string(&cmake_path).unwrap();
    assert!(
        cmake.contains("install(PROGRAMS procps-test.sh")
            && cmake.contains("STARRY_STAGING_ROOT")
            && cmake.contains("ps")
            && cmake.contains("pmap"),
        "{} must install both the procps test script and staging-root tools",
        cmake_path.display()
    );

    for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let timeout = config
            .get("timeout")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        assert!(
            timeout <= 180,
            "{} must fail quickly because procps setup happens before QEMU boot",
            config_path.display()
        );
    }
}

#[test]
fn lua_qemu_case_installs_lua_before_boot() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let case_dir = workspace_root.join("apps/starry/qemu/lua");
    let cmake_path = case_dir.join("c/CMakeLists.txt");
    let script_path = case_dir.join("c/lua-app-tests.sh");
    let prebuild_path = case_dir.join("c/prebuild.sh");

    assert!(
        script_path.is_file(),
        "{} must be installed through the case C pipeline",
        script_path.display()
    );
    assert!(
        cmake_path.is_file(),
        "{} must install Lua assets through the host CMake install phase",
        cmake_path.display()
    );
    assert!(
        prebuild_path.is_file(),
        "{} must install Lua packages into the staging root before QEMU boot",
        prebuild_path.display()
    );
    assert!(
        !case_dir.join("sh").exists(),
        "{} must not keep the old shell pipeline that cannot prebuild packages",
        case_dir.join("sh").display()
    );

    let script = fs::read_to_string(&script_path).unwrap();
    for guest_apk_command in ["apk update", "apk add"] {
        assert!(
            !script.contains(guest_apk_command),
            "{} must not run `{guest_apk_command}` after StarryOS boots",
            script_path.display()
        );
    }
    assert!(
        script.contains("lua5.4 /usr/bin/lua-main.lua alpha beta")
            && script.contains("LUA_APP_TEST_FAILED"),
        "{} must still exercise the Lua runtime and report failures",
        script_path.display()
    );

    let prebuild = fs::read_to_string(&prebuild_path).unwrap();
    assert!(
        prebuild.contains("apk add")
            && prebuild.contains("lua5.4")
            && prebuild.contains("lua5.4-cjson"),
        "{} must install Lua packages during case asset preparation",
        prebuild_path.display()
    );
    for staged_path in [
        "STARRY_STAGING_ROOT/usr/bin/lua5.4",
        "STARRY_STAGING_ROOT/usr/lib/lua/5.4/cjson.so",
    ] {
        assert!(
            prebuild.contains(staged_path),
            "{} must verify {} exists in the staging root",
            prebuild_path.display(),
            staged_path
        );
    }

    let cmake = fs::read_to_string(&cmake_path).unwrap();
    assert!(
        cmake.contains("install(PROGRAMS lua-app-tests.sh")
            && cmake.contains("${STARRY_STAGING_ROOT}/usr/bin/lua5.4")
            && cmake.contains("${STARRY_STAGING_ROOT}/usr/lib/lua/5.4/cjson.so"),
        "{} must install the Lua interpreter, cjson module, and test scripts",
        cmake_path.display()
    );

    for arch in ["aarch64", "riscv64", "x86_64"] {
        let config_path = case_dir.join(format!("qemu-{arch}.toml"));
        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let timeout = config
            .get("timeout")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        assert!(
            timeout <= 180,
            "{} must fail quickly because Lua setup happens before QEMU boot",
            config_path.display()
        );
    }
}
