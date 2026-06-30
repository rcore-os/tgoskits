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
use ax_fs_ng::vfs::{FS_CONTEXT, FsContext};
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
    // Standard mount points (/dev, /proc, /sys, /tmp) already exist in the
    // rootfs image, so we mount straight over them. `loc.mount()` only touches
    // the in-memory mount table, never the backing filesystem.
    if let Ok(loc) = fs.resolve(path) {
        loc.mount(&mount_fs)?;
        info!("Mounted {} at {}", mount_fs.name(), path);
        return Ok(());
    }

    // Mount point is missing (e.g. the non-standard /cgroup). On a writable
    // rootfs we create it normally. On a rootfs forced read-only (dirty/corrupt
    // ext4 mounted without journal replay, common on physical boards after a
    // crash) create_dir fails with EROFS. Mirror the kernel auto-mount recovery
    // used for /boot and /userdata (see axfs-ng root.rs::ensure_mountpoint_dir_result):
    // fall back to a transient in-memory mount-point directory so a missing
    // optional pseudofs mount point never aborts boot. Writable rootfs behaviour
    // is unchanged.
    //
    // The transient helper only accepts a single-component name. /cgroup is the
    // only mount point that can reach this path: every other mount point either
    // exists in the rootfs image or has a parent provided by an already-mounted
    // in-memory fs (devfs/sysfs), so it resolves successfully above. A multi-
    // component path here would fail verify_entry_name and surface its error
    // rather than panic — a safe degradation, not a regression.
    let loc = match fs.create_dir(path, DIR_PERMISSION, 0, 0) {
        Ok(loc) => loc,
        Err(e) if e.canonicalize() == VfsError::ReadOnlyFilesystem => {
            let name = path.strip_prefix('/').unwrap_or(path);
            warn!("  read-only rootfs: using transient in-memory mount point for {path}");
            fs.root_dir()
                .create_transient_mount_dir(name, DIR_PERMISSION, 0, 0)?
        }
        Err(e) => return Err(e.into()),
    };
    loc.mount(&mount_fs)?;
    info!("Mounted {} at {}", mount_fs.name(), path);
    Ok(())
}

/// Mount all filesystems
pub fn mount_all() -> LinuxResult<()> {
    info!("Initialize pseudofs...");

    crate::cgroup::init();

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

    mount_at(&fs, "/cgroup", cgroup::new_cgroup2fs())?;
    if usbfs::has_manager() {
        mount_at(&fs, "/sys/bus/usb", usbfs::new_bus_usb_sysfs())?;
    }

    mount_at(&fs, "/sys/kernel/debug", debug::new_debugfs())?;

    drop(fs);

    #[cfg(feature = "dev-log")]
    dev::bind_dev_log().expect("Failed to bind /dev/log");

    Ok(())
}
