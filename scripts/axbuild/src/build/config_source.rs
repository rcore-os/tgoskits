use std::path::{Path, PathBuf};

use anyhow::Context;
use ax_config::{ConfigError, apply_overlay, parse_overlay};

use super::{BuildConfiguration, reject_extra_fields};

/// Selection identity, not a second copy of the decoded configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Preset(String),
    File {
        path: PathBuf,
        preset: Option<String>,
    },
    Custom,
}

impl ConfigSource {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::File { path, .. } => Some(path),
            Self::Preset(_) | Self::Custom => None,
        }
    }

    pub fn preset(&self) -> Option<&str> {
        match self {
            Self::Preset(name) => Some(name),
            Self::File { preset, .. } => preset.as_deref(),
            Self::Custom => None,
        }
    }
}

/// Decoded input before CLI selection and runtime validation.
///
/// Only field provenance needed after decoding is retained; the raw overlay is
/// dropped before this value leaves the parser.
pub(crate) struct ConfigInput<T> {
    pub config: T,
    pub source: ConfigSource,
    pub target_constraint: Option<String>,
    pub app_c_from_file: bool,
}

impl<T: BuildConfiguration> ConfigInput<T> {
    /// Select the OS's standard QEMU recipe only for a source-free request.
    pub fn select_default_qemu(&mut self, target: &str) -> anyhow::Result<()> {
        if self.source == ConfigSource::Custom
            && let Some(name) = T::default_qemu_preset(target)
        {
            *self = parse_config_input(None, Some(name))?;
        }
        Ok(())
    }

    /// A preset constrains selectors; a custom file remains overridable by CLI.
    pub fn check_selectors(&self, target: Option<&str>, arch: Option<&str>) -> anyhow::Result<()> {
        let Some(expected) = &self.target_constraint else {
            return Ok(());
        };
        if let Some(actual) = target
            && actual != expected
        {
            return Err(selector_conflict(
                &self.source,
                "target",
                expected.clone(),
                actual.to_owned(),
            ));
        }
        let expected_arch = crate::context::arch_for_target_checked(expected)?;
        if let Some(actual) = arch
            && actual != expected_arch
        {
            return Err(selector_conflict(
                &self.source,
                "arch",
                expected_arch.to_owned(),
                actual.to_owned(),
            ));
        }
        Ok(())
    }
}

/// Read a selected user file once, without generating an implicit input file.
pub(crate) fn load_config_input<T: BuildConfiguration>(
    workspace_root: &Path,
    path: Option<&Path>,
    preset: Option<&str>,
) -> anyhow::Result<ConfigInput<T>> {
    let Some(path) = path else {
        return parse_config_input(None, preset);
    };
    let path = std::path::absolute(workspace_root.join(path))
        .with_context(|| format!("failed to resolve build config {}", path.display()))?;
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read build config {}", path.display()))?;
    parse_config_input(Some((&path, &contents)), preset)
        .with_context(|| format!("invalid build config {}", path.display()))
}

/// Compose the source and extract the final OS type once.
pub(crate) fn parse_config_input<T: BuildConfiguration>(
    file: Option<(&Path, &str)>,
    cli_preset: Option<&str>,
) -> anyhow::Result<ConfigInput<T>> {
    let overlay = file
        .map(|(path, contents)| {
            let source = std::path::absolute(path)
                .with_context(|| format!("failed to resolve build config {}", path.display()))?;
            Ok::<_, anyhow::Error>(parse_overlay(&source, contents)?)
        })
        .transpose()?;
    let file_preset = overlay
        .as_ref()
        .and_then(|overlay| overlay.controls.preset.as_deref());
    if let (Some(cli), Some(file)) = (cli_preset, file_preset)
        && cli != file
    {
        return Err(ConfigError::SelectorConflict {
            origin: overlay
                .as_ref()
                .map(|overlay| overlay.source().to_path_buf()),
            field: "preset".into(),
            expected: cli.to_owned(),
            actual: file.to_owned(),
        }
        .into());
    }
    let preset = cli_preset.or(file_preset);
    let source = match &overlay {
        Some(overlay) => ConfigSource::File {
            path: overlay.source().to_path_buf(),
            preset: preset.map(str::to_owned),
        },
        None => preset
            .map(|name| ConfigSource::Preset(name.to_owned()))
            .unwrap_or(ConfigSource::Custom),
    };
    let base = match preset {
        Some(name) => T::preset(name).ok_or_else(|| ConfigError::PresetNotFound {
            origin: source.path().map(Path::to_path_buf),
            name: name.to_owned(),
        })?,
        None => T::default(),
    };
    let target_constraint = preset.and(base.common().target.clone());
    let config = match &overlay {
        Some(overlay) => {
            let config: T = apply_overlay(&base, overlay, T::CLEARABLE_FIELDS)?;
            reject_extra_fields(config.extra_fields())?;
            config.check_source(overlay)?;
            config
        }
        None => base,
    };
    if let Some(expected) = &target_constraint
        && config.common().target.as_deref() != Some(expected)
    {
        return Err(selector_conflict(
            &source,
            "target",
            expected.clone(),
            config.common().target.clone().unwrap_or_default(),
        ));
    }
    let app_c_from_file = overlay
        .as_ref()
        .is_some_and(|overlay| overlay.contains_field("app-c"));
    Ok(ConfigInput {
        config,
        source,
        target_constraint,
        app_c_from_file,
    })
}

fn selector_conflict(
    source: &ConfigSource,
    field: &str,
    expected: String,
    actual: String,
) -> anyhow::Error {
    ConfigError::SelectorConflict {
        origin: source.path().map(Path::to_path_buf),
        field: field.to_owned(),
        expected,
        actual,
    }
    .into()
}
