use std::{collections::BTreeSet, ffi::OsStr, fs, path::PathBuf, process::Command, time::Duration};

use tempfile::tempdir;

use super::{cmake::*, grouped_c::*, prebuild::*, toolchain::*, wrappers::*, *};

fn fake_config() -> CaseAssetConfig {
    CaseAssetConfig {
        grouped_runner: case_assets::GroupedCaseRunnerConfig {
            runner_name: "suite-run-case-tests".to_string(),
            runner_path: "/usr/bin/suite-run-case-tests".to_string(),
            autorun_profile_script: None,
            begin_marker: "SUITE_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "SUITE_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "SUITE_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "SUITE_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "SUITE_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^SUITE_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^SUITE_GROUPED_TEST_FAILED:".to_string(),
        },
        script_env: case_assets::CaseScriptEnvConfig {
            staging_root: "SUITE_STAGING_ROOT".to_string(),
            case_dir: "SUITE_CASE_DIR".to_string(),
            case_c_dir: "SUITE_CASE_C_DIR".to_string(),
            case_work_dir: "SUITE_CASE_WORK_DIR".to_string(),
            case_build_dir: "SUITE_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "SUITE_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: vec!["SUITE_PACKAGE_REGION".to_string()],
        prepare_staging_root: |_| Ok(()),
        prepare_guest_package_env: Some(|_| {
            Ok(vec![("SUITE_PACKAGE_REGION".to_string(), "us".to_string())])
        }),
    }
}

fn fake_case(root: &Path, name: &str) -> TestQemuCase {
    let case_dir = root.join("test-suite/example/default").join(name);
    fs::create_dir_all(&case_dir).unwrap();
    TestQemuCase {
        name: name.to_string(),
        display_name: name.to_string(),
        case_dir: case_dir.clone(),
        qemu_config_path: case_dir.join("qemu-aarch64.toml"),
        test_commands: Vec::new(),
        host_symbolize_success_regex: Vec::new(),
        host_http_server: None,
        subcases: Vec::new(),
        grouped_subcase_filter: None,
    }
}

fn fake_c_subcase(
    root: &Path,
    case: &TestQemuCase,
    name: &str,
    install_targets: &[&str],
) -> TestQemuSubcase {
    let case_dir = case.case_dir.join(name);
    let c_dir = case_dir.join("c");
    fs::create_dir_all(&c_dir).unwrap();
    fs::write(
        c_dir.join(CASE_CMAKE_FILE_NAME),
        format!(
            "add_executable({target} src/main.c)\ninstall(TARGETS {} RUNTIME DESTINATION \
             usr/bin)\n",
            install_targets.join(" "),
            target = install_targets.first().unwrap_or(&name)
        ),
    )
    .unwrap();

    assert!(case_dir.starts_with(root));
    TestQemuSubcase {
        name: name.to_string(),
        case_dir,
        kind: TestQemuSubcaseKind::C,
    }
}

fn command_env(command: &Command, key: &str) -> Option<String> {
    command.get_envs().find_map(|(name, value)| {
        (name == OsStr::new(key))
            .then(|| value.map(|value| value.to_string_lossy().into_owned()))
            .flatten()
    })
}

fn command_args(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn write_musl_loader_search_path_uses_requested_guest_arch() {
    let root = tempdir().unwrap();
    let staging_root = root.path().join("staging-root");
    fs::create_dir_all(staging_root.join("lib")).unwrap();
    fs::write(staging_root.join("lib/ld-musl-riscv64.so.1"), b"").unwrap();

    write_musl_loader_search_path("riscv64", &staging_root).unwrap();

    assert_eq!(
        fs::read_to_string(staging_root.join("etc/ld-musl-riscv64.path")).unwrap(),
        "/usr/lib\n/lib\n"
    );
    assert!(!staging_root.join("etc/ld-musl-aarch64.path").exists());
}

#[test]
fn write_musl_loader_search_path_skips_when_guest_loader_is_missing() {
    let root = tempdir().unwrap();
    let staging_root = root.path().join("staging-root");
    fs::create_dir_all(staging_root.join("lib")).unwrap();
    fs::write(staging_root.join("lib/ld-musl-riscv64.so.1"), b"").unwrap();

    write_musl_loader_search_path("aarch64", &staging_root).unwrap();

    assert!(!staging_root.join("etc/ld-musl-aarch64.path").exists());
    assert!(!staging_root.join("etc/ld-musl-riscv64.path").exists());
}

#[test]
fn build_prebuild_command_uses_guest_shell_and_case_envs() {
    let root = tempdir().unwrap();
    let case = fake_case(root.path(), "usb");
    let layout =
        case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
            .unwrap();
    fs::create_dir_all(layout.staging_root.join("bin")).unwrap();
    fs::write(layout.staging_root.join("bin/sh"), b"").unwrap();
    fs::write(layout.staging_root.join("bin/busybox"), b"").unwrap();
    let prebuild_env = GuestPrebuildEnv {
        qemu_runner: PathBuf::from("/usr/bin/qemu-aarch64-static"),
        script_envs: {
            let mut envs = case_script_envs(&case, &layout, &fake_config());
            envs.push(("SUITE_PACKAGE_REGION".to_string(), "us".to_string()));
            envs
        },
    };
    let prebuild_script = case_c_source_dir(&case).join("prebuild.sh");

    let command = build_prebuild_command(&case, &prebuild_script, &layout, &prebuild_env).unwrap();

    assert_eq!(
        command.get_program(),
        std::ffi::OsStr::new("/usr/bin/qemu-aarch64-static")
    );
    assert_eq!(
        command_args(&command),
        vec![
            "-L".to_string(),
            layout.staging_root.display().to_string(),
            layout
                .staging_root
                .join("bin/busybox")
                .display()
                .to_string(),
            "sh".to_string(),
            "-eu".to_string(),
            prebuild_script.display().to_string(),
        ]
    );
    assert_eq!(
        command.get_current_dir(),
        Some(case_c_source_dir(&case).as_path())
    );
    assert_eq!(
        command_env(&command, "SUITE_CASE_OVERLAY_DIR"),
        Some(layout.overlay_dir.display().to_string())
    );
    assert_eq!(
        command_env(&command, "SUITE_PACKAGE_REGION"),
        Some("us".to_string())
    );
    assert_eq!(
        command_env(&command, "LD_LIBRARY_PATH"),
        Some(guest_library_path(&layout.staging_root))
    );
}

#[test]
fn grouped_c_subcases_keep_only_direct_usr_bin_commands() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "bugfix");
    case.test_commands = vec![
        "/usr/bin/alpha".to_string(),
        "/usr/bin/gamma --stress".to_string(),
    ];

    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
    let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
    let gamma = fake_c_subcase(root.path(), &case, "gamma-dir", &["gamma"]);
    let subcases = vec![&alpha, &beta, &gamma];

    let selected = selected_grouped_c_subcases(&case, subcases).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "gamma-dir"]
    );
}

