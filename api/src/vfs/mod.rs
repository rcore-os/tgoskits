//! Virtual filesystems

pub mod dev;
mod proc;
mod tmp;

use axerrno::LinuxResult;
use axfs::{FS_CONTEXT, FsContext};
use axfs_ng_vfs::{
    Filesystem, NodePermission,
    path::{Path, PathBuf},
};
pub use starry_core::vfs::{Device, DeviceOps, DirMapping, SimpleFs};
pub use tmp::MemoryFs;

const DIR_PERMISSION: NodePermission = NodePermission::from_bits_truncate(0o755);

fn mount_at(fs: &FsContext, path: &str, mount_fs: Filesystem) -> LinuxResult<()> {
    if fs.resolve(path).is_err() {
        fs.create_dir(path, DIR_PERMISSION)?;
    }
    fs.resolve(path)?.mount(&mount_fs)?;
    info!("Mounted {} at {}", mount_fs.name(), path);
    Ok(())
}

/// Mount all filesystems
pub fn mount_all() -> LinuxResult<()> {
    let fs = FS_CONTEXT.lock();
    mount_at(&fs, "/dev", dev::new_devfs())?;
    mount_at(&fs, "/dev/shm", tmp::MemoryFs::new())?;
    mount_at(&fs, "/tmp", tmp::MemoryFs::new())?;
    mount_at(&fs, "/proc", proc::new_procfs())?;

    mount_at(&fs, "/sys", tmp::MemoryFs::new())?;
    let mut path = PathBuf::new();
    for comp in Path::new("/sys/class/graphics/fb0/device").components() {
        path.push(comp.as_str());
        if fs.resolve(&path).is_err() {
            fs.create_dir(&path, DIR_PERMISSION)?;
        }
    }
    path.push("subsystem");
    fs.symlink("whatever", &path)?;
    drop(fs);

    #[cfg(feature = "dev-log")]
    dev::bind_dev_log().expect("Failed to bind /dev/log");

    Ok(())
}
