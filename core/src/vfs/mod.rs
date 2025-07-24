//! Virtual filesystems

pub mod dev;
mod proc;
mod simple;
mod tmp;

use alloc::sync::Arc;

use axerrno::LinuxResult;
use axfs_ng::FS_CONTEXT;
use axfs_ng_vfs::{Filesystem, NodePermission};
use axsync::RawMutex;
pub use simple::{Device, DeviceOps, DirMapping, SimpleFs};
pub use tmp::MemoryFs;

fn mount_at(path: &str, mount_fs: Filesystem<RawMutex>) -> LinuxResult<()> {
    let fs = FS_CONTEXT.lock();
    if fs.resolve(path).is_err() {
        fs.create_dir(path, NodePermission::from_bits_truncate(0o755))?;
    }
    fs.resolve(path)?.mount(&mount_fs)?;
    info!("Mounted {} at {}", mount_fs.name(), path);
    Ok(())
}

/// Mount all filesystems
pub fn mount_all(
    extra_dev: impl FnOnce(&Arc<SimpleFs>, &mut DirMapping<RawMutex>),
) -> LinuxResult<()> {
    mount_at("/dev", dev::new_devfs(extra_dev)?)?;
    mount_at("/tmp", tmp::MemoryFs::new())?;
    mount_at("/proc", proc::new_procfs())?;
    Ok(())
}
