use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::PathBuf,
};

use quote::quote;

const LINKER_TEMPLATE_NAME: &str = "runtime.ld";
const FINAL_LINKER_SCRIPT_NAME: &str = "linker.x";
const EXT_LINKER_SCRIPT_NAME: &str = "runtime.x";
const BUILD_INFO_NAME: &str = "build_info.rs";
const AXTEST_COVERAGE_RUNTIME_SECTIONS_PLACEHOLDER: &str = "%AXTEST_COVERAGE_RUNTIME_SECTIONS%";
const AXTEST_COVERAGE_OUTPUT_SECTIONS_PLACEHOLDER: &str = "%AXTEST_COVERAGE_OUTPUT_SECTIONS%";
const DEFAULT_CPU_CAPACITY: usize = 16;
const DEFAULT_TASK_STACK_SIZE: usize = 0x40000;
const DEFAULT_TICKS_PER_SEC: usize = 100;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EXT_LD");
    println!("cargo:rerun-if-env-changed=SMP");
    println!("cargo:rerun-if-env-changed=DWARF");
    println!("cargo:rerun-if-env-changed=AXTEST_COVERAGE");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ld_content = fs::read_to_string(LINKER_TEMPLATE_NAME)?
        .replace("%DWARF%", dwarf_sections())
        .replace(
            AXTEST_COVERAGE_RUNTIME_SECTIONS_PLACEHOLDER,
            axtest_coverage_runtime_sections(),
        )
        .replace(
            AXTEST_COVERAGE_OUTPUT_SECTIONS_PLACEHOLDER,
            axtest_coverage_output_sections(),
        );
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

fn axtest_coverage_enabled() -> bool {
    env::var("AXTEST_COVERAGE").ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "1" | "true" | "on"
        )
    })
}

fn axtest_coverage_runtime_sections() -> &'static str {
    if axtest_coverage_enabled() {
        r#"
        . = ALIGN(0x10);
        __start___llvm_prf_data = .;
        KEEP(*(__llvm_prf_data))
        __stop___llvm_prf_data = .;

        . = ALIGN(0x10);
        __start___llvm_prf_cnts = .;
        KEEP(*(__llvm_prf_cnts))
        __stop___llvm_prf_cnts = .;

        . = ALIGN(0x10);
        __start___llvm_prf_bits = .;
        KEEP(*(__llvm_prf_bits))
        __stop___llvm_prf_bits = .;

        . = ALIGN(0x10);
        __start___llvm_prf_vnds = .;
        KEEP(*(__llvm_prf_vnds))
        __stop___llvm_prf_vnds = .;"#
    } else {
        ""
    }
}

fn axtest_coverage_output_sections() -> &'static str {
    if axtest_coverage_enabled() {
        r#"    __llvm_prf_names : AT(ADDR(__llvm_prf_names) - AX_LINKER_LOAD_OFFSET) ALIGN(0x10) {
        __start___llvm_prf_names = .;
        KEEP(*(__llvm_prf_names))
        __stop___llvm_prf_names = .;
    }
"#
    } else {
        ""
    }
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

        #[cfg(feature = "fs")]
        pub const TASK_STACK_SIZE: usize = #task_stack_size;

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
        let mut config = Self {
            cpu_capacity: DEFAULT_CPU_CAPACITY,
            task_stack_size: DEFAULT_TASK_STACK_SIZE,
            ticks_per_sec: DEFAULT_TICKS_PER_SEC,
        };

        if let Ok(smp) = env::var("SMP") {
            config.cpu_capacity = parse_usize(&smp)
                .map_err(|err| invalid_data(format!("failed to parse SMP value `{smp}`: {err}")))?;
        }

        Ok(config)
    }
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
                "#[cfg(feature = \"fs\")]\n",
                "pub const TASK_STACK_SIZE: usize = 262144usize;\n",
                "pub const TICKS_PER_SEC: usize = 100usize;\n",
            ))
        );
    }
}
