//! Basic virtual filesystem support

pub(crate) mod cgroup;
pub mod debug;
pub mod dev;
mod device;
mod dir;
mod dyn_debug;
mod file;
mod fs;
pub(crate) mod overlay;
pub(crate) mod proc;
mod sysfs;
mod tmp;
pub(crate) mod usbfs;

use alloc::{boxed::Box, sync::Arc};

use ax_errno::LinuxResult;
use ax_fs_ng::vfs::{FsContext, current_fs_context};
use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{DirNodeOps, FileNodeOps, Filesystem, NodePermission, VfsError, WeakDirEntry};
pub use tmp::MemoryFs;

pub use self::{device::*, dir::*, file::*, fs::*};

/// A callback that builds a `Arc<dyn DirNodeOps>` for a given
/// `WeakDirEntry`.
pub type DirMaker = Arc<dyn Fn(WeakDirEntry) -> Arc<dyn DirNodeOps> + Send + Sync>;

/// An enum containing either a directory ([`DirMaker`]) or a file (`Arc<dyn
/// FileNodeOps>`).
#[derive(Clone)]
pub enum NodeOpsMux {
    /// A directory node.
    Dir(DirMaker),
    /// A file node.
    File(Arc<dyn FileNodeOps>),
}

enum NodeOpsMuxTy {
    Static(NodeOpsMux),
    Dynamic(Box<dyn Fn() -> NodeOpsMux + Send + Sync>),
}

impl From<DirMaker> for NodeOpsMux {
    fn from(maker: DirMaker) -> Self {
        Self::Dir(maker)
    }
}

impl<T: FileNodeOps> From<Arc<T>> for NodeOpsMux {
    fn from(ops: Arc<T>) -> Self {
        Self::File(ops)
    }
}

const DIR_PERMISSION: NodePermission = NodePermission::from_bits_truncate(0o755);

static SHM_TMPFS: LazyInit<Arc<tmp::MemoryFs>> = LazyInit::new();
static TMP_TMPFS: LazyInit<Arc<tmp::MemoryFs>> = LazyInit::new();

pub fn shm_tmpfs() -> Option<Arc<tmp::MemoryFs>> {
    SHM_TMPFS.get().map(Arc::clone)
}

pub fn tmp_tmpfs() -> Option<Arc<tmp::MemoryFs>> {
    TMP_TMPFS.get().map(Arc::clone)
}

fn mount_at(fs: &FsContext, path: &str, mount_fs: Filesystem) -> LinuxResult<()> {
    let result = fs.with_namespace_operation(|namespace| {
        let loc = match namespace.resolve_path(path) {
            Ok(location) => location,
            Err(VfsError::NotFound) => {
                let (parent, name) = match namespace.parent_for_create(path.as_ref()) {
                    Ok(parent) => parent,
                    Err(error) => {
                        warn!("pseudofs mount failed while resolving parent: path={path} error={error:?}");
                        return Err(error);
                    }
                };
                match parent.create(
                    name,
                    axfs_ng_vfs::NodeType::Directory,
                    DIR_PERMISSION,
                    0,
                    0,
                ) {
                    Ok(location) => location,
                    Err(error) => {
                        warn!("pseudofs mount failed while creating target: path={path} error={error:?}");
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                warn!("pseudofs mount failed while resolving target: path={path} error={error:?}");
                return Err(error);
            }
        };
        if let Err(error) = loc.mount_filesystem(&mount_fs, false) {
            warn!("pseudofs mount failed while attaching filesystem: path={path} error={error:?}");
            return Err(error);
        }
        Ok(())
    });
    if let Err(error) = result {
        warn!(
            "pseudofs mount transaction failed: fs={} path={path} error={error:?}",
            mount_fs.name()
        );
        return Err(error.into());
    }
    info!("Mounted {} at {}", mount_fs.name(), path);
    Ok(())
}

/// Mount all filesystems
pub fn mount_all() -> LinuxResult<()> {
    info!("Initialize pseudofs...");

    let fs_context = current_fs_context();
    let fs = fs_context.lock();
    mount_at(&fs, "/dev", dev::new_devfs())?;
    let usbfs = usbfs::new_usbfs().inspect_err(|error| {
        warn!("USB filesystem construction failed before mount: {error:?}");
    })?;
    if let Some(dev_usbfs) = usbfs {
        mount_at(&fs, "/dev/bus/usb", dev_usbfs)?;
    }

    let (shm_fs, shm_handle) = tmp::MemoryFs::new_with_handle();
    mount_at(&fs, "/dev/shm", shm_fs)?;
    SHM_TMPFS.init_once(shm_handle);

    let (tmp_fs, tmp_handle) = tmp::MemoryFs::new_with_handle();
    mount_at(&fs, "/tmp", tmp_fs)?;
    TMP_TMPFS.init_once(tmp_handle);

    mount_at(&fs, "/proc", proc::new_procfs())?;

    mount_at(&fs, "/sys", sysfs::new_sysfs())?;
    if usbfs::has_manager() {
        mount_at(&fs, "/sys/bus/usb", usbfs::new_bus_usb_sysfs())?;
    }

    mount_at(&fs, "/sys/kernel/debug", debug::new_debugfs())?;

    drop(fs);

    #[cfg(feature = "dev-log")]
    dev::bind_dev_log().expect("Failed to bind /dev/log");

    Ok(())
}
