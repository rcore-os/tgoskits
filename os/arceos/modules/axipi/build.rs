use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::{Path, PathBuf},
};

use quote::quote;

const BUILD_INFO_NAME: &str = "build_info.rs";
const DEFAULT_CPU_CAPACITY: usize = 16;

fn main() -> Result<()> {
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=SMP");

    let cpu_capacity = read_cpu_capacity()?;
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(
        out_dir.join(BUILD_INFO_NAME),
        build_info_source(cpu_capacity),
    )
}

fn build_info_source(cpu_capacity: usize) -> String {
    quote! {
        pub const CPU_CAPACITY: usize = #cpu_capacity;
    }
    .to_string()
}

fn read_cpu_capacity() -> Result<usize> {
    let mut cpu_capacity = match env::var("AX_CONFIG_PATH") {
        Ok(path) => {
            println!("cargo:rerun-if-changed={path}");
            read_ax_config_cpu_capacity(Path::new(&path))?
        }
        Err(_) => DEFAULT_CPU_CAPACITY,
    };

    if let Ok(smp) = env::var("SMP") {
        cpu_capacity = parse_usize(&smp)
            .map_err(|err| invalid_data(format!("failed to parse SMP value `{smp}`: {err}")))?;
    }

    Ok(cpu_capacity)
}

fn read_ax_config_cpu_capacity(path: &Path) -> Result<usize> {
    let content = fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&content).map_err(invalid_data)?;
    get_usize(&value, &["plat", "max-cpu-num"])
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

#[cfg(test)]
mod tests {
    use super::*;

    fn semantic_source(source: &str) -> String {
        source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    #[test]
    fn build_info_source_generates_cpu_capacity() {
        assert_eq!(
            semantic_source(&build_info_source(DEFAULT_CPU_CAPACITY)),
            semantic_source("pub const CPU_CAPACITY: usize = 16usize;")
        );
    }
}
