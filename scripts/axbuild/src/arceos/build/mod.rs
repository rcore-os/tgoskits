mod c_app;
mod cargo_config;
mod info;
#[cfg(test)]
mod tests;
mod types;

pub use ax_config::{CommonConfig, LogLevel};
pub(crate) use c_app::{arceos_build_mode, resolve_app_c_dir};
pub(crate) use cargo_config::{
    load_c_app_cargo_config, load_cargo_config, load_cargo_config_with_default_features,
};
pub(crate) use types::ArceosBuildMode;

pub use super::config::ArceosConfig;
