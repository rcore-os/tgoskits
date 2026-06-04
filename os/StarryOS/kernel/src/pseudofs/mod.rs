//! Basic virtual filesystem support

pub(crate) mod cgroup;
pub mod debug;
pub mod dev;
mod device;
mod dir;
mod dyn_debug;
mod file;
mod fs;
pub(crate) mod proc;
mod sysfs;
mod tmp;
pub(crate) mod usbfs;

use alloc::{boxed::Box, sync::Arc};

use ax_errno::LinuxResult;
use ax_fs::{FS_CONTEXT, FsContext};
use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{DirNodeOps, FileNodeOps, Filesystem, NodePermission, WeakDirEntry};
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
    if fs.resolve(path).is_err() {
        fs.create_dir(path, DIR_PERMISSION, 0, 0)?;
    }
    fs.resolve(path)?.mount(&mount_fs)?;
    info!("Mounted {} at {}", mount_fs.name(), path);
    Ok(())
}

/// Mount all filesystems
pub fn mount_all() -> LinuxResult<()> {
    info!("Initialize pseudofs...");

    let fs = FS_CONTEXT.lock();
    mount_at(&fs, "/dev", dev::new_devfs())?;
    let usbfs = usbfs::new_usbfs()?;
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
