use ::std::fs;
use tempfile::tempdir;

use super::*;

fn repo_metadata() -> cargo_metadata::Metadata {
    workspace_metadata().unwrap()
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

mod info;
mod metadata;
mod platform;
mod std_features;
mod std_linker;
mod std_metadata;
mod std_targets;
mod target_specs;
