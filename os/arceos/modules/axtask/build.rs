use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::PathBuf,
};

use quote::quote;

const BUILD_INFO_NAME: &str = "build_info.rs";
const DEFAULT_CPU_CAPACITY: usize = 16;
const DEFAULT_TASK_STACK_SIZE: usize = 0x40000;

fn main() -> Result<()> {
    println!("cargo:rerun-if-env-changed=SMP");
    println!("cargo:rerun-if-env-changed=REALTIME_CPU_ID");

    if cfg!(feature = "host-test") {
        let linker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("host-test.ld");
        println!("cargo:rerun-if-changed={}", linker.display());
        // This crate keeps its scheduler tests in the library target rather
        // than a standalone integration-test target.
        println!("cargo:rustc-link-arg=-T{}", linker.display());
    }

    let config = TaskConfig::load()?;
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join(BUILD_INFO_NAME), build_info_source(config))
}

fn build_info_source(config: TaskConfig) -> String {
    let cpu_capacity = config.cpu_capacity;
    let task_stack_size = config.task_stack_size;
    let realtime_cpu_id = match config.realtime_cpu_id {
        Some(cpu_id) => quote! { Some(#cpu_id) },
        None => quote! { None },
    };

    quote! {
        pub const CPU_CAPACITY: usize = #cpu_capacity;
        pub const DEFAULT_TASK_STACK_SIZE: usize = #task_stack_size;
        pub const REALTIME_CPU_ID: Option<usize> = #realtime_cpu_id;
    }
    .to_string()
}

#[derive(Clone, Copy)]
struct TaskConfig {
    cpu_capacity: usize,
    task_stack_size: usize,
    realtime_cpu_id: Option<usize>,
}

impl TaskConfig {
    fn load() -> Result<Self> {
        let mut config = Self {
            cpu_capacity: DEFAULT_CPU_CAPACITY,
            task_stack_size: DEFAULT_TASK_STACK_SIZE,
            realtime_cpu_id: None,
        };

        if let Ok(smp) = env::var("SMP") {
            config.cpu_capacity = parse_usize(&smp)
                .map_err(|err| invalid_data(format!("failed to parse SMP value `{smp}`: {err}")))?;
        }

        if let Ok(value) = env::var("REALTIME_CPU_ID") {
            config.realtime_cpu_id = parse_realtime_cpu_id(&value, config.cpu_capacity)?;
        }

        Ok(config)
    }
}

fn parse_realtime_cpu_id(value: &str, cpu_capacity: usize) -> Result<Option<usize>> {
    if value.trim() == "-1" {
        return Ok(None);
    }
    if value.trim().starts_with('-') {
        return Err(invalid_data(
            "REALTIME_CPU_ID only accepts -1 as a negative value",
        ));
    }
    let cpu_id = parse_usize(value).map_err(|err| {
        invalid_data(format!(
            "failed to parse REALTIME_CPU_ID value `{value}`: {err}"
        ))
    })?;
    if cpu_id >= cpu_capacity {
        return Err(invalid_data(format!(
            "REALTIME_CPU_ID {cpu_id} exceeds CPU capacity {cpu_capacity}"
        )));
    }
    Ok(Some(cpu_id))
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
    fn build_info_source_generates_task_constants() {
        assert_eq!(
            semantic_source(&build_info_source(TaskConfig {
                cpu_capacity: DEFAULT_CPU_CAPACITY,
                task_stack_size: DEFAULT_TASK_STACK_SIZE,
                realtime_cpu_id: None,
            })),
            semantic_source(
                "pub const CPU_CAPACITY: usize = 16usize; pub const DEFAULT_TASK_STACK_SIZE: \
                 usize = 262144usize;"
            )
        );
    }

    #[test]
    fn realtime_cpu_id_validation() {
        assert_eq!(parse_realtime_cpu_id("-1", 4).unwrap(), None);
        assert_eq!(parse_realtime_cpu_id("3", 4).unwrap(), Some(3));
        assert_eq!(parse_realtime_cpu_id("0x3", 4).unwrap(), Some(3));
        assert!(parse_realtime_cpu_id("-2", 4).is_err());
        assert!(parse_realtime_cpu_id("4", 4).is_err());
    }
}
