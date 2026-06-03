use alloc::{string::String, sync::Arc};
use core::ffi::c_char;

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, OpenOptions};
use ax_task::current;
use linux_raw_sys::general::{MFD_CLOEXEC, O_RDWR};

pub(crate) use crate::file::memfd::{
    check_write_seal_for_shared_file_backend as memfd_check_write_seal_for_shared_file_backend,
    collect_metas_touching_mprotect_range as memfd_collect_metas_touching_mprotect_range,
    on_after_map as memfd_on_after_map,
    on_aspace_replace_metadata as memfd_on_aspace_replace_metadata,
    on_aspace_unmap_range as memfd_on_aspace_unmap_range,
    release_all_shared_writable_counts_for_aspace as memfd_release_all_shared_writable_counts_for_aspace,
    resync_shared_writable_counts_after_mprotect as memfd_resync_shared_writable_counts_after_mprotect,
};
use crate::{
    file::{
        File, FileLike, add_file_like,
        memfd::{Memfd, MemfdRef},
    },
    mm::vm_load_string,
    pseudofs,
    task::AsThread,
};

/// `MFD_ALLOW_SEALING` — bit 1. `linux-raw-sys` does not export it on every
/// target, so define locally.
const MFD_ALLOW_SEALING: u32 = 0x0002;

/// `MFD_HUGETLB` — bit 2. We do not back memfds with hugepages yet, so reject it.
const MFD_HUGETLB: u32 = 0x0004;
/// `MFD_NOEXEC_SEAL` — Linux 6.3+. Forces the W^X policy on the memfd.
/// We don't enforce executable mappings, so accepting and ignoring is
/// equivalent in our security model.
const MFD_NOEXEC_SEAL: u32 = 0x0008;
/// `MFD_EXEC` — opt-out from `MFD_NOEXEC_SEAL`, also Linux 6.3+. Same
/// reasoning: accepted and ignored.
const MFD_EXEC: u32 = 0x0010;

/// Linux enforces `NAME_MAX - strlen("memfd:")` = 249 bytes for the name.
const MEMFD_NAME_MAX: usize = 249;

pub fn sys_memfd_create(name: *const c_char, flags: u32) -> AxResult<isize> {
    let valid_flags = MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_NOEXEC_SEAL | MFD_EXEC;
    if flags & !valid_flags != 0 || flags & MFD_HUGETLB != 0 {
        return Err(AxError::InvalidInput);
    }

    let cloexec = flags & MFD_CLOEXEC != 0;
    let allow_sealing = flags & MFD_ALLOW_SEALING != 0;

    // Load the name argument. Linux rejects overlong names.
    let name_str: String = vm_load_string(name)?;
    if name_str.len() > MEMFD_NAME_MAX {
        return Err(AxError::InvalidInput);
    }

    let (mount_path, tmpfs) = if fs_has_dir("/dev/shm") {
        ("/dev/shm", pseudofs::shm_tmpfs())
    } else {
        ("/tmp", pseudofs::tmp_tmpfs())
    };
    let tmpfs = tmpfs.ok_or(AxError::NotFound)?;

    let fs = FS_CONTEXT.lock();
    let mountpoint = fs.resolve(mount_path)?.mountpoint().clone();
    let cred = current().as_thread().cred();
    let entry = tmpfs.create_anonymous_file(
        &name_str,
        axfs_ng_vfs::NodePermission::from_bits_truncate(0o666),
        cred.fsuid,
        cred.fsgid,
    );
    let loc = axfs_ng_vfs::Location::new(mountpoint, entry);

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open_loc(loc.clone())?
        .into_file()?;

    let inner = Arc::new(File::new(file, O_RDWR));
    let memfd = Memfd::new(inner, name_str, allow_sealing);
    loc.user_data().insert(MemfdRef(memfd.clone()));
    add_file_like(memfd, cloexec).map(|fd| fd as _)
}

fn fs_has_dir(path: &str) -> bool {
    FS_CONTEXT.lock().resolve(path).is_ok()
}

fn memfd_from_file_like(file_like: &Arc<dyn FileLike>) -> Option<Arc<Memfd>> {
    if let Ok(memfd) = file_like.clone().downcast_arc::<Memfd>() {
        return Some(memfd);
    }
    let file = file_like.downcast_ref::<File>()?;
    file.inner()
        .backend()
        .ok()?
        .location()
        .user_data()
        .get::<MemfdRef>()
        .map(|memfd| memfd.0.clone())
}

pub fn memfd_check_write_seal(file_like: &Arc<dyn FileLike>) -> AxResult<()> {
    let Some(memfd) = memfd_from_file_like(file_like) else {
        return Ok(());
    };
    memfd.check_write_seal()
}

pub fn memfd_check_resize_seals(
    file_like: &Arc<dyn FileLike>,
    old_len: u64,
    new_len: u64,
) -> AxResult<()> {
    let Some(memfd) = memfd_from_file_like(file_like) else {
        return Ok(());
    };
    let seals = memfd.get_seals();
    if new_len < old_len && seals & crate::file::memfd::F_SEAL_SHRINK != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    if new_len > old_len && seals & crate::file::memfd::F_SEAL_GROW != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

pub fn memfd_checks_before_stream_write(
    file_like: &Arc<dyn FileLike>,
    write_len: u64,
) -> AxResult<()> {
    if write_len == 0 {
        return Ok(());
    }
    memfd_check_write_seal(file_like)
}

pub fn memfd_checks_before_write_at(
    file_like: &Arc<dyn FileLike>,
    _offset: u64,
    write_len: u64,
) -> AxResult<()> {
    if write_len == 0 {
        return Ok(());
    }
    memfd_check_write_seal(file_like)
}
