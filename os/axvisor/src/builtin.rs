//! Axvisor policy for archive-owned guest configuration and boot assets.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    rc::Rc,
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::cell::RefCell;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail, ensure};
#[cfg(any(target_os = "none", target_env = "musl"))]
use ax_fs_ng::current_fs_context;
use ax_fs_ng::{
    VfsError,
    migration::{MigrationEntry, MigrationPlan, ResourceKind},
    vfs::FsContext,
};
use axvmconfig::{BUILTIN_GUEST_DIR, GuestConfig};

/// Absolute path of the dashboard package inside the built-in bundle: it is
/// the `web-ui` entry directly under [`BUILTIN_GUEST_DIR`]. `axbuild` packs
/// the frontend `dist` tree there when the `web-ui` feature is enabled, and
/// the HTTP control plane serves the dashboard from this path on whichever
/// root (initramfs memory root or installed disk root) is active.
pub const BUILTIN_WEB_UI_DIR: &str = "/guest/builtin/web-ui";

/// The initramfs namespace remains available after a disk root is committed so
/// builtin boot assets can still be read by a later lazy-start request. The
/// context is deliberately *not* created with [`FsContext::into_shared`]:
/// shared contexts are registered in `FS_REGISTRY`, and root switching would
/// then rewrite their root through `propagate_pivot_root`. Keeping this context
/// out of that registry leaves it anchored at the detached initramfs mount;
/// the mount owns an immutable memory filesystem and is independent of the
/// block filesystem released for passthrough.
static RETAINED_BOOT_CONTEXT: OnceLock<Arc<ax_fs_ng::os::sync::Mutex<FsContext>>> = OnceLock::new();

pub fn retain_boot_context(context: &FsContext) -> Result<()> {
    let mut retained = FsContext::new(context.root_dir().clone());
    // A copied `FsContext` still points into the active mount namespace. Clone
    // that namespace before the root switch so detaching the old root cannot
    // invalidate this archive owner. Keep the resulting context out of the
    // global registry: it must not receive the pivot-root propagation update.
    retained
        .unshare_mount_namespace()
        .context("isolate retained initramfs mount namespace")?;
    let retained = Arc::new(ax_fs_ng::os::sync::Mutex::new(retained));
    RETAINED_BOOT_CONTEXT.get_or_init(|| retained);
    Ok(())
}

fn read_retained_builtin_asset(path: &str) -> Option<Result<Vec<u8>>> {
    if !is_builtin_path(path) {
        return None;
    }
    let context = RETAINED_BOOT_CONTEXT.get()?;
    match context.lock().read(path) {
        Ok(bytes) => Some(Ok(bytes)),
        // A disk root may already contain a previously installed builtin
        // package when this boot's archive has no package. Let the normal
        // active-root provider serve that asset instead of turning the
        // retained archive's absence into a hard error.
        Err(VfsError::NotFound) => None,
        Err(error) => Some(Err(anyhow!(
            "read retained builtin asset `{path}`: {error}"
        ))),
    }
}

fn read_retained_builtin_asset_exact(path: &str, read_size: usize) -> Option<Result<Vec<u8>>> {
    if !is_builtin_path(path) {
        return None;
    }
    let context = RETAINED_BOOT_CONTEXT.get()?;
    let context = context.lock();
    let file = match ax_fs_ng::file::File::open(&context, path) {
        Ok(file) => file,
        Err(VfsError::NotFound) => return None,
        Err(error) => {
            return Some(Err(anyhow!(
                "open retained builtin asset `{path}`: {error}"
            )));
        }
    };
    let mut bytes = alloc::vec![0; read_size];
    match file.read_at(bytes.as_mut_slice(), 0) {
        Ok(count) if count == read_size => Some(Ok(bytes)),
        Ok(count) => Some(Err(anyhow!(
            "requested {read_size} bytes from retained builtin asset `{path}`, got {count}"
        ))),
        Err(error) => Some(Err(anyhow!(
            "read retained builtin asset `{path}`: {error}"
        ))),
    }
}

pub fn read_boot_asset(path: &str) -> Result<Option<Vec<u8>>> {
    match read_retained_builtin_asset(path) {
        Some(result) => result.map(Some),
        None => Ok(None),
    }
}

pub fn read_boot_asset_exact(path: &str, read_size: usize) -> Result<Option<Vec<u8>>> {
    match read_retained_builtin_asset_exact(path, read_size) {
        Some(result) => result.map(Some),
        None => Ok(None),
    }
}

pub fn boot_asset_size(path: &str) -> Result<Option<usize>> {
    if !is_builtin_path(path) {
        return Ok(None);
    }
    let Some(context) = RETAINED_BOOT_CONTEXT.get() else {
        return Ok(None);
    };
    let context = context.lock();
    match context.metadata(path) {
        Ok(metadata) => usize::try_from(metadata.size)
            .map(Some)
            .map_err(|_| anyhow!("retained builtin asset `{path}` is too large")),
        Err(VfsError::NotFound) => Ok(None),
        Err(error) => Err(anyhow!("inspect retained builtin asset `{path}`: {error}")),
    }
}

