use std::{collections::BTreeSet, fs, path::Path};

use tempfile::tempdir;

use super::{grouped_c::*, toolchain::*, *};

fn fake_case(root: &Path, name: &str) -> TestQemuCase {
    let case_dir = root.join("test-suite/example/default").join(name);
    fs::create_dir_all(&case_dir).unwrap();
    TestQemuCase {
        name: name.to_string(),
        display_name: name.to_string(),
        case_dir: case_dir.clone(),
        qemu_config_path: case_dir.join("qemu-aarch64.toml"),
        test_commands: Vec::new(),
        grouped_command_selection: Default::default(),
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
    assert!(selected.iter().any(|subcase| subcase.name == "alpha"));
    assert!(selected.iter().any(|subcase| subcase.name == "gamma-dir"));
    assert!(selected.iter().all(|subcase| subcase.name != "beta"));
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
    assert!(selected.iter().any(|subcase| subcase.name == "alpha"));
    assert!(selected.iter().any(|subcase| subcase.name == "beta"));
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
    assert!(selected.iter().any(|subcase| subcase.name == "beta"));
    assert!(selected.iter().all(|subcase| subcase.name != "alpha"));
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

    assert!(
        runner_commands
            .iter()
            .any(|command| command == "/usr/bin/beta --stress")
    );
    assert!(
        runner_commands
            .iter()
            .all(|command| command != "/usr/bin/alpha")
    );
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
fn grouped_runner_commands_preserve_explicit_aggregator_with_subcase_filter() {
    let root = tempdir().unwrap();
    let mut case = fake_case(root.path(), "system");
    case.test_commands = vec!["/usr/bin/starry-run-system-tests".to_string()];
    case.grouped_command_selection = GroupedCommandSelection::PreserveAll;
    case.grouped_subcase_filter = Some(BTreeSet::from(["beta".to_string()]));

    let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
    let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
    let selected = selected_grouped_c_subcases(&case, vec![&alpha, &beta]).unwrap();
    let runner_commands = selected_grouped_runner_commands(&case, &selected).unwrap();

    assert_eq!(
        selected
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<Vec<_>>(),
        vec!["beta"]
    );
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
