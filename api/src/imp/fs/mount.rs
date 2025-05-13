use core::ffi::{c_char, c_void};

use alloc::vec::Vec;
use axerrno::{LinuxError, LinuxResult};
use axsync::Mutex;
use linux_raw_sys::general::AT_FDCWD;

use crate::{
    path::{FilePath, handle_file_path},
    ptr::UserConstPtr,
};

pub fn sys_mount(
    source: UserConstPtr<c_char>,
    target: UserConstPtr<c_char>,
    fs_type: UserConstPtr<c_char>,
    flags: i32,
    _data: UserConstPtr<c_void>,
) -> LinuxResult<isize> {
    let source = source.get_as_str()?;
    let target = target.get_as_str()?;
    let fs_type = fs_type.get_as_str()?;
    info!(
        "sys_mount <= source: {}, target: {}, fs_type: {}, flags: {}",
        source, target, fs_type, flags
    );

    let device_path = handle_file_path(AT_FDCWD, source)?;
    let mount_path = handle_file_path(AT_FDCWD, target)?;
    info!(
        "mount {:?} to {:?} with fs_type={:?}",
        device_path, mount_path, fs_type
    );

    if fs_type != "vfat" {
        debug!("fs_type can only be vfat.");
        return Err(LinuxError::EPERM);
    }

    if !mount_path.exists() {
        debug!("mount path not exist");
        return Err(LinuxError::EPERM);
    }

    if check_mounted(&mount_path) {
        debug!("mount path includes mounted fs");
        return Err(LinuxError::EPERM);
    }

    if !mount_fat_fs(&device_path, &mount_path) {
        debug!("mount error");
        return Err(LinuxError::EPERM);
    }
    Ok(0)
}

pub fn sys_umount2(target: UserConstPtr<c_char>, flags: i32) -> LinuxResult<isize> {
    let target = target.get_as_str()?;
    info!("sys_umount2 <= target: {}, flags: {}", target, flags);

    let mount_path = handle_file_path(AT_FDCWD, target)?;
    if flags != 0 {
        debug!("flags unimplemented");
        return Err(LinuxError::EPERM);
    }

    if !mount_path.exists() {
        debug!("mount path not exist");
        return Err(LinuxError::EPERM);
    }

    if !umount_fat_fs(&mount_path) {
        debug!("umount error");
        return Err(LinuxError::EPERM);
    }
    Ok(0)
}

/// Mounted File System
/// "Mount" means read&write a file as a file system now
struct MountedFs {
    //pub inner: Arc<Mutex<FATFileSystem>>,
    pub device: FilePath,
    pub mnt_dir: FilePath,
}

impl MountedFs {
    pub fn new(device: &FilePath, mnt_dir: &FilePath) -> Self {
        Self {
            device: device.clone(),
            mnt_dir: mnt_dir.clone(),
        }
    }

    #[allow(unused)]
    pub fn device(&self) -> FilePath {
        self.device.clone()
    }

    pub fn mnt_dir(&self) -> FilePath {
        self.mnt_dir.clone()
    }
}

/// List of mounted file system
/// Note that the startup file system is not in the vec, but in mod.rs
static MOUNTED: Mutex<Vec<MountedFs>> = Mutex::new(Vec::new());

/// Mount a fatfs device
pub fn mount_fat_fs(device_path: &FilePath, mount_path: &FilePath) -> bool {
    // device_path needs symlink lookup, but mount_path does not
    // only opened files will be added to the symlink table for now, so do not convert now
    // debug!("mounting {} to {}", device_path.path(), mount_path.path());
    // if let Some(true_device_path) = real_path(device_path) {
    if mount_path.exists() {
        MOUNTED.lock().push(MountedFs::new(device_path, mount_path));
        info!(
            "mounted {} to {}",
            device_path.as_str(),
            mount_path.as_str()
        );
        return true;
    }
    info!(
        "mount failed: {} to {}",
        device_path.as_str(),
        mount_path.as_str()
    );
    false
}

/// unmount a fatfs device
pub fn umount_fat_fs(mount_path: &FilePath) -> bool {
    let mut mounted = MOUNTED.lock();
    let length_before_deletion = mounted.len();
    mounted.retain(|m| m.mnt_dir() != *mount_path);
    length_before_deletion > mounted.len()
}

/// check if a path is mounted
pub fn check_mounted(path: &FilePath) -> bool {
    let mounted = MOUNTED.lock();
    mounted.iter().any(|m| path.starts_with(&m.mnt_dir()))
}
