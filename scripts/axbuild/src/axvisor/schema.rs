use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use schemars::generate::SchemaSettings;

/// Generate the complete guest configuration schema as JSON Schema Draft 7.
pub(super) fn generate_guest_config_schema(output: &Path) -> anyhow::Result<PathBuf> {
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<axvmconfig::GuestConfig>();
    let mut json =
        serde_json::to_string_pretty(&schema).context("serialize guest config schema")?;
    json.push('\n');

    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create schema output directory {}", parent.display()))?;
    fs::write(output, json)
        .with_context(|| format!("write guest config schema {}", output.display()))?;
    ensure!(
        output.is_file(),
        "guest config schema was not created at {}",
        output.display()
    );
    Ok(output.to_path_buf())
}