#[test]
fn grouped_c_subcases_keep_all_dynamic_shell_commands() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "syscall");
    case.test_commands =
        vec!["for bin in /usr/bin/starry-test-suit/*; do \"$bin\"; done".to_string()];

    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
    let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
    let subcases = vec![&alpha, &beta];

    let selected = selected_grouped_c_subcases(&case, subcases).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
}

#[test]
fn grouped_c_subcases_prefer_explicit_filter() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "syscall");
    case.test_commands =
        vec!["for bin in /usr/bin/starry-test-suit/*; do \"$bin\"; done".to_string()];
    case.grouped_subcase_filter = Some(BTreeSet::from(["beta".to_string()]));

    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
    let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
    let subcases = vec![&alpha, &beta];

    let selected = selected_grouped_c_subcases(&case, subcases).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["beta"]
    );
}

#[test]
fn grouped_runner_commands_follow_explicit_subcase_filter_for_direct_commands() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "bugfix");
    case.test_commands = vec![
        "/usr/bin/alpha".to_string(),
        "/usr/bin/beta --stress".to_string(),
    ];
    case.grouped_subcase_filter = Some(BTreeSet::from(["beta-dir".to_string()]));

    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
    let beta = fake_c_subcase(root.path(), &case, "beta-dir", &["beta"]);
    let selected = selected_grouped_c_subcases(&case, vec![&alpha, &beta]).unwrap();
    let runner_commands = selected_grouped_runner_commands(&case, &selected).unwrap();

    assert_eq!(runner_commands, vec!["/usr/bin/beta --stress"]);
}

