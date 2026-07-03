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

    let config = TaskConfig::load()?;
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join(BUILD_INFO_NAME), build_info_source(config))
}

fn build_info_source(config: TaskConfig) -> String {
    let cpu_capacity = config.cpu_capacity;
    let task_stack_size = config.task_stack_size;

    quote! {
        pub const CPU_CAPACITY: usize = #cpu_capacity;
        pub const DEFAULT_TASK_STACK_SIZE: usize = #task_stack_size;
    }
    .to_string()
}

#[derive(Clone, Copy)]
struct TaskConfig {
    cpu_capacity: usize,
    task_stack_size: usize,
}

impl TaskConfig {
    fn load() -> Result<Self> {
        let mut config = Self {
            cpu_capacity: DEFAULT_CPU_CAPACITY,
            task_stack_size: DEFAULT_TASK_STACK_SIZE,
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
            })),
            semantic_source(
                "pub const CPU_CAPACITY: usize = 16usize; pub const DEFAULT_TASK_STACK_SIZE: \
                 usize = 262144usize;"
            )
        );
    }
}
