use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::PathBuf,
};

use quote::quote;

const BUILD_INFO_NAME: &str = "build_info.rs";
const DEFAULT_CPU_CAPACITY: usize = 16;

fn main() -> Result<()> {
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
    match env::var("SMP") {
        Ok(smp) => parse_usize(&smp)
            .map_err(|err| invalid_data(format!("failed to parse SMP value `{smp}`: {err}"))),
        Err(_) => Ok(DEFAULT_CPU_CAPACITY),
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
