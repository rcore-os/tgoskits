use std::{path::PathBuf, process::Command};

#[test]
fn run_all_tests_does_not_exit_after_first_status_count() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_flow = repo_root.join("scripts/.axci/lib/test_flow.sh");

    let output = Command::new("bash")
        .arg("-c")
        .arg(format!(
            r#"
set -e
source "{}"
OPT_PARALLEL=false
get_test_targets() {{ echo "first second"; }}
run_test_target() {{ return 0; }}
collect_target_status() {{
    if [[ "$1" == "first" ]]; then
        echo "passed"
    else
        echo "skipped"
    fi
}}
generate_report() {{ :; }}
log() {{ :; }}
run_all_tests
"#,
            test_flow.display()
        ))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("  - 通过: 1"), "{stdout}");
    assert!(stdout.contains("  - 跳过: 1"), "{stdout}");
    assert!(stdout.contains("  - 失败: 0"), "{stdout}");
}
