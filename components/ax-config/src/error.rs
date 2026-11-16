use std::{error::Error, num::ParseIntError, path::PathBuf};

/// Failures in configuration syntax, composition, or value validation.
///
/// `origin` identifies the input file when known. Field paths are logical paths,
/// not promises of source spans after deserialization or merging.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Invalid TOML syntax, retaining the parser's original span and cause.
    #[cfg(feature = "toml")]
    #[error("invalid TOML in {origin:?}: {source}")]
    Syntax {
        origin: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// A value could not be serialized or strictly deserialized.
    #[error("configuration type error at {field} ({origin:?}): {source}")]
    Type {
        origin: Option<PathBuf>,
        field: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// A field is not part of the selected configuration contract.
    #[error("unknown field {field} ({origin:?}); {hint:?}")]
    UnknownField {
        origin: Option<PathBuf>,
        field: String,
        hint: Option<&'static str>,
        #[source]
        source: Option<Box<dyn Error + Send + Sync>>,
    },
    /// A well-typed value violates a field's semantic rules.
    #[error("invalid {field}={value:?} ({origin:?}): expected {expected}")]
    InvalidValue {
        origin: Option<PathBuf>,
        field: String,
        value: String,
        expected: &'static str,
    },
    /// A process environment value is not Unicode.
    #[error("environment variable {field} is not Unicode: {source}")]
    Encoding {
        field: &'static str,
        #[source]
        source: std::env::VarError,
    },
    /// The winning environment input is not a representable unsigned integer.
    #[error("invalid numeric environment variable {field}={value:?}: {source}")]
    Number {
        field: &'static str,
        value: String,
        #[source]
        source: ParseIntError,
    },
    /// A rounded task allocation is outside the allocator's layout domain.
    #[error("invalid stack layout for {field}={value}: {source}")]
    StackLayout {
        field: &'static str,
        value: usize,
        #[source]
        source: std::alloc::LayoutError,
    },
    /// The requested preset does not exist in the selected OS.
    #[error("preset {name:?} was not found ({origin:?})")]
    PresetNotFound {
        origin: Option<PathBuf>,
        name: String,
    },
    /// Two explicit selectors or feature edits disagree.
    #[error("conflicting {field} ({origin:?}): {expected:?} versus {actual:?}")]
    SelectorConflict {
        origin: Option<PathBuf>,
        field: String,
        expected: String,
        actual: String,
    },
    /// An input Schema cannot be assembled without violating its contract.
    #[cfg(all(feature = "schema", feature = "toml"))]
    #[error("invalid input Schema: {reason}")]
    Schema { reason: String },
}

impl ConfigError {
    pub(crate) fn invalid(
        field: impl Into<String>,
        value: impl ToString,
        expected: &'static str,
    ) -> Self {
        Self::InvalidValue {
            origin: None,
            field: field.into(),
            value: value.to_string(),
            expected,
        }
    }
}
