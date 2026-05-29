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
    #[cfg(unix)]
    {
        fs::set_permissions(CLAW_PATH, std::fs::Permissions::from_mode(0o755)).ok();
    }

    fs::create_dir_all("/tmp/work/.git/refs/heads").ok();
    fs::create_dir_all("/tmp/work/.git/objects").ok();
    fs::write("/tmp/work/.git/HEAD", "ref: refs/heads/master\n").ok();
    fs::write(
        "/tmp/work/.git/config",
        "[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
    )
    .ok();

    let Some(env) = claw_env() else {
        println!("CLAW_SKIP: CLAW_API_KEY not set");
        std::process::exit(0);
    };

    println!("=== Robust-12: multi-agent — spawn 2 sub-agents in parallel ===");
    let (code, _out, _err) = run(&format!(
        "cd /tmp/work && {env} timeout 600 /tmp/claw --allowedTools bash,write,Agent prompt 'Use two sub-agents in parallel. \
         Sub-agent 1: create file /tmp/work/a.txt with content \"apple\". \
         Sub-agent 2: create file /tmp/work/b.txt with content \"banana\". \
         After both finish, create /tmp/work/merged.txt combining both contents on separate lines. \
         Say exactly DONE when complete.' 2>&1"
    ));
    println!("EXIT:{code}");

    let a_exists = std::path::Path::new("/tmp/work/a.txt").exists();
    let b_exists = std::path::Path::new("/tmp/work/b.txt").exists();
    let merged_exists = std::path::Path::new("/tmp/work/merged.txt").exists();
    println!("a.txt exists: {a_exists}");
    println!("b.txt exists: {b_exists}");
    println!("merged.txt exists: {merged_exists}");

    if a_exists && b_exists && merged_exists {
        let merged = fs::read_to_string("/tmp/work/merged.txt").unwrap_or_default();
        println!("merged.txt content: {merged:?}");
        println!("ALL_TESTS_DONE");
    } else {
        println!("ALL_TESTS_FAILED");
        std::process::exit(1);
    }
}

fn run(cmd: &str) -> (i32, String, String) {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .expect("command failed");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    print!("{stdout}");
    eprint!("{stderr}");
    (code, stdout, stderr)
}
