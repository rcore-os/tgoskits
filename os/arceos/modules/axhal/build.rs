use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::PathBuf,
};

use quote::{format_ident, quote};

const SELECTED_PLATFORM_NAME: &str = "selected_platform.rs";
const BUILD_INFO_NAME: &str = "build_info.rs";
const PLATFORM_CRATE_ENV: &str = "AX_PLATFORM_CRATE";
const DEFAULT_PLATFORM_CRATE: &str = "axplat_dyn";
const DEFAULT_CPU_CAPACITY: usize = 16;

fn main() {
    println!("cargo:rerun-if-env-changed=SMP");
    println!("cargo:rerun-if-env-changed={PLATFORM_CRATE_ENV}");

    gen_selected_platform().unwrap();
    gen_build_info(read_cpu_capacity_env().unwrap()).unwrap();
}

fn gen_selected_platform() -> Result<()> {
    let crate_name =
        env::var(PLATFORM_CRATE_ENV).unwrap_or_else(|_| DEFAULT_PLATFORM_CRATE.to_string());
    let content = selected_platform_source(&crate_name);
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join(SELECTED_PLATFORM_NAME), content)
}

fn selected_platform_source(crate_name: &str) -> String {
    let crate_ident = format_ident!("{crate_name}");
    quote! {
        pub extern crate #crate_ident as selected;
    }
    .to_string()
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
            semantic_source(&selected_platform_source("ax_plat_loongarch64_qemu_virt")),
            semantic_source("pub extern crate ax_plat_loongarch64_qemu_virt as selected;")
        );
    }

    #[test]
    fn build_info_source_generates_smp_cpu_capacity() {
        assert_eq!(
            semantic_source(&build_info_source(16)),
            semantic_source("#[cfg(feature = \"smp\")] pub const CPU_CAPACITY: usize = 16usize;")
        );
    }
}
