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
        ltp_case_id: None,
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
fn write_musl_loader_search_path_only_when_guest_loader_exists() {
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

    write_musl_loader_search_path("aarch64", &staging_root).unwrap();

    assert!(!staging_root.join("etc/ld-musl-aarch64.path").exists());
    assert!(staging_root.join("etc/ld-musl-riscv64.path").exists());
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
fn detect_gcc_runtime_dir_prefers_highest_version() {
    let root = tempdir().unwrap();
    let sysroot = root.path().join("sysroot");
    let gcc_root = sysroot.join("usr/lib/gcc/aarch64-alpine-linux-musl");
    fs::create_dir_all(gcc_root.join("9.5.0")).unwrap();
    fs::create_dir_all(gcc_root.join("15.2.0")).unwrap();

    let selected = detect_gcc_runtime_dir(&sysroot, "usr/aarch64-alpine-linux-musl/bin").unwrap();
    assert_eq!(selected, gcc_root.join("15.2.0"));
}