pub fn boot_asset_exists(path: &str) -> bool {
    if !is_builtin_path(path) {
        return false;
    }
    RETAINED_BOOT_CONTEXT.get().is_some_and(|context| {
        context
            .lock()
            .resolve(path)
            .is_ok_and(|asset| asset.check_is_file().is_ok())
    })
}

/// Returns whether a path is the builtin package root or one of its children.
pub fn is_builtin_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path == BUILTIN_GUEST_DIR
        || path
            .strip_prefix(BUILTIN_GUEST_DIR)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// One validated guest configuration shipped in the immutable builtin package.
///
/// Keeping the raw TOML in the catalog lets the runtime defer the expensive
/// AxVM preparation and resource allocation until a caller actually selects
/// the guest. The catalog itself only owns configuration text and metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinGuest {
    id: usize,
    name: String,
    path: String,
    raw: String,
}

impl BuiltinGuest {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn toml(&self) -> &str {
        &self.raw
    }
}

/// Immutable index of the guest configurations packaged under `/guest/builtin`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltinCatalog {
    entries: BTreeMap<usize, BuiltinGuest>,
}

impl BuiltinCatalog {
    pub fn get(&self, id: usize) -> Option<&BuiltinGuest> {
        self.entries.get(&id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &BuiltinGuest> {
        self.entries.values()
    }

    pub fn ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.entries.keys().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Installs archive assets before an explicitly requested disk root is committed.
#[cfg(any(target_os = "none", target_env = "musl"))]
pub fn prepare_root() -> Result<()> {
    let bootargs = ax_hal::boot::bootargs();
    let has_root = ax_fs_ng::bootargs::tokens(bootargs.unwrap_or(""))
        .into_iter()
        .take_while(|arg| arg != "--")
        .any(|arg| arg.starts_with("root="));
    let source_handle = current_fs_context();
    let source = source_handle.lock().clone();
    let has_block_devices = ax_fs_ng::block::runtime::BlockRuntime::installed_devices()
        .is_some_and(|devices| !devices.is_empty());
    if !has_root || !has_block_devices {
        log::info!("Axvisor uses initramfs root; disk root unavailable or not requested");
        retain_boot_context(&source)?;
        validate_builtin(&source, BUILTIN_GUEST_DIR)?;
        return Ok(());
    }
    let prepared =
        ax_fs_ng::root::prepare_block_root(bootargs).context("prepare Axvisor disk root")?;
    if prepared.context().root_dir().is_readonly() {
        bail!("Axvisor disk root is read-only; keeping the initramfs root");
    }
    install_builtin(&source, prepared.context())?;
    retain_boot_context(&source)?;
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
    Ok(config_files_with_paths(context, directory)?
        .into_iter()
        .map(|(_, content)| content)
        .collect())
}

fn config_files_with_paths(context: &FsContext, directory: &str) -> Result<Vec<(String, String)>> {
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
            Ok((path, content))
        })
        .collect()
}

/// Loads and validates the builtin catalog without creating any VM.
pub fn load_catalog(context: &FsContext) -> Result<BuiltinCatalog> {
    let directory = format!("{BUILTIN_GUEST_DIR}/configs");
    let entries = config_files_with_paths(context, &directory)
        .context("load built-in guest configuration")?;
    let mut catalog = BuiltinCatalog::default();
    for (path, raw) in entries {
        let config =
            GuestConfig::from_toml(&raw).with_context(|| format!("invalid guest config {path}"))?;
        let guest = BuiltinGuest {
            id: config.base.id,
            name: config.base.name,
            path,
            raw,
        };
        if catalog.entries.insert(guest.id, guest).is_some() {
            unreachable!("config_files_with_paths rejects duplicate builtin IDs");
        }
    }
    Ok(catalog)
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
            "web-ui" => validate_web_ui(context, staged)?,
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

/// Validates the bundled dashboard tree before publication.
fn validate_web_ui(context: &FsContext, staged: &str) -> Result<()> {
    let directory = format!("{staged}/web-ui");
    context
        .resolve(&directory)?
        .check_is_dir()
        .with_context(|| format!("check built-in dashboard package {BUILTIN_WEB_UI_DIR}"))?;
    let shell = context.resolve(format!("{directory}/index.html"))?;
    shell
        .check_is_file()
        .with_context(|| format!("check built-in dashboard shell {BUILTIN_WEB_UI_DIR}"))?;
    ensure!(
        shell.len()? > 0,
        "built-in dashboard shell {BUILTIN_WEB_UI_DIR}/index.html is empty"
    );
    Ok(())
}
