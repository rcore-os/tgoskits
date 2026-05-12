use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, OpenOptions};
use linux_raw_sys::general::{MFD_CLOEXEC, O_RDWR};

use crate::{
    file::{File, FileLike},
    mm::UserConstPtr,
    pseudofs,
};

// Linux: only MFD_CLOEXEC and MFD_ALLOW_SEALING are defined today.
const MFD_ALLOW_SEALING: u32 = 2;
const MFD_KNOWN_FLAGS: u32 = MFD_CLOEXEC | MFD_ALLOW_SEALING;

/// Linux `memfd_create(2)`: name length limit excluding the terminating NUL.
const MFD_NAME_MAX_LEN: usize = 249;

// TODO: correct memfd implementation (anonymous inode, sealing, etc.)

pub fn sys_memfd_create(name: UserConstPtr<core::ffi::c_char>, flags: u32) -> AxResult<isize> {
    if flags & !MFD_KNOWN_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }
    if name.is_null() {
        return Err(AxError::BadAddress);
    }
    let name_str = name.get_as_str()?;
    if name_str.len() > MFD_NAME_MAX_LEN {
        return Err(AxError::InvalidInput);
    }

    // Create an unlinked tmpfs inode directly, without any pathname.
    // This mirrors Linux's shmem-backed memfd: inode exists and is kept alive
    // only by the returned fd.
    let (mount_path, tmpfs) = if fs_has_dir("/dev/shm") {
        ("/dev/shm", pseudofs::shm_tmpfs())
    } else {
        ("/tmp", pseudofs::tmp_tmpfs())
    };
    let Some(tmpfs) = tmpfs else {
        // Pseudofs should have mounted tmpfs already; treat missing handle as
        // a hard failure rather than falling back to path-visible files.
        return Err(AxError::NotFound);
    };

    let fs = FS_CONTEXT.lock();
    let shm_loc = fs.resolve(mount_path)?;
    let mountpoint = shm_loc.mountpoint().clone();

    let entry = tmpfs.create_anonymous_file(
        name_str,
        axfs_ng_vfs::NodePermission::from_bits_truncate(0o666),
    );
    let loc = axfs_ng_vfs::Location::new(mountpoint, entry);

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open_loc(loc)?
        .into_file()?;

    let cloexec = flags & MFD_CLOEXEC != 0;
    File::new(file, O_RDWR)
        .add_to_fd_table(cloexec)
        .map(|fd| fd as _)
}

fn fs_has_dir(path: &str) -> bool {
    FS_CONTEXT.lock().resolve(path).is_ok()
}
