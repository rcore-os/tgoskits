use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::{Path, PathBuf},
};

const LINKER_SCRIPT_NAME: &str = "axplat.x";
const LINKER_TEMPLATE_NAME: &str = "linker.lds.S";
const DEFAULT_CONFIG_NAME: &str = "axconfig.toml";

#[derive(Debug)]
struct LinkerConfig {
    kernel_base_vaddr: usize,
    kernel_base_paddr: usize,
    phys_virt_offset: usize,
    max_cpu_num: usize,
}

fn main() -> Result<()> {
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    let config = load_linker_config()?;
    gen_linker_script(&config)
}

fn load_linker_config() -> Result<LinkerConfig> {
    let path = env::var("AX_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_NAME.to_string());
    println!("cargo:rerun-if-changed={path}");
    read_linker_config(Path::new(&path))
}

fn read_linker_config(path: &Path) -> Result<LinkerConfig> {
    let content = fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&content).map_err(invalid_data)?;
    Ok(LinkerConfig {
        kernel_base_vaddr: get_usize(&value, &["plat", "kernel-base-vaddr"])?,
        kernel_base_paddr: get_usize(&value, &["plat", "kernel-base-paddr"])?,
        phys_virt_offset: get_usize(&value, &["plat", "phys-virt-offset"])?,
        max_cpu_num: get_usize(&value, &["plat", "max-cpu-num"])?,
    })
}

fn get_usize(value: &toml::Value, keys: &[&str]) -> Result<usize> {
    let value = get_value(value, keys)?;
    parse_value_usize(value, keys)
}

fn parse_value_usize(value: &toml::Value, keys: &[&str]) -> Result<usize> {
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

fn gen_linker_script(config: &LinkerConfig) -> Result<()> {
    let entry_paddr = config.kernel_base_paddr + 0x40;
    let ld_content = fs::read_to_string(LINKER_TEMPLATE_NAME)?
        .replace("%ARCH%", "loongarch64")
        .replace(
            "%KERNEL_BASE_VADDR%",
            &format!("{:#x}", config.kernel_base_vaddr),
        )
        .replace(
            "%KERNEL_BASE_PADDR%",
            &format!("{:#x}", config.kernel_base_paddr),
        )
        .replace(
            "%PHYS_VIRT_OFFSET%",
            &format!("{:#x}", config.phys_virt_offset),
        )
        .replace("%ENTRY_PADDR%", &format!("{entry_paddr:#x}"))
        .replace("%CPU_NUM%", &format!("{}", config.max_cpu_num));

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join(LINKER_SCRIPT_NAME), ld_content)?;
    println!("cargo:rustc-link-search={}", out_dir.display());
    Ok(())
}
