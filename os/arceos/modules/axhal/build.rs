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
    let config = load_linker_config().unwrap();

    if has_plat_dyn && target_os == "none" {
        println!("cargo:rustc-cfg=plat_dyn");
    }

    if config.platform != "dummy" {
        gen_linker_script(&arch, &config).unwrap();
    }
}

#[derive(Debug)]
struct LinkerConfig {
    platform: String,
    kernel_base_vaddr: usize,
    max_cpu_num: usize,
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

fn gen_linker_script(arch: &str, config: &LinkerConfig) -> Result<()> {
    let output_arch = if arch == "x86_64" {
        "i386:x86-64"
    } else if arch.contains("riscv") {
        "riscv" // OUTPUT_ARCH of both riscv32/riscv64 is "riscv"
    } else {
        arch
    };
    let ld_content = std::fs::read_to_string(LINKER_TEMPLATE_NAME)?
        .replace("%ARCH%", output_arch)
        .replace("%KERNEL_BASE%", &format!("{:#x}", config.kernel_base_vaddr))
        .replace(
            "%KERNEL_BASE_PADDR%",
            &format!("{:#x}", config.kernel_base_paddr),
        )
        .replace("%CPU_NUM%", &format!("{}", config.max_cpu_num))
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
        );

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let linker_path = out_dir.join(LINKER_SCRIPT_NAME);

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out/linker.x
    fs::write(&linker_path, &ld_content)?;

    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rustc-link-arg=-T{}", linker_path.display());

    // Keep a stable copy under target/<target_triple>/<mode>/ for callers
    // that still link outside Cargo build-script search paths.
    let target_dir = out_dir.join("../../..");
    fs::write(target_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;

    Ok(())
}
