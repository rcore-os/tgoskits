//! Virtual filesystems

use axerrno::LinuxResult;
use axfs_ng::FS_CONTEXT;
use axfs_ng_vfs::{Filesystem, NodePermission};
use axsync::RawMutex;

mod tmp;

fn mount_at(path: &str, mount_fs: Filesystem<RawMutex>) -> LinuxResult<()> {
    let fs = FS_CONTEXT.lock();
    fs.create_dir(path, NodePermission::from_bits_truncate(0o755))?;
    fs.resolve(path)?.mount(&mount_fs)?;
    Ok(())
}

/// Mount all filesystems
pub fn mount_all() -> LinuxResult<()> {
    mount_at("/tmp", tmp::MemoryFs::new())?;
    Ok(())
}
