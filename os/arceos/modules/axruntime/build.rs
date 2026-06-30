use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::{Path, PathBuf},
};

use quote::quote;

const LINKER_TEMPLATE_NAME: &str = "runtime.ld";
const FINAL_LINKER_SCRIPT_NAME: &str = "linker.x";
const EXT_LINKER_SCRIPT_NAME: &str = "runtime.x";
const BUILD_INFO_NAME: &str = "build_info.rs";
const DEFAULT_CPU_CAPACITY: usize = 16;
const DEFAULT_TASK_STACK_SIZE: usize = 0x40000;
const DEFAULT_TICKS_PER_SEC: usize = 100;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EXT_LD");
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=SMP");
    println!("cargo:rerun-if-env-changed=DWARF");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ld_content = fs::read_to_string(LINKER_TEMPLATE_NAME)?.replace("%DWARF%", dwarf_sections());
    let linker_script_name = if env::var_os("CARGO_FEATURE_EXT_LD").is_some() {
        EXT_LINKER_SCRIPT_NAME
    } else {
        FINAL_LINKER_SCRIPT_NAME
    };
    let linker_path = out_dir.join(linker_script_name);

    fs::write(&linker_path, &ld_content)?;
    fs::write(out_dir.join(BUILD_INFO_NAME), build_info_source()?)?;
    println!("cargo:rustc-link-search={}", out_dir.display());

    Ok(())
}

fn build_info_source() -> Result<String> {
    let arch = env::var("CARGO_CFG_TARGET_ARCH")
        .map_err(|err| std::io::Error::other(format!("CARGO_CFG_TARGET_ARCH is not set: {err}")))?;
    let target = env::var("AX_TARGET").unwrap_or_default();
    let mode = env::var("AX_MODE").unwrap_or_default();
    let config = RuntimeConfig::load()?;
    Ok(build_info_source_from(&arch, &target, &mode, config))
}

fn build_info_source_from(arch: &str, target: &str, mode: &str, config: RuntimeConfig) -> String {
    let cpu_capacity = config.cpu_capacity;
    let task_stack_size = config.task_stack_size;
    let ticks_per_sec = config.ticks_per_sec;

    quote! {
        pub const ARCH: &str = #arch;
        pub const TARGET: &str = #target;
        pub const MODE: &str = #mode;

        #[cfg(feature = "smp")]
        pub const CPU_CAPACITY: usize = #cpu_capacity;

        #[cfg(any(feature = "fs", all(feature = "smp", not(feature = "plat-dyn"))))]
        pub const TASK_STACK_SIZE: usize = #task_stack_size;

        #[cfg(feature = "irq")]
        pub const TICKS_PER_SEC: usize = #ticks_per_sec;
    }
    .to_string()
}

#[derive(Clone, Copy)]
struct RuntimeConfig {
    cpu_capacity: usize,
    task_stack_size: usize,
    ticks_per_sec: usize,
}

impl RuntimeConfig {
    fn load() -> Result<Self> {
        let mut config = match env::var("AX_CONFIG_PATH") {
            Ok(path) => {
                println!("cargo:rerun-if-changed={path}");
                Self::from_ax_config(Path::new(&path))?
            }
            Err(_) => Self {
                cpu_capacity: DEFAULT_CPU_CAPACITY,
                task_stack_size: DEFAULT_TASK_STACK_SIZE,
                ticks_per_sec: DEFAULT_TICKS_PER_SEC,
            },
        };

        if let Ok(smp) = env::var("SMP") {
            config.cpu_capacity = parse_usize(&smp)
                .map_err(|err| invalid_data(format!("failed to parse SMP value `{smp}`: {err}")))?;
        }

        Ok(config)
    }

    fn from_ax_config(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let value: toml::Value = toml::from_str(&content).map_err(invalid_data)?;
        Ok(Self {
            cpu_capacity: get_usize(&value, &["plat", "max-cpu-num"])?,
            task_stack_size: get_usize(&value, &["task-stack-size"])?,
            ticks_per_sec: get_usize(&value, &["ticks-per-sec"])?,
        })
    }
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

fn dwarf_sections() -> &'static str {
    if env_truthy("DWARF") {
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
    }
}

fn env_truthy(key: &str) -> bool {
    env::var(key).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "1" | "true" | "on"
        )
    })
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
    fn build_info_source_generates_banner_constants() {
        assert_eq!(
            semantic_source(&build_info_source_from(
                "riscv64",
                "riscv64gc-unknown-none-elf",
                "release",
                RuntimeConfig {
                    cpu_capacity: DEFAULT_CPU_CAPACITY,
                    task_stack_size: DEFAULT_TASK_STACK_SIZE,
                    ticks_per_sec: DEFAULT_TICKS_PER_SEC,
                },
            )),
            semantic_source(concat!(
                "pub const ARCH: &str = \"riscv64\";\n",
                "pub const TARGET: &str = \"riscv64gc-unknown-none-elf\";\n",
                "pub const MODE: &str = \"release\";\n",
                "#[cfg(feature = \"smp\")]\n",
                "pub const CPU_CAPACITY: usize = 16usize;\n",
                "#[cfg(any(feature = \"fs\", all(feature = \"smp\", not(feature = \
                 \"plat-dyn\"))))]\n",
                "pub const TASK_STACK_SIZE: usize = 262144usize;\n",
                "#[cfg(feature = \"irq\")]\n",
                "pub const TICKS_PER_SEC: usize = 100usize;\n",
            ))
        );
    }
}
