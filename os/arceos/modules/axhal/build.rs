use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::{Path, PathBuf},
};

const LINKER_SCRIPT_NAME: &str = "linker.x";
const LINKER_TEMPLATE_NAME: &str = "linker.lds.S";

fn main() {
    println!("cargo:rustc-check-cfg=cfg(plat_dyn)");
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let has_plat_dyn = std::env::var_os("CARGO_FEATURE_PLAT_DYN").is_some();
    let has_axvisor_linker = std::env::var_os("CARGO_FEATURE_AXVISOR_LINKER").is_some();
    let config = load_linker_config().unwrap();

    if has_plat_dyn && target_os == "none" {
        println!("cargo:rustc-cfg=plat_dyn");
    }

    if config.platform != "dummy" {
        gen_linker_script(&arch, &config, has_axvisor_linker).unwrap();
    }
}

#[derive(Debug)]
struct LinkerConfig {
    platform: String,
    kernel_base_vaddr: usize,
    max_cpu_num: usize,
    phys_virt_offset: usize,
    kernel_base_paddr: usize,
}

fn load_linker_config() -> Result<LinkerConfig> {
    match env::var("AX_CONFIG_PATH") {
        Ok(path) => {
            println!("cargo:rerun-if-changed={path}");
            read_linker_config(Path::new(&path))
        }
        Err(_) => Ok(LinkerConfig {
            platform: ax_config::PLATFORM.to_string(),
            kernel_base_vaddr: ax_config::plat::KERNEL_BASE_VADDR,
            max_cpu_num: ax_config::plat::MAX_CPU_NUM,
            phys_virt_offset: ax_config::plat::PHYS_VIRT_OFFSET,
            kernel_base_paddr: ax_config::plat::KERNEL_BASE_PADDR,
        }),
    }
}

fn read_linker_config(path: &Path) -> Result<LinkerConfig> {
    let content = fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&content).map_err(invalid_data)?;
    Ok(LinkerConfig {
        platform: get_string(&value, &["platform"])?,
        kernel_base_vaddr: get_usize(&value, &["plat", "kernel-base-vaddr"])?,
        max_cpu_num: get_usize(&value, &["plat", "max-cpu-num"])?,
        phys_virt_offset: get_usize(&value, &["plat", "phys-virt-offset"])?,
        kernel_base_paddr: get_usize(&value, &["plat", "kernel-base-paddr"])?,
    })
}

fn get_string(value: &toml::Value, keys: &[&str]) -> Result<String> {
    let value = get_value(value, keys)?;
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| invalid_data(format!("{} must be a string", keys.join("."))))
}

fn get_usize(value: &toml::Value, keys: &[&str]) -> Result<usize> {
    let value = get_value(value, keys)?;
    match value {
        toml::Value::Integer(value) => usize::try_from(*value)
            .map_err(|_| invalid_data(format!("{} is out of range", keys.join(".")))),
        toml::Value::String(value) => parse_usize(value)
            .map_err(|err| invalid_data(format!("failed to parse {}: {err}", keys.join(".")))),
        _ => Err(invalid_data(format!(
            "{} must be an integer or integer string",
            keys.join(".")
        ))),
    }
}

fn get_value<'a>(value: &'a toml::Value, keys: &[&str]) -> Result<&'a toml::Value> {
    let mut current = value;
    for key in keys {
        current = current
            .get(*key)
            .ok_or_else(|| invalid_data(format!("missing config key {}", keys.join("."))))?;
    }
    Ok(current)
}

fn parse_usize(value: &str) -> std::result::Result<usize, std::num::ParseIntError> {
    let value = value.replace('_', "");
    if let Some(hex) = value.strip_prefix("0x") {
        usize::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
}

fn invalid_data(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidData, error.to_string())
}

