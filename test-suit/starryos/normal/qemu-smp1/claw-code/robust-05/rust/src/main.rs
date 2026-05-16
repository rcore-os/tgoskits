use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

const CLAW_PATH: &str = "/tmp/claw";
const API_BASE_URL: Option<&str> = option_env!("CLAW_API_BASE_URL");
const API_AUTH_TOKEN: Option<&str> = option_env!("CLAW_API_KEY");
const API_MODEL: Option<&str> = option_env!("CLAW_API_MODEL");

fn main() {
    let claw_bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/claw-binary"));
    fs::write(CLAW_PATH, claw_bytes).expect("failed to write claw binary");
    #[cfg(unix)] { fs::set_permissions(CLAW_PATH, std::fs::Permissions::from_mode(0o755)).ok(); }

    fs::create_dir_all("/tmp/work/.git/refs/heads").ok();
    fs::create_dir_all("/tmp/work/.git/objects").ok();
    fs::write("/tmp/work/.git/HEAD", "ref: refs/heads/master\n").ok();
    fs::write("/tmp/work/.git/config", "[core]\n\trepositoryformatversion = 0\n\tbare = false\n").ok();

    let env = format!("ANTHROPIC_BASE_URL={} ANTHROPIC_AUTH_TOKEN={} ANTHROPIC_MODEL={}",
        API_BASE_URL.unwrap_or("https://api.deepseek.com/anthropic"),
        API_AUTH_TOKEN.expect("CLAW_API_KEY env var not set at build time"),
        API_MODEL.unwrap_or("deepseek-chat"),
    );
    println!("=== Robust-05: generate first 50 prime numbers ===");
    let output = Command::new("sh").arg("-c").arg(format!("cd /tmp/work && {env} timeout 600 /tmp/claw --allowedTools bash,write prompt 'write a Python script that generates the first 50 prime numbers, run it, and save the output to primes.txt' 2>&1")).output().unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));

    if std::path::Path::new("/tmp/work/primes.txt").exists() {
        let s = fs::read_to_string("/tmp/work/primes.txt").unwrap_or_default();
        let last = s.lines().last().unwrap_or("");
        println!("primes.txt last: {last}");
        if last.contains("229") { println!("ALL_TESTS_DONE"); } else { println!("ALL_TESTS_FAILED"); std::process::exit(1); }
    } else { println!("ALL_TESTS_FAILED"); std::process::exit(1); }
}
