use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

const CLAW_PATH: &str = "/tmp/claw";
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
    let claw_bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/claw-binary"));
    fs::write(CLAW_PATH, claw_bytes).expect("failed to write claw binary");
    #[cfg(unix)] { fs::set_permissions(CLAW_PATH, std::fs::Permissions::from_mode(0o755)).ok(); }

    fs::create_dir_all("/tmp/work/.git/refs/heads").ok();
    fs::create_dir_all("/tmp/work/.git/objects").ok();
    fs::write("/tmp/work/.git/HEAD", "ref: refs/heads/master\n").ok();
    fs::write("/tmp/work/.git/config", "[core]\n\trepositoryformatversion = 0\n\tbare = false\n").ok();

    let Some(env) = claw_env() else {
        println!("CLAW_SKIP: CLAW_API_KEY not set");
        std::process::exit(0);
    };
    println!("=== Robust-02: count files recursively, write count ===");
    let output = Command::new("sh").arg("-c").arg(format!("cd /tmp/work && {env} timeout 300 /tmp/claw --allowedTools bash,write prompt 'use bash to count all files recursively in /tmp, and write the number to count.txt' 2>&1")).output().unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));

    if std::path::Path::new("/tmp/work/count.txt").exists() {
        let s = fs::read_to_string("/tmp/work/count.txt").unwrap_or_default();
        println!("count.txt exists: {s}");
        println!("ALL_TESTS_DONE");
    } else {
        println!("ALL_TESTS_FAILED");
        std::process::exit(1);
    }
}
