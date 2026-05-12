use alloc::sync::Arc;
use core::sync::atomic::AtomicU32;

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, FileFlags, OpenOptions};
use ax_io::{Seek, SeekFrom};
use linux_raw_sys::general::{F_SEAL_GROW, F_SEAL_SHRINK, F_SEAL_WRITE, MFD_CLOEXEC, O_RDWR};

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

#[derive(Debug)]
pub struct MemFdMeta {
    pub allow_sealing: bool,
    pub seals: AtomicU32,
}

pub fn memfd_seals_for_file_like(file_like: &Arc<dyn FileLike>) -> Option<u32> {
    let file = file_like.downcast_ref::<File>()?;
    let loc = file.inner().backend().ok()?.location();
    let meta = loc.user_data().get::<MemFdMeta>()?;
    Some(meta.seals.load(core::sync::atomic::Ordering::Relaxed))
}

pub fn memfd_check_write_seal(file_like: &Arc<dyn FileLike>) -> AxResult<()> {
    let Some(seals) = memfd_seals_for_file_like(file_like) else {
        return Ok(());
    };
    if seals & F_SEAL_WRITE != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

pub fn memfd_check_resize_seals(
    file_like: &Arc<dyn FileLike>,
    old_len: u64,
    new_len: u64,
) -> AxResult<()> {
    let Some(seals) = memfd_seals_for_file_like(file_like) else {
        return Ok(());
    };
    if new_len > old_len && (seals & F_SEAL_GROW != 0) {
        return Err(AxError::OperationNotPermitted);
    }
    if new_len < old_len && (seals & F_SEAL_SHRINK != 0) {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

pub fn memfd_check_implicit_grow_seal(
    file_like: &Arc<dyn FileLike>,
    old_len: u64,
    end_offset: u64,
) -> AxResult<()> {
    let Some(seals) = memfd_seals_for_file_like(file_like) else {
        return Ok(());
    };
    if end_offset > old_len && (seals & F_SEAL_GROW != 0) {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

/// Memfd seal checks before a seekable write at the file's current offset (`write` / `writev`).
pub fn memfd_checks_before_stream_write(
    file_like: &Arc<dyn FileLike>,
    write_len: u64,
) -> AxResult<()> {
    memfd_check_write_seal(file_like)?;
    if write_len == 0 {
        return Ok(());
    }
    let Some(file) = file_like.downcast_ref::<File>() else {
        return Ok(());
    };
    let old_len = file.inner().location().len()?;
    let pos = if file.inner().access(FileFlags::APPEND).is_ok() {
        old_len
    } else {
        file.inner().seek(SeekFrom::Current(0))?
    };
    let end = pos.saturating_add(write_len);
    memfd_check_implicit_grow_seal(file_like, old_len, end)?;
    Ok(())
}

/// Memfd seal checks before `pwrite` / `write_at` at a fixed byte offset.
pub fn memfd_checks_before_write_at(
    file_like: &Arc<dyn FileLike>,
    offset: u64,
    write_len: u64,
) -> AxResult<()> {
    memfd_check_write_seal(file_like)?;
    if write_len == 0 {
        return Ok(());
    }
    let Some(file) = file_like.downcast_ref::<File>() else {
        return Ok(());
    };
    let old_len = file.inner().location().len()?;
    let end = offset.saturating_add(write_len);
    memfd_check_implicit_grow_seal(file_like, old_len, end)?;
    Ok(())
}

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
    loc.user_data().insert(MemFdMeta {
        allow_sealing: flags & MFD_ALLOW_SEALING != 0,
        seals: AtomicU32::new(0),
    });

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
