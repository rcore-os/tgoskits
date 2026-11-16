use std::{env, fs, io::Result, path::PathBuf};

use quote::{format_ident, quote};

const SELECTED_PLATFORM_NAME: &str = "selected_platform.rs";
const BUILD_INFO_NAME: &str = "build_info.rs";
const PLATFORM_CRATE_ENV: &str = "AX_PLATFORM_CRATE";
const DEFAULT_PLATFORM_CRATE: &str = "axplat_dyn";

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let kernel_tls = std::env::var_os("CARGO_FEATURE_TLS").is_some()
        && std::env::var_os("CARGO_FEATURE_USPACE").is_none();
    println!("cargo::rustc-check-cfg=cfg(kernel_tls)");
    if kernel_tls {
        println!("cargo::rustc-cfg=kernel_tls");
    }

    println!("cargo:rerun-if-env-changed={PLATFORM_CRATE_ENV}");

    let config = ax_config::load_build_env()?;
    gen_selected_platform()?;
    gen_build_info(config.cpu_capacity)?;
    Ok(())
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
