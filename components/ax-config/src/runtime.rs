use std::{alloc::Layout, env, num::ParseIntError};

use crate::{ConfigError, RuntimeOptions};

/// Default fixed CPU capacity, independent of discovered online CPU count.
pub const DEFAULT_CPU_CAPACITY: usize = 16;
/// Default task stack size in bytes.
pub const DEFAULT_TASK_STACK_SIZE: usize = 0x40000;
/// Default scheduler tick interval in milliseconds.
pub const DEFAULT_SCHEDULER_TICK_MS: u64 = 10;

const NANOS_PER_MILLISECOND: u64 = 1_000_000;
const PAGE_SIZE: usize = 4096;

/// Raw environment snapshot for one build request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeEnv {
    pub smp: Option<String>,
    pub task_stack_size: Option<String>,
    pub scheduler_tick_ms: Option<String>,
}

/// Effective runtime values returned by [`resolve_runtime`].
///
/// Callers constructing or modifying this value must preserve the same validated
/// nonzero, stack-layout and finite-clock-domain constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeValues {
    pub cpu_capacity: usize,
    pub task_stack_size: usize,
    pub scheduler_tick_ms: u64,
}

impl RuntimeEnv {
    /// Read each relevant variable once, rejecting non-Unicode values.
    ///
    /// Numeric parsing is deferred so typed overrides can supersede malformed
    /// numeric strings. No process environment is modified.
    pub fn capture() -> Result<Self, ConfigError> {
        Ok(Self {
            smp: capture("SMP")?,
            task_stack_size: capture("AX_TASK_STACK_SIZE")?,
            scheduler_tick_ms: capture("AX_SCHEDULER_TICK_MS")?,
        })
    }
}

impl RuntimeValues {
    /// Convert the tick interval to nanoseconds without reparsing.
    ///
    /// Returns an error for zero, multiplication overflow, or values outside
    /// the signed clock domain, including values constructed without resolution.
    pub fn scheduler_tick_interval_nanos(&self) -> Result<u64, ConfigError> {
        Ok(self.scheduler_tick_ms * NANOS_PER_MILLISECOND)
    }
}

/// Resolve each parameter independently: typed input, environment, then default.
///
/// Only the winning numeric source is parsed. Resolution does not change the
/// caller's CPU Option or imply that SMP is enabled.
pub fn resolve_runtime(
    max_cpu_num: Option<usize>,
    options: RuntimeOptions,
    env: &RuntimeEnv,
) -> Result<RuntimeValues, ConfigError> {
    let cpu_capacity = choose(
        max_cpu_num,
        env.smp.as_deref(),
        DEFAULT_CPU_CAPACITY,
        "SMP",
        parse_usize,
    )?;
    if cpu_capacity == 0 {
        return Err(ConfigError::invalid(
            "max_cpu_num",
            cpu_capacity,
            "a nonzero CPU capacity",
        ));
    }
    let task_stack_size = choose(
        options.task_stack_size,
        env.task_stack_size.as_deref(),
        DEFAULT_TASK_STACK_SIZE,
        "AX_TASK_STACK_SIZE",
        parse_usize,
    )?;
    validate_stack(task_stack_size)?;
    let scheduler_tick_ms = choose(
        options.scheduler_tick_ms,
        env.scheduler_tick_ms.as_deref(),
        DEFAULT_SCHEDULER_TICK_MS,
        "AX_SCHEDULER_TICK_MS",
        parse_u64,
    )?;
    if scheduler_tick_ms == 0
        || scheduler_tick_ms
            .checked_mul(NANOS_PER_MILLISECOND)
            .is_none_or(|nanos| nanos > i64::MAX as u64)
    {
        return Err(ConfigError::invalid(
            "runtime.scheduler_tick_ms",
            scheduler_tick_ms,
            "a nonzero interval whose nanoseconds fit in i64",
        ));
    }
    Ok(RuntimeValues {
        cpu_capacity,
        task_stack_size,
        scheduler_tick_ms,
    })
}

/// Resolve runtime inputs for a Cargo build script and declare their invalidation.
pub fn load_build_env() -> Result<RuntimeValues, ConfigError> {
    for key in ["SMP", "AX_TASK_STACK_SIZE", "AX_SCHEDULER_TICK_MS"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    resolve_runtime(None, RuntimeOptions::default(), &RuntimeEnv::capture()?)
}

fn capture(field: &'static str) -> Result<Option<String>, ConfigError> {
    match env::var(field) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(source) => Err(ConfigError::Encoding { field, source }),
    }
}

fn choose<T: Copy>(
    typed: Option<T>,
    raw: Option<&str>,
    default: T,
    field: &'static str,
    parse: fn(&str) -> Result<T, ParseIntError>,
) -> Result<T, ConfigError> {
    if let Some(value) = typed {
        return Ok(value);
    }
    match raw {
        None => Ok(default),
        Some(value) => parse(value).map_err(|source| ConfigError::Number {
            field,
            value: value.into(),
            source,
        }),
    }
}

fn parse_usize(value: &str) -> Result<usize, ParseIntError> {
    let value = value.replace('_', "");
    if let Some(hex) = value.strip_prefix("0x") {
        usize::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
}

fn parse_u64(value: &str) -> Result<u64, ParseIntError> {
    let value = value.replace('_', "");
    if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
}

fn validate_stack(size: usize) -> Result<(), ConfigError> {
    let invalid = || {
        ConfigError::invalid(
            "runtime.task_stack_size",
            size,
            "a nonzero 16-byte-aligned size with a representable rounded allocation and \
             guard-page sum",
        )
    };
    if size == 0 || !size.is_multiple_of(16) {
        return Err(invalid());
    }
    let rounded = size.checked_add(PAGE_SIZE - 1).ok_or_else(invalid)? & !(PAGE_SIZE - 1);
    rounded.checked_add(PAGE_SIZE).ok_or_else(invalid)?;
    Layout::from_size_align(rounded, PAGE_SIZE).map_err(|source| ConfigError::StackLayout {
        field: "runtime.task_stack_size",
        value: size,
        source,
    })?;
    Ok(())
}
