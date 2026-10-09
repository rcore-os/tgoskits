//! Axvisor policy for archive-owned guest configuration and boot assets.

use alloc::{collections::BTreeSet, format, rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;

use anyhow::{Context, Result, anyhow, bail, ensure};
#[cfg(any(target_os = "none", target_env = "musl"))]
use ax_fs_ng::current_fs_context;
use ax_fs_ng::{
    VfsError,
    migration::{MigrationEntry, MigrationPlan, ResourceKind},
    vfs::FsContext,
};
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
    install_builtin(&source, prepared.context())?;
    // Release the copied archive context before committing; the runtime's root
    // and task contexts are replaced by commit and are then the last old owners.
    drop(source);
    prepared.commit().context("commit Axvisor disk root")
}

/// Replaces the built-in package after validating its assets on the target root.
/// A missing source preserves the installed version and returns `false`.
/// The caller must exclude concurrent writers until installation finishes.
pub fn install_builtin(source: &FsContext, target: &FsContext) -> Result<bool> {
    let validation_error = Rc::new(RefCell::new(None));
    let validation_error_for_validator = Rc::clone(&validation_error);
    let mut plan = MigrationPlan::new();
    plan.add(
        MigrationEntry::new(
            ResourceKind::Immutable,
            BUILTIN_GUEST_DIR,
            BUILTIN_GUEST_DIR,
        )
        .with_validator(move |context, path| {
            validate_builtin_assets(context, path, Some(context)).map_err(|error| {
                validation_error_for_validator.replace(Some(format!("{error:#}")));
                VfsError::InvalidData
            })
        }),
    )?;
    let result = plan.execute(source, target);
    drop(plan);
    let report = result
        .map_err(|error| {
            if error == VfsError::InvalidData {
                if let Some(validation) = validation_error.borrow_mut().take() {
                    return anyhow!(validation);
                }
            }
            anyhow::Error::from(error)
        })
        .context("install built-in guest package")?;
    Ok(report.migrated != 0)
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

/// Validates archive-owned assets while preserving their final absolute paths.
/// External absolute assets are deferred to VM loading on an initramfs root.
pub fn validate_builtin(context: &FsContext, staged: &str) -> Result<()> {
    validate_builtin_assets(context, staged, None)
}

fn validate_builtin_assets(
    context: &FsContext,
    staged: &str,
    external_root: Option<&FsContext>,
) -> Result<()> {
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
            "configs" | "images" | "symbols" => {
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
            let resource = if let Some(suffix) = path
                .strip_prefix(BUILTIN_GUEST_DIR)
                .filter(|suffix| suffix.starts_with("/images/"))
            {
                if suffix.split('/').any(|part| matches!(part, "." | "..")) {
                    bail!("invalid built-in boot asset path: {path}");
                }
                context.resolve(format!("{staged}{suffix}"))
            } else {
                // Physical-board handoffs may provide guest images from the
                // prepared disk root. Validate them before publishing the
                // package or committing the root switch.
                ensure!(
                    path.starts_with('/'),
                    "boot asset path must be absolute: {path}"
                );
                let Some(external_root) = external_root else {
                    continue;
                };
                external_root.resolve(path)
            }
            .with_context(|| format!("resolve guest boot asset {path}"))?;
            resource
                .check_is_file()
                .with_context(|| format!("guest boot asset is not a file: {path}"))?;
            if resource.len()? == 0 {
                bail!("empty guest boot asset: {path}");
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
