use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::{Path, PathBuf},
};

const LINKER_SCRIPT_NAME: &str = "axplat.x";
const LINKER_TEMPLATE_NAME: &str = "axplat.lds.S";
const LOONGARCH64_LINKER_TEMPLATE_NAME: &str =
    "../../../../platforms/ax-plat-loongarch64-qemu-virt/linker.lds.S";
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
        feature: "aarch64-qemu-virt",
        target_arch: Some("aarch64"),
        crate_name: "ax_plat_aarch64_qemu_virt",
    },
    PlatformFeature {
        feature: "aarch64-raspi",
        target_arch: Some("aarch64"),
        crate_name: "ax_plat_aarch64_raspi",
    },
    PlatformFeature {
        feature: "aarch64-bsta1000b",
        target_arch: Some("aarch64"),
        crate_name: "ax_plat_aarch64_bsta1000b",
    },
    PlatformFeature {
        feature: "aarch64-phytium-pi",
        target_arch: Some("aarch64"),
        crate_name: "ax_plat_aarch64_phytium_pi",
    },
    PlatformFeature {
        feature: "riscv64-qemu-virt",
        target_arch: Some("riscv64"),
        crate_name: "ax_plat_riscv64_qemu_virt",
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
    ("aarch64", "ax_plat_aarch64_qemu_virt"),
    ("loongarch64", "ax_plat_loongarch64_qemu_virt"),
    ("riscv64", "ax_plat_riscv64_qemu_virt"),
    ("x86_64", "ax_plat_x86_pc"),
];

fn main() {
    println!("cargo:rustc-check-cfg=cfg(plat_dyn)");
    println!("cargo:rustc-check-cfg=cfg(ax_hal_any_platform_feature)");
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-changed={LOONGARCH64_LINKER_TEMPLATE_NAME}");
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

    let platform_linker_is_external = selected_platform
        .is_some_and(|platform| matches!(platform.feature, "plat-dyn" | "x86-qemu-q35"));

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
    phys_virt_offset: Option<usize>,
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
            phys_virt_offset: None,
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
        phys_virt_offset: get_optional_usize(&value, &["plat", "phys-virt-offset"])?,
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

fn get_optional_usize(value: &toml::Value, keys: &[&str]) -> Result<Option<usize>> {
    match get_value(value, keys) {
        Ok(value) => parse_value_usize(value, keys).map(Some),
        Err(err) if err.to_string().starts_with("missing config key ") => Ok(None),
        Err(err) => Err(err),
    }
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
    let template_name = if config.platform == "loongarch64-qemu-virt" {
        LOONGARCH64_LINKER_TEMPLATE_NAME
    } else {
        LINKER_TEMPLATE_NAME
    };
    let mut ld_content = std::fs::read_to_string(template_name)?
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
    if config.platform == "loongarch64-qemu-virt" {
        let phys_virt_offset = config
            .phys_virt_offset
            .ok_or_else(|| invalid_data("missing config key plat.phys-virt-offset"))?;
        let entry_paddr = config.kernel_base_paddr + 0x40;
        ld_content = ld_content
            .replace("%PHYS_VIRT_OFFSET%", &format!("{phys_virt_offset:#x}"))
            .replace("%ENTRY_PADDR%", &format!("{entry_paddr:#x}"));
    }

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let linker_path = out_dir.join(LINKER_SCRIPT_NAME);

    // target/<target_triple>/<mode>/build/ax-hal-xxxx/out/axplat.x
    fs::write(&linker_path, &ld_content)?;

    println!("cargo:rustc-link-search={}", out_dir.display());

    // Keep a stable copy under target/<target_triple>/<mode>/ for callers
    // that still link outside Cargo build-script search paths.
    let target_dir = out_dir.join("../../..");
    fs::write(target_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;

    Ok(())
}
