//! Host-side configuration shared by build tools and kernel build scripts.
//!
//! The base API owns configuration data and pure runtime validation. Optional
//! TOML overlays never read files or modify process environment variables.

pub mod boards;
mod error;
#[cfg(feature = "toml")]
mod overlay;
mod preset;
mod runtime;
mod schema;

pub use error::ConfigError;
#[cfg(feature = "toml")]
pub use overlay::{Overlay, OverlayControls, apply_overlay, parse_overlay};
pub use preset::{Board, CommonDefaults, CommonPreset};
pub use runtime::{
    DEFAULT_CPU_CAPACITY, DEFAULT_SCHEDULER_TICK_MS, DEFAULT_TASK_STACK_SIZE, RuntimeEnv,
    RuntimeValues, load_build_env, resolve_runtime,
};
#[cfg(all(feature = "schema", feature = "toml"))]
pub use schema::input_schema;
pub use schema::{CommonConfig, LogLevel, RuntimeOptions};
#[cfg(feature = "schema")]
pub use schemars::{JsonSchema, Schema};
