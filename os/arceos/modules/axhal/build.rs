use std::{io::Result, path::PathBuf};

const LINKER_SCRIPT_NAME: &str = "linker.x";

fn main() {
    println!("cargo:rustc-check-cfg=cfg(plat_dyn)");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let has_plat_dyn = std::env::var_os("CARGO_FEATURE_PLAT_DYN").is_some();
    let platform = ax_config::PLATFORM;

    if has_plat_dyn && target_os == "none" {
        println!("cargo:rustc-cfg=plat_dyn");
    }

    if platform != "dummy" {
        gen_linker_script(&arch, platform).unwrap();
    }
}

fn gen_linker_script(arch: &str, platform: &str) -> Result<()> {
    let is_loongarch_qemu_virt = arch == "loongarch64" && platform == "loongarch64-qemu-virt";
    let legacy_fname = format!("linker_{platform}.lds");
    let output_arch = if arch == "x86_64" {
        "i386:x86-64"
    } else if arch.contains("riscv") {
        "riscv" // OUTPUT_ARCH of both riscv32/riscv64 is "riscv"
    } else {
        arch
    };
    let linker_constants = if is_loongarch_qemu_virt {
        format!(
            "PHYS_VIRT_OFFSET = {:#x};\nKERNEL_BASE_PADDR = {:#x};",
            ax_config::plat::PHYS_VIRT_OFFSET,
            ax_config::plat::KERNEL_BASE_PADDR
        )
    } else {
        String::new()
    };
    let entry_directive = if is_loongarch_qemu_virt {
        format!(
            "EXTERN(_start)\nENTRY({:#x})",
            ax_config::plat::KERNEL_BASE_PADDR + 0x40
        )
    } else {
        "ENTRY(_start)".to_string()
    };
    let ld_content = std::fs::read_to_string("linker.lds.S")?;
    let ld_content = ld_content.replace("%ARCH%", output_arch);
    let ld_content = ld_content.replace(
        "%KERNEL_BASE%",
        &format!("{:#x}", ax_config::plat::KERNEL_BASE_VADDR),
    );
    let ld_content = ld_content.replace("%LINKER_CONSTANTS%", &linker_constants);
    let ld_content = ld_content.replace("%ENTRY_DIRECTIVE%", &entry_directive);
    let ld_content = ld_content.replace(
        "%TEXT_AT%",
        if is_loongarch_qemu_virt {
            " : AT(ADDR(.text) - PHYS_VIRT_OFFSET)"
        } else {
            ""
        },
    );
    let ld_content = ld_content.replace(
        "%RODATA_AT%",
        if is_loongarch_qemu_virt {
            " : AT(ADDR(.rodata) - PHYS_VIRT_OFFSET)"
        } else {
            ""
        },
    );
    let ld_content = ld_content.replace(
        "%INIT_ARRAY_AT%",
        if is_loongarch_qemu_virt {
            " : AT(ADDR(.init_array) - PHYS_VIRT_OFFSET)"
        } else {
            ""
        },
    );
    let ld_content = ld_content.replace(
        "%DATA_AT%",
        if is_loongarch_qemu_virt {
            " : AT(ADDR(.data) - PHYS_VIRT_OFFSET)"
        } else {
            ""
        },
    );
    let ld_content = ld_content.replace(
        "%LINKME_SECTION%",
        if is_loongarch_qemu_virt {
            r#"
    /*
     * Keep trap-handler slices in the higher-half image. If the linker leaves
     * them orphaned at VMA 0, the LoongArch trap dispatcher can jump to 0x0
     * after interrupts are enabled.
     */
    . = ALIGN(8);
    .linkme : AT(ADDR(.linkme) - PHYS_VIRT_OFFSET) {
        __start_linkme_PAGE_FAULT = .;
        KEEP(*(linkme_PAGE_FAULT))
        __stop_linkme_PAGE_FAULT = .;
        __start_linkme_IRQ = .;
        KEEP(*(linkme_IRQ))
        __stop_linkme_IRQ = .;
    }
"#
        } else {
            ""
        },
    );
    let ld_content = ld_content.replace(
        "%TDATA_AT%",
        if is_loongarch_qemu_virt {
            " : AT(ADDR(.tdata) - PHYS_VIRT_OFFSET)"
        } else {
            ""
        },
    );
    let ld_content = ld_content.replace(
        "%TBSS_AT%",
        if is_loongarch_qemu_virt {
            " : AT(ADDR(.tbss) - PHYS_VIRT_OFFSET)"
        } else {
            ""
        },
    );
    let ld_content = ld_content.replace(
        "%PERCPU_SECTION%",
        if is_loongarch_qemu_virt {
            r#"    . = ALIGN(64);
    /*
     * LoongArch QEMU direct boot derives PT_LOAD copy addresses from the VMA
     * rather than p_paddr, so `.percpu` must keep a higher-half VMA here.
     */
    .percpu : AT(ADDR(.percpu) - PHYS_VIRT_OFFSET) {
        _percpu_start = .;
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        _percpu_load_end_aligned = ALIGN(64);
        . = _percpu_start + (_percpu_load_end_aligned - _percpu_load_start) * %CPU_NUM%;
    }
    _percpu_end = .;"#
        } else {
            r#"    . = ALIGN(4K);
    _percpu_start = .;
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    .percpu 0x0 : AT(_percpu_start) {
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        . = _percpu_load_start + ALIGN(64) * %CPU_NUM%;
    }
    . = _percpu_end;"#
        },
    );
    let ld_content = ld_content.replace(
        "%BSS_AT%",
        if is_loongarch_qemu_virt {
            " : AT(ADDR(.bss) - PHYS_VIRT_OFFSET)"
        } else {
            " : AT(.)"
        },
    );
    let ld_content = ld_content.replace("%CPU_NUM%", &format!("{}", ax_config::plat::MAX_CPU_NUM));
    let ld_content = ld_content.replace(
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
    );

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rustc-link-arg=-T{LINKER_SCRIPT_NAME}");

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out/linker.x
    std::fs::write(out_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;

    // Keep a stable copy under target/<target_triple>/<mode>/ for callers
    // that still link outside Cargo build-script search paths.
    let target_dir = out_dir.join("../../..");
    std::fs::write(target_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;
    std::fs::write(target_dir.join(legacy_fname), ld_content)?;
    Ok(())
}
