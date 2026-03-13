#![doc = include_str!("../README.md")]

mod config;
mod output;
mod ty;
mod value;

#[cfg(test)]
mod tests;

use toml_edit::TomlError;

pub use self::config::{Config, ConfigItem};
pub use self::output::OutputFormat;
pub use self::ty::ConfigType;
pub use self::value::ConfigValue;

/// The error type on config parsing.
pub enum ConfigErr {
    /// TOML parsing error.
    Parse(TomlError),
    /// Invalid config value.
    InvalidValue,
    /// Invalid config type.
    InvalidType,
    /// Config value and type mismatch.
    ValueTypeMismatch,
    /// Other error.
    Other(String),
}

impl From<TomlError> for ConfigErr {
    fn from(e: TomlError) -> Self {
        Self::Parse(e)
    }
}

impl core::fmt::Display for ConfigErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "{}", e),
            Self::InvalidValue => write!(f, "Invalid config value"),
            Self::InvalidType => write!(f, "Invalid config type"),
            Self::ValueTypeMismatch => write!(f, "Config value and type mismatch"),
            Self::Other(s) => write!(f, "{}", s),
        }
    }
}

impl core::fmt::Debug for ConfigErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self)
    }
}

/// A specialized [`Result`] type with [`ConfigErr`] as the error type.
pub type ConfigResult<T> = Result<T, ConfigErr>;
