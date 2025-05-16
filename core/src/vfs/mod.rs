//! Virtual filesystems

mod dynamic;
mod file;
mod proc;
mod tmp;

use axerrno::LinuxResult;
use axfs_ng::FS_CONTEXT;
use axfs_ng_vfs::{Filesystem, NodePermission};
use axsync::RawMutex;

fn mount_at(path: &str, mount_fs: Filesystem<RawMutex>) -> LinuxResult<()> {
    let fs = FS_CONTEXT.lock();
    fs.create_dir(path, NodePermission::from_bits_truncate(0o755))?;
    fs.resolve(path)?.mount(&mount_fs)?;
    info!("Mounted {} at {}", mount_fs.name(), path);
    Ok(())
}

/// Mount all filesystems
pub fn mount_all() -> LinuxResult<()> {
    mount_at("/tmp", tmp::MemoryFs::new())?;
    mount_at("/proc", proc::new_procfs())?;
    Ok(())
}
