use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let claw_out = out_dir.join("claw-binary");

    let url = "https://github.com/MuZhao2333/tgoskits/releases/download/claw-code-binary/claw";
    let status = std::process::Command::new("curl")
        .args(["-sL", "-o", claw_out.to_str().unwrap(), url])
        .status()
        .expect("failed to run curl to download claw binary");
    if !status.success() {
        panic!(
            "curl failed with exit code {}",
            status.code().unwrap_or(-1)
        );
    }
    println!("cargo:warning=downloaded claw binary from GitHub release");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&claw_out).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&claw_out, perms).unwrap();
    }

    println!("cargo:rerun-if-changed=build.rs");
}
