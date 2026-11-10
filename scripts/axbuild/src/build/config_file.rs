use ax_config::{ConfigError, apply_overlay, parse_overlay};

use super::*;

/// The OS owns validation of the final flattened input, not the merge engine.
pub(crate) trait BuildConfiguration: Default + Serialize + DeserializeOwned {
    const CLEARABLE_FIELDS: &'static [&'static str];

    /// PR1-only file defaults applied before the user's sparse overlay.
    /// The parser supplies an absolute source path.
    fn file_base(_path: &Path) -> Self {
        Self::default()
    }

    fn extra_fields(&self) -> &toml::Table;

    /// Validate the final values after CLI target/package/runtime selection.
    fn validate_input(&self) -> Result<(), ConfigError>;
}

pub(crate) fn parse_build_config<T: BuildConfiguration>(
    path: &Path,
    contents: &str,
) -> anyhow::Result<T> {
    let source = std::path::absolute(path)
        .with_context(|| format!("failed to resolve build config path {}", path.display()))?;
    let overlay = parse_overlay(&source, contents)?;
    // PR1 keeps the existing board files as its only preset source. A preset
    // control must not disappear merely because the static registry is pending.
    if let Some(name) = &overlay.controls.preset {
        return Err(ConfigError::PresetNotFound {
            origin: Some(source),
            name: name.clone(),
        }
        .into());
    }
    let config: T = apply_overlay(&T::file_base(&source), &overlay, T::CLEARABLE_FIELDS)?;
    reject_extra_fields(config.extra_fields())
        .with_context(|| format!("invalid build config {}", path.display()))?;
    Ok(config)
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

// PR1 deliberately retains default file materialization. Explicit files are
// checked by the resolver and never pass through this missing-default branch.
pub(crate) fn ensure_build_info<T>(path: &Path, default: impl FnOnce() -> T) -> anyhow::Result<()>
where
    T: Serialize,
{
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&default())?)?;
    Ok(())
}