#[test]
fn grouped_runner_commands_keep_dynamic_shell_loop_with_explicit_filter() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "syscall");
    case.test_commands =
        vec!["for bin in /usr/bin/starry-test-suit/*; do \"$bin\"; done".to_string()];
    case.grouped_subcase_filter = Some(BTreeSet::from(["beta".to_string()]));

    let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
    let selected = selected_grouped_c_subcases(&case, vec![&beta]).unwrap();
    let runner_commands = selected_grouped_runner_commands(&case, &selected).unwrap();

    assert_eq!(runner_commands, case.test_commands);
}

#[test]
fn grouped_c_subcases_reject_missing_direct_usr_bin_commands() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "bugfix");
    case.test_commands = vec!["/usr/bin/missing".to_string()];

    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
    let err = selected_grouped_c_subcases(&case, vec![&alpha]).unwrap_err();

    assert!(
        err.to_string()
            .contains("references test command(s) without C subcases: missing")
    );
}

#[test]
fn cmake_configure_command_passes_staging_root_define() {
    let root = tempdir().unwrap();
    let case = fake_case(root.path(), "usb");
    let layout =
        case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
            .unwrap();
    let build_env = HostCrossBuildEnv {
        cmake: PathBuf::from("/usr/bin/cmake"),
        pkg_config: PathBuf::from("/usr/bin/pkg-config"),
        make_program: PathBuf::from("/usr/bin/make"),
        cmake_toolchain_file: PathBuf::from("/tmp/cmake-toolchain.cmake"),
        command_envs: vec![("PKG_CONFIG_LIBDIR".to_string(), "/sysroot".to_string())],
    };

    let config = fake_config();
    let command = build_cmake_configure_command(&case, &layout, &build_env, &config);
    let args = command_args(&command);

    assert_eq!(
        command.get_program(),
        std::ffi::OsStr::new("/usr/bin/cmake")
    );
    assert!(args.contains(&format!(
        "-DCMAKE_TOOLCHAIN_FILE={}",
        build_env.cmake_toolchain_file.display()
    )));
    assert!(args.contains(&format!(
        "-D{}={}",
        config.script_env.staging_root,
        layout.staging_root.display()
    )));
    assert_eq!(
        command_env(&command, "PKG_CONFIG_LIBDIR"),
        Some("/sysroot".to_string())
    );
}

#[test]
fn grouped_c_root_configure_command_passes_selected_subcase_list() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "bugfix");
    case.test_commands = vec!["/usr/bin/beta".to_string()];
    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
    let beta = fake_c_subcase(root.path(), &case, "beta-dir", &["beta"]);
    let subcases = [&alpha, &beta];
    let selected = selected_grouped_c_subcases(&case, subcases.to_vec()).unwrap();
    let layout =
        case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "bugfix")
            .unwrap();
    let build_env = HostCrossBuildEnv {
        cmake: PathBuf::from("/usr/bin/cmake"),
        pkg_config: PathBuf::from("/usr/bin/pkg-config"),
        make_program: PathBuf::from("/usr/bin/make"),
        cmake_toolchain_file: PathBuf::from("/tmp/cmake-toolchain.cmake"),
        command_envs: Vec::new(),
    };

    let command = build_grouped_c_root_project_configure_command(
        &case,
        &selected,
        subcases.len(),
        &layout,
        &build_env,
        &fake_config(),
    );
    let args = command_args(&command);

    assert!(args.contains(&"-DSTARRY_GROUPED_C_SUBCASES=beta-dir".to_string()));
}

#[test]
fn grouped_c_subcase_list_is_sorted_and_comma_separated() {
    let root = tempdir().unwrap();
    let case = fake_case(root.path(), "system");
    let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);

    assert_eq!(grouped_c_subcase_list(&[&beta, &alpha]), "alpha,beta");
}

#[test]
fn cross_compile_spec_maps_supported_arches() {
    assert_eq!(
        cross_compile_spec("aarch64").unwrap(),
        CrossCompileSpec {
            llvm_target: "aarch64-linux-musl",
            cmake_system_processor: "aarch64",
            guest_tool_dir: "usr/aarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "aarch64-linux-musl",
            qemu_user_binaries: &["qemu-aarch64-static", "qemu-aarch64"],
        }
    );
    assert_eq!(
        cross_compile_spec("loongarch64").unwrap(),
        CrossCompileSpec {
            llvm_target: "loongarch64-linux-musl",
            cmake_system_processor: "loongarch64",
            guest_tool_dir: "usr/loongarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "loongarch64-linux-musl",
            qemu_user_binaries: &["qemu-loongarch64-static", "qemu-loongarch64"],
        }
    );
}

