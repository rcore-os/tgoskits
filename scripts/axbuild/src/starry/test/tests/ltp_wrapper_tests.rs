use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

#[test]
fn ltp_wrapper_rejects_successful_exit_before_all_required_results() {
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path();
    fs::create_dir_all(fixture.join("runtest")).unwrap();
    fs::create_dir_all(fixture.join("testcases/bin")).unwrap();
    fs::write(fixture.join("Version"), "20260529\n").unwrap();
    fs::write(fixture.join("runtest/syscalls"), "execve03 execve03\n").unwrap();

    let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-suit/starryos/qemu/system/ltp-syscalls");
    let template = fs::read_to_string(assets.join("wrapper.sh.in")).unwrap();
    let contracts = fs::read_to_string(assets.join("minimum-passes.txt")).unwrap();
    let required = contracts
        .lines()
        .find_map(|line| line.strip_prefix("execve03 "))
        .expect("the six-case execve03 completion contract must remain configured");
    assert_eq!(required, "6");
    let wrapper = fixture.join("wrapper.sh");
    // Relocate only the installation path; execute the actual wrapper logic.
    fs::write(
        &wrapper,
        template
            .replace("@LTP_CASE_ID@", "execve03")
            .replace("@LTP_MIN_PASSES@", required)
            .replace(
                "ltp_root=\"/opt/ltp\"",
                &format!("ltp_root=\"{}\"", fixture.display()),
            ),
    )
    .unwrap();
    let program = fixture.join("testcases/bin/execve03");
    for (passes, failed, exit_code, succeeds) in [
        (0, false, 0, false),
        (4, false, 0, false),
        (6, false, 0, true),
        (6, true, 0, false),
        (6, false, 42, false),
    ] {
        let mut script = String::from("#!/bin/sh\n");
        for _ in 0..passes {
            script.push_str("printf 'execve03.c:102: TPASS: completed errno check\\n'\n");
        }
        if failed {
            script.push_str("printf 'execve03.c:102: TFAIL: rejected result\\n'\n");
        }
        script.push_str(&format!("exit {exit_code}\n"));
        fs::write(&program, script).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new("sh").arg(&wrapper).output().unwrap();
        assert_eq!(
            output.status.success(),
            succeeds,
            "passes={passes}, failed={failed}, exit={exit_code}: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
