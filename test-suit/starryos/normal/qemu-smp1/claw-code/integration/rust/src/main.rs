use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

const CLAW_PATH: &str = "/tmp/claw";

// --- Configuration ---
fn claw_env() -> Option<String> {
    let api_key = std::env::var("CLAW_API_KEY").ok()?;
    let api_base = std::env::var("CLAW_API_BASE_URL")
        .unwrap_or_else(|_| "https://api.deepseek.com/anthropic".into());
    let api_model =
        std::env::var("CLAW_API_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    Some(format!(
        "ANTHROPIC_BASE_URL={api_base} ANTHROPIC_AUTH_TOKEN={api_key} ANTHROPIC_MODEL={api_model}",
    ))
}



fn main() {
    // Extract embedded claw binary (provided by build.rs from local file or GitHub release)
    let claw_bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/claw-binary"));
    fs::write(CLAW_PATH, claw_bytes).expect("failed to write claw binary");
    let mut perms = fs::metadata(CLAW_PATH)
        .expect("failed to stat claw")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(CLAW_PATH, perms).expect("failed to chmod claw");

    run_test("/tmp/claw --help", "=== Smoke: claw --help ===");
    run_test("/tmp/claw version", "=== Diagnostic: claw version ===");

    // Create fake git repo so claw sees a normal git environment
    fs::create_dir_all("/tmp/work/.git/refs/heads").ok();
    fs::create_dir_all("/tmp/work/.git/objects").ok();
    fs::write("/tmp/work/.git/HEAD", "ref: refs/heads/master\n").ok();
    fs::write("/tmp/work/.git/config", "[core]\n\trepositoryformatversion = 0\n\tbare = false\n").ok();

    let Some(env) = claw_env() else {
        println!("CLAW_SKIP: CLAW_API_KEY not set");
        std::process::exit(0);
    };

    // Functional: basic prompt
    if !run_test_with_retry(
        &format!("cd /tmp/work && {env} timeout 300 /tmp/claw prompt 'say just the word ok and nothing else' 2>&1"),
        "=== Functional: claw prompt ===",
        3,
    ) {
        println!("ALL_TESTS_FAILED: functional");
        std::process::exit(1);
    }

    // Tool test: bash echo
    if !run_test_with_retry(
        &format!("cd /tmp/work && {env} timeout 300 /tmp/claw --allowedTools bash prompt 'use bash to echo hello world' 2>&1"),
        "=== Tool: bash echo ===",
        3,
    ) {
        println!("ALL_TESTS_FAILED: bash");
        std::process::exit(1);
    }

    // Small project: create a file
    if !run_test_with_retry(
        &format!("cd /tmp/work && {env} timeout 300 /tmp/claw --allowedTools bash,write prompt 'create a file /tmp/claw-hello.txt containing exactly: hello from claw' 2>&1"),
        "=== Project: create file ===",
        3,
    ) {
        println!("ALL_TESTS_FAILED: create file");
        std::process::exit(1);
    }
    run_test("cat /tmp/claw-hello.txt 2>&1", "=== Verify: cat /tmp/claw-hello.txt ===");

    // C project: write, compile, and run a program
    if !run_test_with_retry(
        &format!("cd /tmp/work && {env} timeout 300 /tmp/claw --allowedTools bash,write prompt 'write a C program /tmp/hello.c that prints \"StarryOS CLAW OK\", compile it, and run it' 2>&1"),
        "=== Project: C compile & run ===",
        3,
    ) {
        println!("ALL_TESTS_FAILED: C compile");
        std::process::exit(1);
    }
    run_test("cat /tmp/hello.c 2>&1", "=== Verify: cat /tmp/hello.c ===");

    println!("ALL_TESTS_DONE");
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

fn run_test_with_retry(cmd: &str, label: &str, max_retries: u32) -> bool {
    for attempt in 1..=max_retries {
        println!("{label} (attempt {}/{})", attempt, max_retries);
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .expect("failed to execute test command");
        let status = output.status;
        std::io::stdout().write_all(&output.stdout).unwrap();
        std::io::stderr().write_all(&output.stderr).unwrap();
        let code = status.code().unwrap_or(-1);
        println!("EXIT:{}", code);
        if code == 0 {
            return true;
        }
        if attempt < max_retries {
            println!("Retrying...");
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    false
}
