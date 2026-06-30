use ::std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::tempdir;
use walkdir::WalkDir;

use super::*;

fn metadata_for_manifest(manifest_path: &Path) -> cargo_metadata::Metadata {
    workspace_metadata_root_manifest(manifest_path).unwrap()
}

fn metadata_for_manifest_with_deps(manifest_path: &Path) -> cargo_metadata::Metadata {
    crate::context::workspace_metadata_root_manifest_with_deps(manifest_path).unwrap()
}

fn repo_metadata() -> cargo_metadata::Metadata {
    workspace_metadata().unwrap()
}

fn gnu_lld_pre_link_args(spec: &serde_json::Value) -> Vec<&str> {
    spec["pre-link-args"]["gnu-lld"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect()
}

fn temp_workspace(
    package_name: &str,
    dependency_block: &str,
) -> anyhow::Result<::std::path::PathBuf> {
    let root = tempdir()?.keep();

    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n\n[workspace.package]\nedition = \
         \"2024\"\n",
    )?;

    let app_dir = root.join("app");
    fs::create_dir_all(&app_dir)?;
    fs::write(
        app_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \
             \"2024\"\n\n[dependencies]\n{dependency_block}"
        ),
    )?;
    fs::create_dir_all(app_dir.join("src"))?;
    fs::write(app_dir.join("src/lib.rs"), "pub fn smoke() {}\n")?;

    Ok(root)
}

fn add_platform_package(
    workspace: &Path,
    package_name: &str,
    config_package_name: &str,
) -> anyhow::Result<()> {
    let platform_dir = workspace.join("platforms");
    fs::create_dir_all(platform_dir.join("src"))?;
    fs::write(
        platform_dir.join("Cargo.toml"),
        format!("[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
    )?;
    fs::write(platform_dir.join("src/lib.rs"), "")?;
    fs::write(
        platform_dir.join("axconfig.toml"),
        format!(
            "arch = \"aarch64\" # str\nplatform = \"custom-board\" # str\npackage = \
             \"{config_package_name}\" # str\n"
        ),
    )?;
    Ok(())
}

mod checked_configs;
mod config;
mod info;
mod metadata;
mod platform;
mod platform_config;
mod std_features;
mod std_linker;
mod std_metadata;
mod std_targets;
mod target_specs;
