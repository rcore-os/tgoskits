use std::process::Command;

#[test]
fn execute_lock_names_the_selected_shared_usb_controller() {
    let output = Command::new(env!("CARGO_BIN_EXE_ivc-vision-actuator"))
        .arg("--execute")
        .output()
        .expect("run ivc-vision-actuator");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("actuator stderr is UTF-8");
    assert!(
        stderr.contains("/usb@fc880000"),
        "unexpected stderr: {stderr}"
    );
    assert!(!stderr.contains("fc400000"), "stale topology: {stderr}");
}
