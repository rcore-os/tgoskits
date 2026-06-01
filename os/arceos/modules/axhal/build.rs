use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::{Path, PathBuf},
};

const LINKER_SCRIPT_NAME: &str = "axplat.x";
const LINKER_TEMPLATE_NAME: &str = "axplat.lds.S";
const SELECTED_PLATFORM_NAME: &str = "selected_platform.rs";

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
        feature: "x86-pc",
        target_arch: Some("x86_64"),
        crate_name: "ax_plat_x86_pc",
    },
    PlatformFeature {
        feature: "x86-qemu-q35",
        target_arch: Some("x86_64"),
        crate_name: "ax_plat_x86_qemu_q35",
    },
    PlatformFeature {
        feature: "x86-asus-nuc15crh",
        target_arch: Some("x86_64"),
        crate_name: "ax_plat_x86_asus_nuc15crh",
    },
    PlatformFeature {
        feature: "riscv64-sg2002",
        target_arch: Some("riscv64"),
        crate_name: "ax_plat_riscv64_sg2002",
    },
    PlatformFeature {
        feature: "riscv64-visionfive2",
        target_arch: Some("riscv64"),
        crate_name: "ax_plat_riscv64_visionfive2",
    },
    PlatformFeature {
        feature: "loongarch64-qemu-virt",
        target_arch: Some("loongarch64"),
        crate_name: "ax_plat_loongarch64_qemu_virt",
    },
];

const DEFAULT_PLATFORMS: &[(&str, &str)] = &[
    ("loongarch64", "ax_plat_loongarch64_qemu_virt"),
    ("x86_64", "ax_plat_x86_pc"),
];

fn main() {
    println!("cargo:rustc-check-cfg=cfg(plat_dyn)");
    println!("cargo:rustc-check-cfg=cfg(ax_hal_any_platform_feature)");
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed={}", feature_env("myplat"));
    println!("cargo:rerun-if-env-changed={}", feature_env("defplat"));
    for platform in PLATFORM_FEATURES {
        println!(
            "cargo:rerun-if-env-changed={}",
            feature_env(platform.feature)
        );
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let selected_platform = check_platform_features(&arch, &target_os);
    gen_selected_platform(&arch, &target_os, selected_platform).unwrap();

    let config = load_linker_config().unwrap();

    let platform_linker_is_external = selected_platform.is_some_and(|platform| {
        matches!(
            platform.feature,
            "plat-dyn" | "x86-asus-nuc15crh" | "x86-qemu-q35" | "loongarch64-qemu-virt"
        )
    });

    if config.platform != "dummy" && !platform_linker_is_external {
        gen_linker_script(&arch, &config).unwrap();
    }
}

fn check_platform_features(arch: &str, target_os: &str) -> Option<&'static PlatformFeature> {
    let has_myplat = feature_enabled("myplat");
    let enabled_platforms = PLATFORM_FEATURES
        .iter()
        .filter(|platform| feature_enabled(platform.feature))
        .collect::<Vec<_>>();

    if has_myplat || !enabled_platforms.is_empty() {
        println!("cargo:rustc-cfg=ax_hal_any_platform_feature");
    }

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

    if target_os == "none" {
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
    }

    enabled_platforms.into_iter().find(|platform| {
        platform
            .target_arch
            .is_none_or(|target_arch| target_arch == arch)
    })
}

fn gen_selected_platform(
    arch: &str,
    target_os: &str,
    platform: Option<&PlatformFeature>,
) -> Result<()> {
    let crate_name = if let Some(platform) = platform {
        if platform.feature == "plat-dyn" {
            (target_os == "none").then_some(platform.crate_name)
        } else {
            platform
                .target_arch
                .is_some_and(|target_arch| target_arch == arch)
                .then_some(platform.crate_name)
        }
    } else if target_os == "none" && feature_enabled("defplat") && !feature_enabled("myplat") {
        DEFAULT_PLATFORMS
            .iter()
            .find_map(|(target_arch, crate_name)| (*target_arch == arch).then_some(*crate_name))
    } else {
        None
    };

    if crate_name == Some("axplat_dyn") {
        println!("cargo:rustc-cfg=plat_dyn");
    }

    let content = crate_name
        .map(|crate_name| format!("extern crate {crate_name} as _;\n"))
        .unwrap_or_default();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join(SELECTED_PLATFORM_NAME), content)
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