fn gen_linker_script(arch: &str, config: &LinkerConfig, has_axvisor_linker: bool) -> Result<()> {
    let legacy_fname = format!("linker_{}.lds", config.platform);
    let use_axvisor_loongarch_linker = has_axvisor_linker && arch == "loongarch64";
    let output_arch = if arch == "x86_64" {
        "i386:x86-64"
    } else if arch.contains("riscv") {
        "riscv" // OUTPUT_ARCH of both riscv32/riscv64 is "riscv"
    } else {
        arch
    };
    let extra_linker_constants = if use_axvisor_loongarch_linker {
        format!(
            "PHYS_VIRT_OFFSET = {:#x};\nPHYS_BASE_ADDRESS = {:#x};",
            config.phys_virt_offset, config.kernel_base_paddr
        )
    } else {
        String::new()
    };
    let entry_directive = if use_axvisor_loongarch_linker {
        format!(
            "EXTERN(_start)\n/*\n * QEMU LoongArch direct kernel boot keeps ELF e_entry as-is, so \
             it must point\n * at the low physical entry even though the linked VMA lives in the \
             higher\n * half.\n */\nENTRY({:#x})",
            config.kernel_base_paddr + 0x40
        )
    } else {
        "ENTRY(_start)".to_string()
    };
    let ld_content = std::fs::read_to_string(LINKER_TEMPLATE_NAME)?
        .replace("%ARCH%", output_arch)
        .replace("%KERNEL_BASE%", &format!("{:#x}", config.kernel_base_vaddr))
        .replace("%CPU_NUM%", &format!("{}", config.max_cpu_num))
        .replace("%EXTRA_LINKER_CONSTANTS%", &extra_linker_constants)
        .replace("%ENTRY_DIRECTIVE%", &entry_directive)
        .replace(
            "%TEXT_AT%",
            if use_axvisor_loongarch_linker {
                ": AT(ADDR(.text) - PHYS_VIRT_OFFSET)"
            } else {
                ":"
            },
        )
        .replace(
            "%TEXT_PROLOGUE%",
            if use_axvisor_loongarch_linker {
                "/*\n         \
                 * The axplat LoongArch boot stub starts with a 64-byte Linux/EFI\n         \
                 * boot header. Keep `.text.boot`, but make the ELF entry point land on\n         \
                 * the first real instruction after the header.\n         \
                 */\n        KEEP(*(.text.boot))\n        _start_actual = _start + 0x40;"
            } else {
                "*(.text.boot)"
            },
        )
        .replace(
            "%RODATA_AT%",
            if use_axvisor_loongarch_linker {
                ": AT(ADDR(.rodata) - PHYS_VIRT_OFFSET)"
            } else {
                ":"
            },
        )
        .replace(
            "%RODATA_EXTRA_BODY%",
            if use_axvisor_loongarch_linker {
                ""
            } else {
                "*(.sdata2 .sdata2.*)"
            },
        )
        .replace(
            "%INIT_ARRAY_SECTION%",
            if use_axvisor_loongarch_linker {
                ""
            } else {
                r#"
    .init_array : ALIGN(0x10) {
        __init_array_start = .;
        *(.init_array .init_array.*)
        __init_array_end = .;
    }
"#
            },
        )
        .replace(
            "%DWARF%",
            if use_axvisor_loongarch_linker {
                ""
            } else if std::env::var("DWARF").is_ok_and(|v| v == "y") {
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
        .replace(
            "%DATA_AT%",
            if use_axvisor_loongarch_linker {
                ": AT(ADDR(.data) - PHYS_VIRT_OFFSET)"
            } else {
                ":"
            },
        )
        .replace(
            "%DATA_PROLOGUE%",
            if use_axvisor_loongarch_linker {
                ""
            } else {
                "*(.data.boot_page_table)\n        . = ALIGN(4K);"
            },
        )
        .replace(
            "%DATA_EXTRA_BODY%",
            if use_axvisor_loongarch_linker {
                "__sdriver_register = .;\n        KEEP(*(.driver.register*))\n        \
                 __edriver_register = .;"
            } else {
                r#"*(.got .got.*)

        . = ALIGN(0x10);
        _sdriver = .;
        KEEP(*(.driver.register*))
        _edriver = .;

        . = ALIGN(0x10);
        _ex_table_start = .;
        KEEP(*(__ex_table))
        _ex_table_end = .;"#
            },
        )
        .replace(
            "%POST_DATA_SECTION%",
            if use_axvisor_loongarch_linker {
                r#"
    /*
     * Trap handler slices emitted by `linkme` must live in the regular
     * higher-half image. If left orphaned, ld can place them at VMA 0, which
     * makes the trap dispatcher load a NULL handler pointer from the low
     * linear map and jump to 0x0 once IRQs are enabled.
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
        )
        .replace(
            "%TDATA_AT%",
            if use_axvisor_loongarch_linker {
                ": AT(ADDR(.tdata) - PHYS_VIRT_OFFSET)"
            } else {
                ":"
            },
        )
        .replace(
            "%TBSS_AT%",
            if use_axvisor_loongarch_linker {
                ": AT(ADDR(.tbss) - PHYS_VIRT_OFFSET)"
            } else {
                ":"
            },
        )
        .replace(
            "%PERCPU_SECTION%",
            if use_axvisor_loongarch_linker {
                r#"    . = ALIGN(64);
    /*
     * QEMU LoongArch direct boot loads ELF PT_LOAD segments by translating the
     * segment VMA through `cpu_loongarch_virt_to_phys()`, not by using
     * `p_paddr`. Therefore `.percpu` must keep a higher-half VMA here;
     * otherwise a VMA of 0 would be copied to physical 0x0 instead of the
     * runtime percpu area at `_percpu_start`.
     */
    .percpu : AT(ADDR(.percpu) - PHYS_VIRT_OFFSET) {
        _percpu_start = .;
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        _percpu_load_end_aligned = ALIGN(64);
        . = _percpu_start
          + (_percpu_load_end_aligned - _percpu_load_start) * CPU_NUM;
    }
    _percpu_end = .;
"#
            } else {
                r#"    . = ALIGN(4K);
    _percpu_start = .;
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    .percpu 0x0 : AT(_percpu_start) {
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        . = _percpu_load_start + ALIGN(64) * CPU_NUM;
    }
    . = _percpu_end;
"#
            },
        )
        .replace(
            "%BSS_PREFIX%",
            if use_axvisor_loongarch_linker {
                "_sbss = .;"
            } else {
                ""
            },
        )
        .replace(
            "%BSS_AT%",
            if use_axvisor_loongarch_linker {
                ": AT(ADDR(.bss) - PHYS_VIRT_OFFSET)"
            } else {
                ": AT(.)"
            },
        )
        .replace("%BSS_INNER_PREFIX%", "_sbss = .;")
        .replace(
            "%DISCARD_BODY%",
            if use_axvisor_loongarch_linker {
                "*(.eh_frame)\n        *(.comment)\n        *(.note)\n        *(.note.gnu.build-id)"
            } else {
                "*(.comment) *(.gnu*) *(.note*) *(.eh_frame*)"
            },
        )
        .replace(
            "%TRAILING_SECTIONS%",
            if use_axvisor_loongarch_linker {
                ""
            } else {
                r#"
SECTIONS {
    scope_local : { KEEP(*(scope_local)) }
}
INSERT AFTER .tbss;
"#
            },
        );

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let linker_path = out_dir.join(LINKER_SCRIPT_NAME);

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out/linker.x
    std::fs::write(&linker_path, &ld_content)?;

    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rustc-link-arg=-T{}", linker_path.display());

    if !use_axvisor_loongarch_linker {
        // Keep a stable copy under target/<target_triple>/<mode>/ for callers
        // that still link outside Cargo build-script search paths.
        let target_dir = out_dir.join("../../..");
        std::fs::write(target_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;
        std::fs::write(target_dir.join(legacy_fname), ld_content)?;
    }

    Ok(())
}
