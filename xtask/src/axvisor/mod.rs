use std::{path::Path, process::Command};

use anyhow::{Context, bail};

pub fn run_test(target: &str) -> anyhow::Result<()> {
    if target.contains("aarch64") {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("failed to locate workspace root")?;
        let args = [
            "axvisor",
            "qemu",
            "--build-config",
            "os/axvisor/configs/board/qemu-aarch64.toml",
            "--qemu-config",
            "xtask/src/axvisor/qemu-aarch64.toml",
        ];

        println!("running: cargo {}", args.join(" "));
        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args(args)
            .status()
            .context("failed to spawn `cargo axvisor qemu`")?;

        if !status.success() {
            bail!("`cargo {}` failed with status {}", args.join(" "), status);
        }
    }

    Ok(())
}
