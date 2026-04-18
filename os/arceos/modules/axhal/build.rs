use std::{io::Result, path::PathBuf};

const LINKER_SCRIPT_NAME: &str = "linker.x";
const LINKER_TEMPLATE_NAME: &str = "linker.lds.S";
const AXVISOR_LOONGARCH_LINKER_TEMPLATE_NAME: &str = "linker_axvisor_loongarch64.lds.S";

fn main() {
    println!("cargo:rustc-check-cfg=cfg(plat_dyn)");
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-changed={AXVISOR_LOONGARCH_LINKER_TEMPLATE_NAME}");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let has_plat_dyn = std::env::var_os("CARGO_FEATURE_PLAT_DYN").is_some();
    let has_axvisor_linker = std::env::var_os("CARGO_FEATURE_AXVISOR_LINKER").is_some();
    let platform = ax_config::PLATFORM;

    if has_plat_dyn && target_os == "none" {
        println!("cargo:rustc-cfg=plat_dyn");
    }

    if platform != "dummy" {
        gen_linker_script(&arch, platform, has_axvisor_linker).unwrap();
    }
}

fn gen_linker_script(arch: &str, platform: &str, has_axvisor_linker: bool) -> Result<()> {
    let legacy_fname = format!("linker_{platform}.lds");
    let use_axvisor_loongarch_linker = has_axvisor_linker && arch == "loongarch64";
    let ld_content = if use_axvisor_loongarch_linker {
        std::fs::read_to_string(AXVISOR_LOONGARCH_LINKER_TEMPLATE_NAME)?
            .replace("%ARCH%", arch)
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
            )
    } else {
        let output_arch = if arch == "x86_64" {
            "i386:x86-64"
        } else if arch.contains("riscv") {
            "riscv" // OUTPUT_ARCH of both riscv32/riscv64 is "riscv"
        } else {
            arch
        };
        std::fs::read_to_string(LINKER_TEMPLATE_NAME)?
            .replace("%ARCH%", output_arch)
            .replace(
                "%KERNEL_BASE%",
                &format!("{:#x}", ax_config::plat::KERNEL_BASE_VADDR),
            )
            .replace("%CPU_NUM%", &format!("{}", ax_config::plat::MAX_CPU_NUM))
            .replace(
                "%DWARF%",
                if std::env::var("DWARF").is_ok_and(|v| v == "y") {
                    r#"debug_abbrev : { . += SIZEOF(.debug_abbrev); }
    debug_addr : { . += SIZEOF(.debug_addr); }
    debug_aranges : { . += SIZEOF(.debug_aranges); }
    debug_info : { . += SIZEOF(.debug_info); }
    debug_line : { . += SIZEOF(.debug_line); }
    debug_line_str : { . += SIZEOF(.debug_line_str); }
    debug_ranges : { . += SIZEOF(.debug_ranges); }
    debug_rnglists : { . += SIZEOF(.debug_rnglists); }
    debug_str : { . += SIZEOF(.debug_str); }
    debug_str_offsets : { . += SIZEOF(.debug_str_offsets); }"#
                } else {
                    ""
                },
            )
    };

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rustc-link-arg=-T{LINKER_SCRIPT_NAME}");

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out/linker.x
    std::fs::write(out_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;

    if !use_axvisor_loongarch_linker {
        // Keep a stable copy under target/<target_triple>/<mode>/ for callers
        // that still link outside Cargo build-script search paths.
        let target_dir = out_dir.join("../../..");
        std::fs::write(target_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;
        std::fs::write(target_dir.join(legacy_fname), ld_content)?;
    }

    Ok(())
}
