use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn test_script_forces_host_target_for_axci_runner() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script_path = repo_root.join("scripts/test.sh");
    let temp_dir = unique_temp_dir("arm-vcpu-script-test");
    let fake_axci_dir = temp_dir.join(".axci");
    let marker_path = temp_dir.join("invocation.txt");

    fs::create_dir_all(&fake_axci_dir).unwrap();
    write_executable(
        &fake_axci_dir.join("tests.sh"),
        &format!(
            "#!/bin/bash\nset -e\nprintf 'target=%s\\n' \"$CARGO_BUILD_TARGET\" > \"{}\"\nprintf 'pwd=%s\\n' \"$PWD\" >> \"{}\"\nprintf 'args=%s\\n' \"$*\" >> \"{}\"\n",
            marker_path.display(),
            marker_path.display(),
            marker_path.display()
        ),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source \"{}\"; AXCI_DIR=\"{}\"; COMPONENT_DIR=\"{}\"; run_axci_tests --dry-run",
            script_path.display(),
            fake_axci_dir.display(),
            repo_root.display()
        ))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "script failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let invocation = fs::read_to_string(&marker_path).unwrap();
    let host = String::from_utf8(Command::new("rustc").args(["-vV"]).output().unwrap().stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap()
        .to_string();

    assert!(
        invocation.contains(&format!("target={host}")),
        "{invocation}"
    );
    assert!(
        invocation.contains(&format!("pwd={}", repo_root.display())),
        "{invocation}"
    );
    assert!(
        invocation.contains(&format!(
            "args=--component-dir {} --dry-run",
            repo_root.display()
        )),
        "{invocation}"
    );
}
