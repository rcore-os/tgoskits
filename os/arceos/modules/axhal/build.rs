use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::{Path, PathBuf},
};

use quote::{format_ident, quote};

const LINKER_SCRIPT_NAME: &str = "axplat.x";
const LINKER_TEMPLATE_NAME: &str = "axplat.lds.S";
const SELECTED_PLATFORM_NAME: &str = "selected_platform.rs";
const BUILD_INFO_NAME: &str = "build_info.rs";
const DEFAULT_CPU_CAPACITY: usize = 16;

struct PlatformFeature {
    feature: &'static str,
    target_arch: Option<&'static str>,
    crate_name: &'static str,
}

const PLATFORM_FEATURES: &[PlatformFeature] = &[
    PlatformFeature {
        feature: "plat-dyn",
        target_arch: None,
        crate_name: "axplat_dyn",
    },
    PlatformFeature {
        feature: "riscv64-sg2002",
        target_arch: Some("riscv64"),
        crate_name: "ax_plat_riscv64_sg2002",
    },
];

fn main() {
    println!("cargo:rustc-check-cfg=cfg(plat_dyn)");
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=SMP");
    println!("cargo:rerun-if-env-changed={}", feature_env("host-test"));
    println!("cargo:rerun-if-env-changed={}", feature_env("myplat"));
    println!("cargo:rerun-if-env-changed={}", feature_env("defplat"));
    for platform in PLATFORM_FEATURES {
        println!(
            "cargo:rerun-if-env-changed={}",
            feature_env(platform.feature)
        );
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let selected_platform = check_platform_features(&arch);
    gen_selected_platform(&arch, selected_platform).unwrap();

    let platform_linker_is_external =
        selected_platform.is_some_and(|platform| platform.feature == "plat-dyn");

    let config = load_linker_config(platform_linker_is_external).unwrap();
    gen_build_info(config.max_cpu_num).unwrap();

    if !config.is_dummy && !platform_linker_is_external {
        gen_linker_script(&arch, &config).unwrap();
    }
}

fn check_platform_features(arch: &str) -> Option<&'static PlatformFeature> {
    let has_myplat = feature_enabled("myplat");
    let enabled_platforms = PLATFORM_FEATURES
        .iter()
        .filter(|platform| feature_enabled(platform.feature))
        .collect::<Vec<_>>();

    if has_myplat && !enabled_platforms.is_empty() {
        panic!("ax-hal/myplat must not be combined with a built-in ax-hal platform feature");
    }

    if feature_enabled("plat-dyn") && enabled_platforms.len() > 1 {
        panic!("ax-hal/plat-dyn must not be combined with a built-in ax-hal platform feature");
    }

    for platform in &enabled_platforms {
        if let Some(target_arch) = platform.target_arch {
            let conflicting_features = enabled_platforms
                .iter()
                .filter(|other| other.target_arch == Some(target_arch))
                .map(|platform| platform.feature)
                .collect::<Vec<_>>();
            if conflicting_features.len() > 1 {
                panic!(
                    "multiple ax-hal platform features are enabled for target_arch = \"{}\": {}",
                    target_arch,
                    conflicting_features.join(", ")
                );
            }
        }
    }

    for platform in &enabled_platforms {
        if let Some(target_arch) = platform.target_arch
            && arch != target_arch
        {
            panic!(
                "ax-hal/{} requires target_arch = \"{}\"",
                platform.feature, target_arch
            );
        }
    }

    enabled_platforms.into_iter().find(|platform| {
        platform
            .target_arch
            .is_none_or(|target_arch| target_arch == arch)
    })
}

fn gen_selected_platform(arch: &str, platform: Option<&PlatformFeature>) -> Result<()> {
    let crate_name = if let Some(platform) = platform {
        if platform.feature == "plat-dyn" {
            Some(platform.crate_name)
        } else {
            platform
                .target_arch
                .is_some_and(|target_arch| target_arch == arch)
                .then_some(platform.crate_name)
        }
    } else {
        None
    };

    if crate_name == Some("axplat_dyn") {
        println!("cargo:rustc-cfg=plat_dyn");
    }

    let content = selected_platform_source(crate_name);
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join(SELECTED_PLATFORM_NAME), content)
}

