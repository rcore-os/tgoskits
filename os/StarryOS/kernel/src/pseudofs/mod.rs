//! Basic virtual filesystem support

pub(crate) mod cgroup;
pub mod debug;
pub mod dev;
mod device;
mod dir;
mod dyn_debug;
mod file;
mod fs;
mod mqueue;
pub(crate) mod overlay;
pub(crate) mod proc;
mod proc_mountinfo;
mod sysfs;
pub(crate) mod usbfs;

use alloc::{boxed::Box, sync::Arc};

use ax_fs_ng::vfs::{FsContext, current_fs_context};
use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{
    DirNodeOps, FileNodeOps, Filesystem, MutationCredentials, NodePermission, WeakDirEntry,
};
pub use ax_fs_ng::MemoryFs;

pub use self::{device::*, dir::*, file::*, fs::*};
use crate::StarryResult;

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

static SHM_TMPFS: LazyInit<Arc<MemoryFs>> = LazyInit::new();
static TMP_TMPFS: LazyInit<Arc<MemoryFs>> = LazyInit::new();

pub fn shm_tmpfs() -> Option<Arc<MemoryFs>> {
    SHM_TMPFS.get().map(Arc::clone)
}

pub fn tmp_tmpfs() -> Option<Arc<MemoryFs>> {
    TMP_TMPFS.get().map(Arc::clone)
}

/// Bytes the global allocator has handed out, which `/proc/meminfo` and the
/// per-node meminfo both subtract from total RAM so the two views agree.
fn allocator_used_bytes(usages: &ax_alloc::Usages) -> usize {
    usages.get(ax_alloc::UsageKind::RustHeap)
        + usages.get(ax_alloc::UsageKind::VirtMem)
        + usages.get(ax_alloc::UsageKind::PageCache)
        + usages.get(ax_alloc::UsageKind::PageTable)
        + usages.get(ax_alloc::UsageKind::TaskStack)
        + usages.get(ax_alloc::UsageKind::Dma)
        + usages.get(ax_alloc::UsageKind::Global)
}

fn mount_at(fs: &FsContext, path: &str, mount_fs: Filesystem) -> StarryResult<()> {
    let initial_resolve = fs.resolve(path);
    if initial_resolve.is_err() {
        fs.create_dir(path, DIR_PERMISSION, 0, 0, &MutationCredentials::root())?;
    }
    let loc = fs.resolve(path)?;
    loc.mount_with_source(&mount_fs, mount_fs.name())?;
    info!("Mounted {} at {}", mount_fs.name(), path);
    Ok(())
}

/// Mount all filesystems
pub fn mount_all() -> StarryResult<()> {
    info!("Initialize pseudofs...");

    let fs_context = current_fs_context();
    let fs = fs_context.lock();
    let usbfs = usbfs::new_usbfs()?;
    let root_mount_device = fs.root_dir().mountpoint().device();
    mount_at(&fs, "/dev", dev::new_devfs(root_mount_device))?;
    if let Some(dev_usbfs) = usbfs {
        mount_at(&fs, "/dev/bus/usb", dev_usbfs)?;
    }

    let (shm_fs, shm_handle) = MemoryFs::new_with_handle();
    mount_at(&fs, "/dev/shm", shm_fs)?;
    SHM_TMPFS.init_once(shm_handle);

    let (tmp_fs, tmp_handle) = MemoryFs::new_with_handle();
    mount_at(&fs, "/tmp", tmp_fs)?;
    TMP_TMPFS.init_once(tmp_handle);

    mount_at(&fs, "/dev/mqueue", mqueue::new_mqueuefs())?;

    mount_at(
        &fs,
        "/proc",
        proc::new_procfs(crate::task::ROOT_PID_NS.clone()),
    )?;

    // Each CPU samples its own cache registers here, before `/sys` exists, so
    // `cpuN/cache` always serves `cpuN`'s snapshot whichever CPU reads it.
    sysfs::init_cpu_cache();
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
