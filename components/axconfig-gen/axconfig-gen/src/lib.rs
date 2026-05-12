#![doc = include_str!("../README.md")]

mod config;
mod output;
mod ty;
mod value;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use toml_edit::TomlError;

pub use self::{
    config::{Config, ConfigItem},
    output::OutputFormat,
    ty::ConfigType,
    value::ConfigValue,
};

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

impl std::error::Error for ConfigErr {}

/// A specialized [`Result`] type with [`ConfigErr`] as the error type.
pub type ConfigResult<T> = Result<T, ConfigErr>;

/// Options for loading, merging, updating, and writing config files.
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// Config specification files merged in order.
    pub specs: Vec<PathBuf>,
    /// Optional old config used to preserve existing values.
    pub oldconfig: Option<PathBuf>,
    /// Optional output file. If absent, generated text is returned only.
    pub output: Option<PathBuf>,
    /// Output format.
    pub fmt: OutputFormat,
    /// Values to override after specs and oldconfig are loaded.
    pub writes: Vec<String>,
    /// Whether to keep a `.old.*` backup when overwriting a changed output file.
    pub keep_backup: bool,
}

impl GenerateOptions {
    /// Create TOML generation options from config specification paths.
    pub fn new(specs: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            specs: specs.into_iter().collect(),
            oldconfig: None,
            output: None,
            fmt: OutputFormat::Toml,
            writes: Vec::new(),
            keep_backup: false,
        }
    }
}

/// Result of a config generation run.
#[derive(Debug, Clone)]
pub struct GenerateReport {
    /// Config items that were not present in the old config.
    pub untouched: Vec<ConfigItem>,
    /// Old config items that are not present in the specification.
    pub extra: Vec<ConfigItem>,
    /// Generated output text.
    pub output: String,
}

/// Parse a config read argument in `key` or `table.key` form.
pub fn parse_config_read_arg(arg: &str) -> ConfigResult<(String, String)> {
    if let Some((table, key)) = arg.split_once('.') {
        Ok((table.into(), key.into()))
    } else {
        Ok((Config::GLOBAL_TABLE_NAME.into(), arg.into()))
    }
}

/// Parse a config write argument in `key=value` or `table.key=value` form.
pub fn parse_config_write_arg(arg: &str) -> ConfigResult<(String, String, String)> {
    let (item, value) = arg.split_once('=').ok_or_else(|| {
        ConfigErr::Other(format!(
            "Invalid config setting command `{}`, expected `table.key=value`",
            arg
        ))
    })?;
    if let Some((table, key)) = item.split_once('.') {
        Ok((table.into(), key.into(), value.into()))
    } else {
        Ok((Config::GLOBAL_TABLE_NAME.into(), item.into(), value.into()))
    }
}

/// Load and merge config specification files.
pub fn load_config_specs(specs: &[PathBuf]) -> ConfigResult<Config> {
    let mut config = Config::new();
    for spec in specs {
        let spec_toml = std::fs::read_to_string(spec).map_err(|err| {
            ConfigErr::Other(format!(
                "Failed to read config specification file {}: {}",
                spec.display(),
                err
            ))
        })?;
        let sub_config = Config::from_toml(&spec_toml)?;
        config.merge(&sub_config)?;
    }
    Ok(config)
}

/// Load one config file.
pub fn load_config(path: impl AsRef<Path>) -> ConfigResult<Config> {
    let path = path.as_ref();
    let toml = std::fs::read_to_string(path).map_err(|err| {
        ConfigErr::Other(format!(
            "Failed to read config file {}: {}",
            path.display(),
            err
        ))
    })?;
    Config::from_toml(&toml)
}

/// Apply write overrides to a loaded config.
pub fn apply_config_writes(config: &mut Config, writes: &[String]) -> ConfigResult<()> {
    for arg in writes {
        let (table, key, value) = parse_config_write_arg(arg)?;
        let new_value = ConfigValue::new(&value)?;
        let item = config
            .config_at_mut(&table, &key)
            .ok_or_else(|| ConfigErr::Other(format!("Config item `{}` not found", arg)))?;
        item.value_mut().update(new_value)?;
    }
    Ok(())
}

/// Generate config output from specs, optional old config, and write overrides.
pub fn generate_config(options: &GenerateOptions) -> ConfigResult<GenerateReport> {
    let mut config = load_config_specs(&options.specs)?;
    let (untouched, extra) = if let Some(oldconfig_path) = &options.oldconfig {
        let oldconfig = load_config(oldconfig_path)?;
        config.update(&oldconfig)?
    } else {
        (Vec::new(), Vec::new())
    };

    apply_config_writes(&mut config, &options.writes)?;
    let output = config.dump(options.fmt.clone())?;
    if let Some(path) = options.output.as_deref() {
        write_config_output(path, &output, options.keep_backup)?;
    }

    Ok(GenerateReport {
        untouched,
        extra,
        output,
    })
}

/// Read one config item value from merged specs.
pub fn read_config_value(specs: &[PathBuf], item: &str) -> ConfigResult<String> {
    let config = load_config_specs(specs)?;
    read_loaded_config_value(&config, item)
}

/// Read one string config item value from merged specs.
pub fn read_config_string(specs: &[PathBuf], item: &str) -> ConfigResult<String> {
    let config = load_config_specs(specs)?;
    read_loaded_config_string(&config, item)
}

/// Read one config item value from an already loaded config.
pub fn read_loaded_config_value(config: &Config, item: &str) -> ConfigResult<String> {
    Ok(find_config_item(config, item)?.value().to_toml_value())
}

/// Read one string config item value from an already loaded config.
pub fn read_loaded_config_string(config: &Config, item: &str) -> ConfigResult<String> {
    find_config_item(config, item)?
        .value()
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ConfigErr::Other(format!("Config item `{}` is not a string", item)))
}

fn find_config_item<'a>(config: &'a Config, item: &str) -> ConfigResult<&'a ConfigItem> {
    let (table, key) = parse_config_read_arg(item)?;
    config
        .config_at(&table, &key)
        .ok_or_else(|| ConfigErr::Other(format!("Config item `{}` not found", item)))
}

fn write_config_output(path: &Path, output: &str, keep_backup: bool) -> ConfigResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            ConfigErr::Other(format!(
                "Failed to create output directory {}: {}",
                parent.display(),
                err
            ))
        })?;
    }
    if let Ok(oldconfig) = std::fs::read_to_string(path) {
        if oldconfig == output {
            return Ok(());
        }
        if keep_backup {
            let bak_path = if let Some(ext) = path.extension() {
                path.with_extension(format!("old.{}", ext.to_string_lossy()))
            } else {
                path.with_extension("old")
            };
            std::fs::write(&bak_path, oldconfig).map_err(|err| {
                ConfigErr::Other(format!(
                    "Failed to write backup config file {}: {}",
                    bak_path.display(),
                    err
                ))
            })?;
        }
    }
    std::fs::write(path, output).map_err(|err| {
        ConfigErr::Other(format!(
            "Failed to write config file {}: {}",
            path.display(),
            err
        ))
    })
}