#[test]
fn write_cross_bin_wrappers_generates_prefixed_and_plain_tools() {
    let root = tempdir().unwrap();
    let layout =
        case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
            .unwrap();
    fs::create_dir_all(
        layout
            .staging_root
            .join("usr/aarch64-alpine-linux-musl/bin"),
    )
    .unwrap();
    for tool in [
        "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
    ] {
        let path = layout
            .staging_root
            .join("usr/aarch64-alpine-linux-musl/bin")
            .join(tool);
        fs::write(path, b"").unwrap();
    }

    write_cross_bin_wrappers(
        &layout,
        cross_compile_spec("aarch64").unwrap(),
        Path::new("/usr/bin/qemu-aarch64-static"),
    )
    .unwrap();

    let plain = fs::read_to_string(layout.cross_bin_dir.join("ld")).unwrap();
    let prefixed = fs::read_to_string(layout.cross_bin_dir.join("aarch64-linux-musl-ld")).unwrap();
    assert!(plain.contains("qemu-aarch64-static"));
    assert!(plain.contains("LD_LIBRARY_PATH"));
    assert!(plain.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
    assert!(prefixed.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
    assert!(prefixed.contains("-0"));
}

#[test]
fn write_cmake_toolchain_file_contains_clang_cross_settings() {
    let root = tempdir().unwrap();
    let layout =
        case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
            .unwrap();
    fs::create_dir_all(&layout.cross_bin_dir).unwrap();
    fs::create_dir_all(
        layout
            .staging_root
            .join("usr/lib/gcc/aarch64-alpine-linux-musl/15.2.0"),
    )
    .unwrap();

    write_cmake_toolchain_file(
        &layout,
        cross_compile_spec("aarch64").unwrap(),
        Path::new("/usr/bin/clang"),
    )
    .unwrap();

    let content = fs::read_to_string(&layout.cmake_toolchain_file).unwrap();
    assert!(content.contains("set(CMAKE_SYSTEM_NAME Linux)"));
    assert!(content.contains("set(CMAKE_C_COMPILER \"/usr/bin/clang\")"));
    assert!(content.contains("set(CMAKE_C_COMPILER_TARGET \"aarch64-linux-musl\")"));
    assert!(content.contains("--gcc-toolchain="));
    assert!(content.contains("-B"));
    assert!(content.contains("-L"));
    assert!(content.contains("CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER"));
}

#[test]
fn detect_gcc_runtime_dir_prefers_highest_version() {
    let root = tempdir().unwrap();
    let sysroot = root.path().join("sysroot");
    let gcc_root = sysroot.join("usr/lib/gcc/aarch64-alpine-linux-musl");
    fs::create_dir_all(gcc_root.join("9.5.0")).unwrap();
    fs::create_dir_all(gcc_root.join("15.2.0")).unwrap();

    let selected = detect_gcc_runtime_dir(&sysroot, "usr/aarch64-alpine-linux-musl/bin").unwrap();
    assert_eq!(selected, gcc_root.join("15.2.0"));
}

#[test]
fn qemu_user_binary_names_cover_supported_arches() {
    assert_eq!(
        qemu_user_binary_names("aarch64").unwrap(),
        &["qemu-aarch64-static", "qemu-aarch64"]
    );
    assert_eq!(
        qemu_user_binary_names("riscv64").unwrap(),
        &["qemu-riscv64-static", "qemu-riscv64"]
    );
    assert_eq!(
        qemu_user_binary_names("x86_64").unwrap(),
        &["qemu-x86_64-static", "qemu-x86_64"]
    );
    assert_eq!(
        qemu_user_binary_names("loongarch64").unwrap(),
        &["qemu-loongarch64-static", "qemu-loongarch64"]
    );
}

#[test]
fn case_script_envs_include_expected_paths() {
    let root = tempdir().unwrap();
    let case = fake_case(root.path(), "usb");
    let layout =
        case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
            .unwrap();

    let envs = case_script_envs(&case, &layout, &fake_config());

    assert!(envs.contains(&(
        "SUITE_CASE_DIR".to_string(),
        case.case_dir.display().to_string()
    )));
    assert!(envs.contains(&(
        "SUITE_CASE_BUILD_DIR".to_string(),
        layout.build_dir.display().to_string()
    )));
}

#[test]
fn format_duration_like_summary_helpers_are_precise_enough() {
    assert_eq!(
        format!("{:.2}", Duration::from_millis(1250).as_secs_f64()),
        "1.25"
    );
}
