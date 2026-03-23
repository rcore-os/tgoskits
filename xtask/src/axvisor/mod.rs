use std::path::Path;

use anyhow::Context;

pub async fn run_test_qemu(target: Option<String>) -> anyhow::Result<()> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("failed to locate workspace root")?;
    let axvisor_dir = workspace_root.join("os/axvisor");
    axbuild::axvisor::xtest::run_test_qemu(target, axvisor_dir).await?;

    // if target.contains("aarch64") {
    //     let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
    //         .parent()
    //         .context("failed to locate workspace root")?;
    //     let args = [
    //         "axvisor",
    //         "qemu",
    //         "--build-config",
    //         "os/axvisor/configs/board/qemu-aarch64.toml",
    //         "--qemu-config",
    //         "xtask/src/axvisor/qemu-aarch64.toml",
    //     ];

    //     println!("running: cargo {}", args.join(" "));
    //     let status = Command::new("cargo")
    //         .current_dir(workspace_root)
    //         .args(args)
    //         .status()
    //         .context("failed to spawn `cargo axvisor qemu`")?;

    //     if !status.success() {
    //         bail!("`cargo {}` failed with status {}", args.join(" "), status);
    //     }
    // }

    Ok(())
}
