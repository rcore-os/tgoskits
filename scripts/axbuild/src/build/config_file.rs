use ax_config::ConfigError;

use super::*;

/// The OS owns validation of the final flattened input, not the merge engine.
pub(crate) trait BuildConfiguration: Default + Serialize + DeserializeOwned {
    const CLEARABLE_FIELDS: &'static [&'static str];

    fn preset(name: &str) -> Option<Self>;

    fn default_qemu_preset(target: &str) -> Option<&'static str>;

    fn common(&self) -> &CommonConfig;

    /// Reject legacy ambiguous resource selection before the source is discarded.
    fn check_source(&self, _overlay: &ax_config::Overlay) -> anyhow::Result<()> {
        Ok(())
    }

    fn extra_fields(&self) -> &toml::Table;

    /// Validate the final values after CLI target/package/runtime selection.
    fn validate_input(&self) -> Result<(), ConfigError>;
}

pub(crate) fn parse_build_config<T: BuildConfiguration>(
    path: &Path,
    contents: &str,
) -> anyhow::Result<T> {
    Ok(super::config_source::parse_config_input::<T>(Some((path, contents)), None)?.config)
}

pub(crate) fn load_build_config<T: BuildConfiguration>(path: &Path) -> anyhow::Result<T> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read build config {}", path.display()))?;
    parse_build_config(path, &contents)
        .with_context(|| format!("failed to parse build config {}", path.display()))
}

pub(crate) fn reject_extra_fields(extra: &toml::Table) -> Result<(), ConfigError> {
    if let Some((field, _)) = extra.iter().next() {
        return Err(ConfigError::UnknownField {
            origin: None,
            field: field.clone(),
            source: None,
            hint: match field.as_str() {
                "std" => Some("std-aware Rust builds are the default; remove `std`"),
                "plat_dyn" => Some("dynamic platform selection is automatic; remove `plat_dyn`"),
                _ => None,
            },
        });
    }
    Ok(())
}

pub(crate) fn invalid_config_value(
    field: &str,
    value: impl ToString,
    expected: &'static str,
) -> ConfigError {
    ConfigError::InvalidValue {
        origin: None,
        field: field.to_string(),
        value: value.to_string(),
        expected,
    }
}

pub(crate) fn validate_config_target(common: &CommonConfig) -> Result<(), ConfigError> {
    let target = common.target.as_deref().ok_or_else(|| {
        invalid_config_value("target", "<unspecified>", "a resolved build target")
    })?;
    crate::context::arch_for_target_checked(target)
        .map_err(|_| invalid_config_value("target", target, "a supported kernel build target"))?;
    Ok(())
}

pub(crate) fn validate_resource_path(field: &str, path: &Path) -> Result<(), ConfigError> {
    if path.as_os_str().is_empty() {
        return Err(invalid_config_value(field, "", "a nonempty resource path"));
    }
    Ok(())
}
