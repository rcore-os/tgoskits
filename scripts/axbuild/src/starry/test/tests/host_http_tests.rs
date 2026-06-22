use super::*;

#[test]
fn starry_qemu_case_starts_host_http_server_from_loaded_config() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
    fs::create_dir_all(&case_dir).unwrap();
    let test_case = TestQemuCase {
        name: "qemu-smp1/system".to_string(),
        display_name: "qemu-smp1/system".to_string(),
        case_dir: case_dir.clone(),
        qemu_config_path: case_dir.join("qemu-x86_64.toml"),
        test_commands: Vec::new(),
        host_symbolize_success_regex: Vec::new(),
        host_http_server: Some(case::HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
            body: "fixture".to_string(),
            body_size: Some(4),
            body_byte: b'Z',
        }),
        subcases: Vec::new(),
        grouped_subcase_filter: None,
    };

    let guard = start_qemu_case_host_http_server(&test_case).unwrap();

    assert!(guard.is_some());
}

#[test]
fn starry_qemu_single_subcase_skips_unneeded_host_http_server() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
    let subcase_dir = case_dir.join("syscall-test-uid-gid-re-setters");
    fs::create_dir_all(subcase_dir.join("src")).unwrap();
    fs::write(
        subcase_dir.join("src/main.c"),
        "int main(void) { return 0; }\n",
    )
    .unwrap();
    let test_case = grouped_host_http_test_case(
        &case_dir,
        Some(BTreeSet::from([
            "syscall-test-uid-gid-re-setters".to_string()
        ])),
    );

    let guard = start_qemu_case_host_http_server(&test_case).unwrap();

    assert!(guard.is_none());
}

#[test]
fn starry_qemu_single_subcase_keeps_needed_host_http_server() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
    let subcase_dir = case_dir.join("apk-curl-equivalence");
    fs::create_dir_all(subcase_dir.join("src")).unwrap();
    fs::write(
        subcase_dir.join("src/apk-curl-equivalence.sh"),
        "curl -fsSL http://10.0.2.2:18380/payload.bin\n",
    )
    .unwrap();
    let mut test_case = grouped_host_http_test_case(
        &case_dir,
        Some(BTreeSet::from(["apk-curl-equivalence".to_string()])),
    );
    test_case.host_http_server.as_mut().unwrap().port = 0;

    let guard = start_qemu_case_host_http_server(&test_case).unwrap();

    assert!(guard.is_some());
}

#[test]
fn busybox_guest_script_reports_case_start_and_bounds_nologin() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script_path = workspace_root.join("apps/starry/qemu/busybox/sh/busybox-tests.sh");
    let script = fs::read_to_string(&script_path).unwrap();

    assert!(
        script.contains("echo \"START: $BB_CASE_NAME\""),
        "{} must print case start markers so CI timeout logs identify the hanging BusyBox applet",
        script_path.display()
    );
    assert!(
        script.contains("timeout 2 busybox nologin"),
        "{} must run nologin in the foreground under a timeout",
        script_path.display()
    );
    assert!(
        !script.contains("busybox nologin >/tmp/bb_nologin.out 2>&1 &"),
        "{} must not leave the nologin probe as a background child",
        script_path.display()
    );
}
