use std::{io::Result, path::PathBuf};

const LINKER_SCRIPT_NAME: &str = "linker.x";
const LINKER_TEMPLATE_NAME: &str = "linker.lds.S";

fn main() {
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");
    if let Ok(config_path) = std::env::var("AX_CONFIG_PATH") {
        println!("cargo:rerun-if-changed={config_path}");
    }

    if std::env::var_os("CARGO_FEATURE_AXVISOR_LINKER").is_some() {
        println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
        gen_axvisor_linker_script().unwrap();
    }
}

fn gen_axvisor_linker_script() -> Result<()> {
    let ld_content = include_str!("linker.lds.S")
        .replace("%ARCH%", "loongarch64")
        .replace(
            "%KERNEL_BASE_VADDR%",
            &format!("{:#x}", ax_config::plat::KERNEL_BASE_VADDR),
        )
        .replace(
            "%PHYS_VIRT_OFFSET%",
            &format!("{:#x}", ax_config::plat::PHYS_VIRT_OFFSET),
        )
        .replace(
            "%KERNEL_BASE_PADDR%",
            &format!("{:#x}", ax_config::plat::KERNEL_BASE_PADDR),
        )
        .replace("%CPU_NUM%", &format!("{}", ax_config::plat::MAX_CPU_NUM))
        .replace(
            "%ENTRY_PADDR%",
            &format!("{:#x}", ax_config::plat::KERNEL_BASE_PADDR + 0x40),
        );

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out_dir.display());
    std::fs::write(out_dir.join(LINKER_SCRIPT_NAME), ld_content)?;
    Ok(())
}
