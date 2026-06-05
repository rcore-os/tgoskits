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
    fs::set_permissions(CLAW_PATH, fs::metadata(CLAW_PATH).unwrap().permissions()).ok();
    #[cfg(unix)] { fs::set_permissions(CLAW_PATH, std::fs::Permissions::from_mode(0o755)).ok(); }

    fs::create_dir_all("/tmp/work/.git/refs/heads").ok();
    fs::create_dir_all("/tmp/work/.git/objects").ok();
    fs::write("/tmp/work/.git/HEAD", "ref: refs/heads/master\n").ok();
    fs::write("/tmp/work/.git/config", "[core]\n\trepositoryformatversion = 0\n\tbare = false\n").ok();

    let Some(env) = claw_env() else {
        println!("CLAW_SKIP: CLAW_API_KEY not set");
        std::process::exit(0);
    };
    println!("=== Robust-01: search LLM agent papers, write digest ===");
    let (code, _out, _err) = run(&format!("cd /tmp/work && {env} timeout 600 /tmp/claw --allowedTools bash,write prompt 'search for llm agent papers and write a digest to digest.md' 2>&1"));
    println!("EXIT:{code}");

    if std::path::Path::new("/tmp/work/digest.md").exists() {
        let content = fs::read_to_string("/tmp/work/digest.md").unwrap_or_default();
        println!("digest.md exists with {} lines", content.lines().count());
        println!("ALL_TESTS_DONE");
    } else {
        println!("digest.md NOT found");
        println!("ALL_TESTS_FAILED");
        std::process::exit(1);
    }
}

fn run(cmd: &str) -> (i32, String, String) {
    let output = Command::new("sh").arg("-c").arg(cmd).output().expect("command failed");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    print!("{stdout}");
    eprint!("{stderr}");
    (code, stdout, stderr)
}
