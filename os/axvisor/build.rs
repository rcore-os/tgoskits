use std::{env, fs, path::PathBuf};

use anyhow::Context;

fn fallback_platform_for_arch(arch: &str) -> &'static str {
    match arch {
        "aarch64" => "aarch64-generic",
        "loongarch64" => "loongarch64-plat-dyn",
        "x86_64" => "dummy",
        "riscv64" => "riscv64-plat-dyn",
        _ => "dummy",
    }
}

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=linker.ld");
    let out_dir = PathBuf::from(env::var("OUT_DIR").context("OUT_DIR is not set")?);
    let linker = out_dir.join("linker.x");
    fs::write(&linker, include_str!("linker.ld"))?;
    println!("cargo:rustc-link-search={}", out_dir.display());
    fs::write(
        out_dir.join("../../..").join("linker.x"),
        include_str!("linker.ld"),
    )?;

    let arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").context("CARGO_CFG_TARGET_ARCH is not set")?;

    let platform = fallback_platform_for_arch(&arch);

    println!("cargo:rustc-cfg=platform=\"{platform}\"");

    Ok(())
}