fn selected_platform_source(crate_name: Option<&str>) -> String {
    crate_name
        .map(|crate_name| {
            let crate_ident = format_ident!("{crate_name}");
            quote! {
                extern crate #crate_ident as _;
            }
            .to_string()
        })
        .unwrap_or_default()
}

fn feature_enabled(feature: &str) -> bool {
    std::env::var_os(feature_env(feature)).is_some()
}

fn feature_env(feature: &str) -> String {
    format!(
        "CARGO_FEATURE_{}",
        feature.replace('-', "_").to_ascii_uppercase()
    )
}

#[derive(Debug)]
struct LinkerConfig {
    is_dummy: bool,
    kernel_base_vaddr: usize,
    max_cpu_num: usize,
    kernel_base_paddr: usize,
}

fn load_linker_config(platform_linker_is_external: bool) -> Result<LinkerConfig> {
    match env::var("AX_CONFIG_PATH") {
        Ok(path) => {
            println!("cargo:rerun-if-changed={path}");
            read_linker_config(Path::new(&path))
        }
        Err(_) if platform_linker_is_external => Ok(external_linker_config()?),
        Err(_) => Ok(dummy_linker_config()),
    }
}

fn read_linker_config(path: &Path) -> Result<LinkerConfig> {
    let content = fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&content).map_err(invalid_data)?;
    let platform = get_string(&value, &["platform"])?;
    Ok(LinkerConfig {
        is_dummy: platform == "dummy",
        kernel_base_vaddr: get_usize(&value, &["plat", "kernel-base-vaddr"])?,
        max_cpu_num: get_usize(&value, &["plat", "max-cpu-num"])?,
        kernel_base_paddr: get_usize(&value, &["plat", "kernel-base-paddr"])?,
    })
}

fn external_linker_config() -> Result<LinkerConfig> {
    Ok(LinkerConfig {
        is_dummy: false,
        kernel_base_vaddr: 0,
        max_cpu_num: read_cpu_capacity_env()?,
        kernel_base_paddr: 0,
    })
}

fn dummy_linker_config() -> LinkerConfig {
    LinkerConfig {
        is_dummy: true,
        kernel_base_vaddr: 0,
        max_cpu_num: 1,
        kernel_base_paddr: 0,
    }
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

fn read_cpu_capacity_env() -> Result<usize> {
    match env::var("SMP") {
        Ok(value) => parse_usize(&value)
            .map_err(|err| invalid_data(format!("failed to parse SMP value `{value}`: {err}"))),
        Err(_) => Ok(DEFAULT_CPU_CAPACITY),
    }
}

fn gen_build_info(cpu_capacity: usize) -> Result<()> {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    fs::write(
        out_dir.join(BUILD_INFO_NAME),
        build_info_source(cpu_capacity),
    )
}

fn build_info_source(cpu_capacity: usize) -> String {
    quote! {
        #[cfg(feature = "smp")]
        pub const CPU_CAPACITY: usize = #cpu_capacity;
    }
    .to_string()
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
            "%KERNEL_BASE_VADDR%",
            &format!("{:#x}", config.kernel_base_vaddr),
        )
        .replace(
            "%KERNEL_BASE_PADDR%",
            &format!("{:#x}", config.kernel_base_paddr),
        )
        .replace("%CPU_NUM%", &format!("{}", config.max_cpu_num));

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let linker_path = out_dir.join(LINKER_SCRIPT_NAME);

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out/axplat.x
    fs::write(&linker_path, &ld_content)?;

    println!("cargo:rustc-link-search={}", out_dir.display());

    Ok(())
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
    fn selected_platform_source_handles_crate_names_with_underscores() {
        assert_eq!(
            semantic_source(&selected_platform_source(Some(
                "ax_plat_loongarch64_qemu_virt"
            ))),
            semantic_source("extern crate ax_plat_loongarch64_qemu_virt as _;")
        );
    }

    #[test]
    fn selected_platform_source_is_empty_without_platform() {
        assert!(selected_platform_source(None).is_empty());
    }

    #[test]
    fn build_info_source_generates_smp_cpu_capacity() {
        assert_eq!(
            semantic_source(&build_info_source(16)),
            semantic_source("#[cfg(feature = \"smp\")] pub const CPU_CAPACITY: usize = 16usize;")
        );
    }
}
