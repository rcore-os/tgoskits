//! Axvisor policy for archive-owned guest configuration and boot assets.

use alloc::{collections::BTreeSet, format, string::String, vec::Vec};

use anyhow::{Context, Result, bail, ensure};
#[cfg(any(target_os = "none", target_env = "musl"))]
use ax_fs_ng::current_fs_context;
use ax_fs_ng::{VfsError, vfs::FsContext};
use axvmconfig::{BUILTIN_GUEST_DIR, GuestConfig};

/// Installs archive assets before an explicitly requested disk root is committed.
#[cfg(any(target_os = "none", target_env = "musl"))]
pub fn prepare_root() -> Result<()> {
    let bootargs = ax_hal::boot::bootargs();
    let has_root = ax_fs_ng::bootargs::tokens(bootargs.unwrap_or(""))
        .into_iter()
        .take_while(|arg| arg != "--")
        .any(|arg| arg.starts_with("root="));
    let source = current_fs_context().lock().clone();
    let has_block_devices = ax_fs_ng::block::runtime::BlockRuntime::installed_devices()
        .is_some_and(|devices| !devices.is_empty());
    if !has_root || !has_block_devices {
        log::info!("Axvisor uses initramfs root; disk root unavailable or not requested");
        validate_builtin(&source, BUILTIN_GUEST_DIR)?;
        return Ok(());
    }
    let prepared =
        ax_fs_ng::root::prepare_block_root(bootargs).context("prepare Axvisor disk root")?;
    if prepared.context().root_dir().is_readonly() {
        bail!("Axvisor disk root is read-only; keeping the initramfs root");
    }
    ax_fs_ng::bundle::install_directory(
        &source,
        prepared.context(),
        BUILTIN_GUEST_DIR,
        |context, path| {
            validate_builtin(context, path).map_err(|error| {
                log::error!("invalid built-in guest package: {error:#}");
                VfsError::InvalidData
            })
        },
    )
    .context("install built-in guest package")?;
    // Release the copied archive context before committing; the runtime's root
    // and task contexts are replaced by commit and are then the last old owners.
    drop(source);
    prepared.commit().context("commit Axvisor disk root")
}

fn config_files(context: &FsContext, directory: &str) -> Result<Vec<String>> {
    let entries = match context.read_dir(directory) {
        Ok(entries) => entries,
        Err(VfsError::NotFound) => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("read configuration directory {directory}"));
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.name.ends_with(".toml") {
            paths.push(format!("{directory}/{}", entry.name));
        }
    }
    paths.sort();
    let mut ids = BTreeSet::new();
    paths
        .into_iter()
        .map(|path| {
            let content = context
                .read_to_string(&path)
                .with_context(|| format!("read guest config {path}"))?;
            if content.trim().is_empty() {
                bail!("guest config {path} is empty");
            }
            let config = GuestConfig::from_toml(&content)
                .with_context(|| format!("invalid guest config {path}"))?;
            if !ids.insert(config.base.id) {
                bail!("duplicate VM ID {} in {directory}", config.base.id);
            }
            Ok(content)
        })
        .collect()
}

/// Validates the staged package while preserving its final absolute paths.
pub fn validate_builtin(context: &FsContext, staged: &str) -> Result<()> {
    let entries = match context.read_dir(staged) {
        Ok(entries) => entries,
        Err(VfsError::NotFound) => return Ok(()),
        Err(error) => return Err(error).context("read built-in guest package"),
    };
    let mut nonempty = false;
    for entry in entries {
        let entry = entry?;
        match entry.name.as_str() {
            "." | ".." => {}
            "configs" | "images" => {
                nonempty = true;
                context
                    .resolve(format!("{staged}/{}", entry.name))?
                    .check_is_dir()?;
            }
            _ => bail!("unexpected entry in built-in guest package: {}", entry.name),
        }
    }
    if !nonempty {
        return Ok(());
    }
    context
        .resolve(format!("{staged}/configs"))?
        .check_is_dir()?;
    for content in config_files(context, &format!("{staged}/configs"))? {
        let config = GuestConfig::from_toml(&content)?;
        for path in config.kernel.boot_image_paths() {
            let Some(suffix) = path
                .strip_prefix(BUILTIN_GUEST_DIR)
                .filter(|suffix| suffix.starts_with("/images/"))
            else {
                // Physical-board handoffs may provide guest images from the
                // published disk root (for example `/linux/...`).  The
                // configuration itself is still carried by the builtin
                // package; those external paths are resolved only after a
                // disk root has been selected.
                ensure!(
                    path.starts_with('/'),
                    "boot asset path must be absolute: {path}"
                );
                continue;
            };
            if suffix.split('/').any(|part| matches!(part, "." | "..")) {
                bail!("invalid built-in boot asset path: {path}");
            }
            let resource = context.resolve(format!("{staged}{suffix}"))?;
            resource.check_is_file()?;
            if resource.len()? == 0 {
                bail!("empty built-in boot asset: {path}");
            }
        }
    }
    Ok(())
}

/// Uses a valid nonempty user configuration set before the installed defaults.
/// Invalid user configuration is an error and never triggers a fallback.
pub fn selected_configs(context: &FsContext) -> Result<Vec<String>> {
    let user =
        config_files(context, "/guest/vm_default").context("load user guest configuration")?;
    if !user.is_empty() {
        return Ok(user);
    }
    config_files(context, &format!("{BUILTIN_GUEST_DIR}/configs"))
        .context("load built-in guest configuration")
}
