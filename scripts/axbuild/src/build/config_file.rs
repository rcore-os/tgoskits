use super::*;

pub(crate) fn ensure_build_info<T>(path: &Path, default: impl FnOnce() -> T) -> anyhow::Result<()>
where
    T: Serialize,
{
    println!("Using build config: {}", path.display());

    if path.exists() {
        info!("Found build config at {}", path.display());
        return Ok(());
    }

    info!(
        "Build config not found at {}, writing default config",
        path.display()
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let default = default();
    std::fs::write(path, toml::to_string_pretty(&default)?)?;
    Ok(())
}

pub(crate) fn load_build_info<T>(path: &Path) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let contents = std::fs::read_to_string(path)?;
    reject_removed_std_field(path, &contents)?;
    toml::from_str::<T>(&contents)
        .with_context(|| format!("failed to parse build info {}", path.display()))
}

pub(crate) fn reject_removed_std_field(path: &Path, contents: &str) -> anyhow::Result<()> {
    if let Ok(table) = toml::from_str::<toml::Table>(contents)
        && table.contains_key("std")
    {
        bail!(
            "build config {} uses removed `std` field; std-aware Rust builds are now the default, \
             remove `std = ...`",
            path.display()
        );
    }

    Ok(())
}

pub(crate) fn reject_arceos_app_c_field(path: &Path, contents: &str) -> anyhow::Result<()> {
    if let Ok(table) = toml::from_str::<toml::Table>(contents)
        && table.contains_key("app-c")
    {
        bail!(
            "build config {} uses ArceOS-only `app-c` field; remove it or use an ArceOS build \
             command",
            path.display()
        );
    }

    Ok(())
}
