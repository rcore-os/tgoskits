use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn main() {
    // Extract embedded claw binary
    let claw_bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/claw-binary"));
    let claw_path = "/tmp/claw";
    fs::write(claw_path, claw_bytes).expect("failed to write claw binary");
    let mut perms = fs::metadata(claw_path)
        .expect("failed to stat claw")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(claw_path, perms).expect("failed to chmod claw");

    run_test("/tmp/claw --help", "=== Smoke: claw --help ===");
    run_test("/tmp/claw version", "=== Diagnostic: claw version ===");
    run_test("/tmp/claw doctor 2>&1", "=== Diagnostic: claw doctor ===");
}

fn run_test(cmd: &str, label: &str) {
    println!("{label}");
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .expect("failed to execute test command");
    let status = output.status;
    std::io::stdout().write_all(&output.stdout).unwrap();
    std::io::stderr().write_all(&output.stderr).unwrap();
    println!("EXIT:{}", status.code().unwrap_or(-1));
}
