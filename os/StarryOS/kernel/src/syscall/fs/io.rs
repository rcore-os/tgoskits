use alloc::{borrow::Cow, sync::Arc, vec, vec::Vec};
use core::{
    ffi::{c_char, c_int},
    task::Context,
};

use ax_fs_ng::vfs::{FileFlags, OpenOptions};
use ax_io::{IoBuf, Read, Seek, SeekFrom};
use ax_task::current;
use axfs_ng_vfs::{
    DirectoryCursor, FileRangeOperation, NodePermission, NodeType, PreallocationMode,
};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{
    __kernel_off_t, FALLOC_FL_COLLAPSE_RANGE, FALLOC_FL_INSERT_RANGE, FALLOC_FL_KEEP_SIZE,
    FALLOC_FL_PUNCH_HOLE, FALLOC_FL_ZERO_RANGE, O_APPEND,
};
use starry_vm::{VmMutPtr, VmPtr, vm_read_slice};
use syscalls::Sysno;

use super::memfd::{
    memfd_check_resize_seals, memfd_check_write_seal, memfd_checks_before_stream_write,
    memfd_checks_before_write_at,
};
use crate::{
    Errno, StarryError, StarryResult,
    file::{
        Directory, File, FileLike, Pipe, get_file_like,
        memfd::{F_SEAL_ANY_WRITE, F_SEAL_GROW, Memfd},
    },
    ipc::mqueue::MqDescriptor,
    mm::{IoVec, IoVectorBuf, VmBytesMut, check_access, prepare_user_read, vm_load_path_string},
    task::AsThread,
};

/// Get a [`File`] from fd, converting type-mismatch errors to ESPIPE.
/// Use this for syscalls that require a regular file fd and should return
/// ESPIPE for pipes/sockets (lseek, pread, pwrite, fallocate, etc.).
fn file_or_espipe(fd: c_int) -> StarryResult<Arc<File>> {
    File::from_fd(fd).map_err(|e| {
        if matches!(
            e,
            StarryError::IsADirectory | StarryError::BadFileDescriptor
        ) {
            e
        } else {
            StarryError::from(Errno::ESPIPE)
        }
    })
}

/// Like `file_or_espipe`, but for write operations: converts IsADirectory
/// to BadFileDescriptor because directories cannot be opened for writing.
/// and verifies that the file descriptor is writable.
fn file_or_espipe_write(fd: c_int) -> StarryResult<Arc<File>> {
    let f = file_or_espipe(fd).map_err(|e| {
        if matches!(e, StarryError::IsADirectory) {
            StarryError::BadFileDescriptor
        } else {
            e
        }
    })?;
    let _ = f.inner().access(FileFlags::WRITE)?;
    Ok(f)
}

fn offset_from_hilo(pos_l: __kernel_off_t, _pos_h: usize) -> __kernel_off_t {
    #[cfg(target_pointer_width = "32")]
    {
        let offset = ((_pos_h as u64) << 32) | (pos_l as u32 as u64);
        offset as i64 as __kernel_off_t
    }

    #[cfg(target_pointer_width = "64")]
    {
        pos_l
    }
}

struct DummyFd;
impl FileLike for DummyFd {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[dummy]".into()
    }
}
impl Pollable for DummyFd {
    fn poll(&self) -> IoEvents {
        IoEvents::empty()
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

pub fn sys_dummy_fd(sysno: Sysno) -> StarryResult<isize> {
    if current().name().starts_with("qemu-") {
        // We need to be honest to qemu, since it can automatically fallback to
        // other strategies.
        return Err(StarryError::Unsupported);
    }
    warn!("Dummy fd created: {sysno}");
    DummyFd.add_to_fd_table(false).map(|fd| fd as isize)
}

/// Read data from the file indicated by `fd`.
///
/// Return the read size if success.
pub fn sys_read(fd: i32, buf: *mut u8, len: usize) -> StarryResult<isize> {
    debug!("sys_read <= fd: {fd}, buf: {buf:p}, len: {len}");
    Ok(get_file_like(fd)?.read(&mut VmBytesMut::new(buf, len))? as _)
}

pub fn sys_readv(fd: i32, iov: *const IoVec, iovcnt: usize) -> StarryResult<isize> {
    debug!("sys_readv <= fd: {fd}, iovcnt: {iovcnt}");
    let f = get_file_like(fd)?;
    f.read(&mut IoVectorBuf::new(iov, iovcnt)?.into_io())
        .map(|n| n as _)
}

/// Write data to the file indicated by `fd`.
///
/// Return the written size if success.
pub fn sys_write(fd: i32, buf: *mut u8, len: usize) -> StarryResult<isize> {
    debug!("sys_write <= fd: {fd}, buf: {buf:p}, len: {len}");
    let file_like = get_file_like(fd)?;
    file_like.validate_write_len(len)?;
    // Linux checks address geometry before entering the file write path,
    // then shmem write_begin checks seals before faulting the payload.
    check_access(buf as usize, len)?;
    memfd_checks_before_stream_write(&file_like, len as u64)?;
    let data = copy_user_read_buf(buf.cast_const(), len)?;
    Ok(file_like.write(&mut data.as_slice())? as _)
}

pub fn sys_writev(fd: i32, iov: *const IoVec, iovcnt: usize) -> StarryResult<isize> {
    debug!("sys_writev <= fd: {fd}, iovcnt: {iovcnt}");
    let file_like = get_file_like(fd)?;
    // Check length invariants (e.g. eventfd count) before importing segment
    // data, so a count error (EINVAL) takes precedence over a bad segment
    // pointer (EFAULT), matching Linux vfs_writev / eventfd_write ordering.
    let source = IoVectorBuf::new_with_len_validator(iov, iovcnt, |len| {
        file_like.validate_write_len(len)
    })?;
    memfd_checks_before_stream_write(&file_like, source.byte_len() as u64)?;
    let data = copy_user_iov_read_buf(source)?;
    file_like.write(&mut data.as_slice()).map(|n| n as _)
}

pub fn sys_lseek(fd: c_int, offset: __kernel_off_t, whence: c_int) -> StarryResult<isize> {
    debug!("sys_lseek <= {fd} {offset} {whence}");
    let pos = match whence {
        0 => {
            if offset < 0 {
                return Err(StarryError::InvalidInput);
            }
            SeekFrom::Start(offset as _)
        }
        1 => SeekFrom::Current(offset as _),
        2 => SeekFrom::End(offset as _),
        _ => return Err(StarryError::InvalidInput),
    };
    let any_file = get_file_like(fd)?;

    // File::from_fd transparently unwraps Memfd onto its backing File, so
    // memfd fds take this branch and get regular-file seek semantics
    // (lseek, pread/pwrite, fallocate). Without it memfd would fall
    // through to ESPIPE.
    if let Ok(f) = File::from_fd(fd) {
        let off = f.inner().seek(pos)?;
        return Ok(off as _);
    }

    if let Ok(mq) = any_file.clone().downcast_arc::<MqDescriptor>() {
        return Ok(mq.seek_status(pos)? as _);
    }

    if let Ok(d) = any_file.downcast_arc::<Directory>() {
        let mut position = d.position.lock();
        let new_pos = match pos {
            SeekFrom::Start(pos) => pos,
            SeekFrom::End(delta) => d
                .inner()
                .directory_end_cursor()?
                .offset()
                .checked_add_signed(delta)
                .ok_or(StarryError::InvalidInput)?,
            SeekFrom::Current(delta) => position
                .cursor
                .offset()
                .checked_add_signed(delta)
                .ok_or(StarryError::InvalidInput)?,
        };
        if new_pos > i64::MAX as u64 {
            return Err(StarryError::InvalidInput);
        }
        position.cursor = DirectoryCursor::new(new_pos);
        position.read_state = None;
        return Ok(new_pos as _);
    }

    Err(StarryError::from(Errno::ESPIPE))
}

pub fn sys_truncate(path: *const c_char, length: __kernel_off_t) -> StarryResult<isize> {
    let path = vm_load_path_string(path)?;
    debug!("sys_truncate <= {path:?} {length}");
    if path.is_empty() {
        return Err(StarryError::from(Errno::ENOENT));
    }
    if length < 0 {
        return Err(StarryError::InvalidInput);
    }
    let file = OpenOptions::new()
        .write(true)
        .open(&ax_fs_ng::vfs::current_fs_context().lock(), &path)?
        .into_file()?;
    if (length as u64) > u32::MAX as u64 * 4096 {
        return Err(StarryError::from(Errno::EFBIG));
    }
    // Check write permission against current credentials following the
    // same owner/group/other + root-bypass rules as faccessat2(2).
    let cred = current().as_thread().cred();
    if cred.fsuid != 0 {
        let metadata = file.location().metadata()?;
        let (file_uid, file_gid, file_mode) = (metadata.uid, metadata.gid, metadata.mode);
        let has_write = if cred.fsuid == file_uid {
            file_mode.contains(NodePermission::OWNER_WRITE)
        } else if cred.fsgid == file_gid || cred.groups.contains(&file_gid) {
            file_mode.contains(NodePermission::GROUP_WRITE)
        } else {
            file_mode.contains(NodePermission::OTHER_WRITE)
        };
        if !has_write {
            return Err(StarryError::from(Errno::EACCES));
        }
    }
    file.access(FileFlags::WRITE)?.set_len(length as _)?;
    Ok(0)
}

pub fn sys_ftruncate(fd: c_int, length: __kernel_off_t) -> StarryResult<isize> {
    debug!("sys_ftruncate <= {fd} {length}");
    if length < 0 {
        return Err(StarryError::InvalidInput);
    }
    if let Ok(memfd) = Memfd::from_fd(fd) {
        memfd.set_len_sealed(length as u64)?;
        return Ok(0);
    }
    let f = File::from_fd(fd).map_err(|e| {
        if matches!(e, StarryError::IsADirectory) {
            StarryError::from(Errno::EINVAL)
        } else {
            e
        }
    })?;
    if (length as u64) > u32::MAX as u64 * 4096 {
        return Err(StarryError::from(Errno::EFBIG));
    }
    f.inner().access(FileFlags::WRITE)?.set_len(length as _)?;
    Ok(0)
}

pub fn sys_fallocate(
    fd: c_int,
    mode: u32,
    offset: __kernel_off_t,
    len: __kernel_off_t,
) -> StarryResult<isize> {
    debug!("sys_fallocate <= fd: {fd}, mode: {mode}, offset: {offset}, len: {len}");
    // Linux resolves the descriptor before entering vfs_fallocate, but range
    // and mode checks precede write access and inode-type checks after that.
    let f_like = get_file_like(fd)?;
    if offset < 0 || len <= 0 {
        return Err(StarryError::InvalidInput);
    }
    let keep_size = mode & FALLOC_FL_KEEP_SIZE != 0;
    let operation = mode & !FALLOC_FL_KEEP_SIZE;
    let supported_mode = operation == 0
        || operation == FALLOC_FL_ZERO_RANGE
        || operation == FALLOC_FL_PUNCH_HOLE && keep_size
        || operation == FALLOC_FL_COLLAPSE_RANGE && !keep_size
        || operation == FALLOC_FL_INSERT_RANGE && !keep_size;
    if !supported_mode {
        return Err(StarryError::OperationNotSupported);
    }
    let f = file_or_espipe_write(fd)?;
    memfd_check_write_seal(&f_like)?;
    let end = (offset as u64)
        .checked_add(len as u64)
        .ok_or(StarryError::from(Errno::EFBIG))?;
    // Reject sizes beyond what ext4 can represent (u32 block numbers × 4 KiB blocks).
    if end > u32::MAX as u64 * 4096 {
        return Err(StarryError::from(Errno::EFBIG));
    }
    // For memfd fds, enforce the seal mask before changing the size.
    // `F_SEAL_WRITE`/`F_SEAL_FUTURE_WRITE` already forbid any data-mutating
    // path; `F_SEAL_GROW` additionally forbids a fallocate that would extend
    // EOF. Linux surfaces all as EPERM (shmem_fallocate checks the same
    // `F_SEAL_WRITE | F_SEAL_FUTURE_WRITE` mask; memfd_test.c covers this).
    if let Ok(memfd) = Memfd::from_fd(fd) {
        let seals = memfd.get_seals();
        if seals & F_SEAL_ANY_WRITE != 0 {
            return Err(StarryError::OperationNotPermitted);
        }
        let cur_len = f.inner().backend()?.location().len()?;
        if !keep_size && end > cur_len && seals & F_SEAL_GROW != 0 {
            return Err(StarryError::OperationNotPermitted);
        }
    }
    let inner = f.inner();
    let file = inner.access(FileFlags::WRITE)?;
    let old_len = file.location().len()?;
    let new_len = match operation {
        FALLOC_FL_COLLAPSE_RANGE => old_len
            .checked_sub(len as u64)
            .ok_or(StarryError::InvalidInput)?,
        FALLOC_FL_INSERT_RANGE => old_len
            .checked_add(len as u64)
            .ok_or(StarryError::from(Errno::EFBIG))?,
        _ if keep_size => old_len,
        _ => old_len.max(end),
    };
    if new_len > u32::MAX as u64 * 4096 {
        return Err(StarryError::from(Errno::EFBIG));
    }
    memfd_check_resize_seals(&f_like, old_len, new_len)?;

    match operation {
        0 => {
            if Memfd::from_fd(fd).is_ok() {
                if keep_size {
                    return Err(StarryError::OperationNotSupported);
                }
                if new_len != old_len {
                    file.set_len(new_len)?;
                }
            } else {
                let mode = if keep_size {
                    PreallocationMode::KeepSize
                } else {
                    PreallocationMode::ExtendSize
                };
                file.preallocate(offset as u64, len as u64, mode)?;
            }
        }
        FALLOC_FL_ZERO_RANGE => {
            let mode = if keep_size {
                PreallocationMode::KeepSize
            } else {
                PreallocationMode::ExtendSize
            };
            file.operate_range(
                offset as u64,
                len as u64,
                FileRangeOperation::ZeroRange(mode),
            )?;
        }
        FALLOC_FL_PUNCH_HOLE => {
            file.operate_range(offset as u64, len as u64, FileRangeOperation::PunchHole)?;
        }
        FALLOC_FL_COLLAPSE_RANGE => {
            file.operate_range(offset as u64, len as u64, FileRangeOperation::CollapseRange)?;
        }
        FALLOC_FL_INSERT_RANGE => {
            file.operate_range(offset as u64, len as u64, FileRangeOperation::InsertRange)?;
        }
        _ => unreachable!(),
    }
    Ok(0)
}

pub fn sys_fsync(fd: c_int) -> StarryResult<isize> {
    debug!("sys_fsync <= {fd}");
    let any_file = get_file_like(fd)?;
    if let Ok(memfd) = any_file.clone().downcast_arc::<Memfd>() {
        // Linux treats memfd as a regular file for fsync: a successful
        // no-op (the contents are already in memory). Forward to the
        // inner File so any future backing-store changes still hook
        // through one path.
        memfd.inner().inner().sync(false)?;
        return Ok(0);
    }
    if let Ok(f) = any_file.clone().downcast_arc::<File>() {
        f.inner().sync(false)?;
        return Ok(0);
    } else if let Ok(d) = any_file.downcast_arc::<Directory>() {
        d.inner().sync(false)?;
        return Ok(0);
    }
    Err(StarryError::from(Errno::EINVAL))
}

pub fn sys_fdatasync(fd: c_int) -> StarryResult<isize> {
    debug!("sys_fdatasync <= {fd}");
    let any_file = get_file_like(fd)?;
    if let Ok(memfd) = any_file.clone().downcast_arc::<Memfd>() {
        memfd.inner().inner().sync(true)?;
        return Ok(0);
    }
    if let Ok(f) = any_file.clone().downcast_arc::<File>() {
        f.inner().sync(true)?;
        return Ok(0);
    } else if let Ok(d) = any_file.downcast_arc::<Directory>() {
        d.inner().sync(true)?;
        return Ok(0);
    }
    Err(StarryError::from(Errno::EINVAL))
}

pub fn sys_sync_file_range(fd: c_int, offset: i64, nbytes: i64, flags: u32) -> StarryResult<isize> {
    debug!("sys_sync_file_range <= fd: {fd}, flags: {flags:#x}");
    const SYNC_FILE_RANGE_WAIT_BEFORE: u32 = 1;
    const SYNC_FILE_RANGE_WRITE: u32 = 2;
    const SYNC_FILE_RANGE_WAIT_AFTER: u32 = 4;
    const SYNC_FILE_RANGE_ALL: u32 =
        SYNC_FILE_RANGE_WAIT_BEFORE | SYNC_FILE_RANGE_WRITE | SYNC_FILE_RANGE_WAIT_AFTER;
    // Linux resolves the descriptor before validating range arguments and
    // flags, so an invalid descriptor takes precedence over EINVAL.
    let any = get_file_like(fd)?;
    if offset < 0 || nbytes < 0 {
        return Err(StarryError::from(Errno::EINVAL));
    }
    if (flags & !SYNC_FILE_RANGE_ALL) != 0 {
        return Err(StarryError::from(Errno::EINVAL));
    }
    // sync_file_range(2) is an advisory hint to initiate writeback for a
    // byte range. Until range-based writeback is implemented, keep this as
    // a no-op after basic fd validation rather than turning it into a
    // stronger whole-file fdatasync-style flush (matches the advisory
    // nature documented in the man page). Invalid fds still surface the
    // underlying error (EBADF). Directory fds are accepted to match fsync.
    if any.downcast_ref::<File>().is_none()
        && any.downcast_ref::<Directory>().is_none()
        && any.downcast_ref::<Memfd>().is_none()
    {
        return Err(StarryError::from(Errno::ESPIPE));
    }
    Ok(0)
}

pub fn sys_fadvise64(
    fd: c_int,
    offset: __kernel_off_t,
    len: __kernel_off_t,
    advice: u32,
) -> StarryResult<isize> {
    debug!("sys_fadvise64 <= fd: {fd}, offset: {offset}, len: {len}, advice: {advice}");
    // Validate fd first: invalid/closed → EBADF, non-file/non-dir → ESPIPE.
    // Linux fadvise64 accepts regular files, directories, and memfd fds
    // (advisory hint).
    let f = get_file_like(fd)?;
    if f.downcast_ref::<File>().is_none()
        && f.downcast_ref::<Directory>().is_none()
        && f.downcast_ref::<Memfd>().is_none()
    {
        return Err(StarryError::from(Errno::ESPIPE));
    }
    if len < 0 {
        return Err(StarryError::InvalidInput);
    }
    if advice > 5 {
        return Err(StarryError::InvalidInput);
    }
    Ok(0)
}

pub fn sys_pread64(
    fd: c_int,
    buf: *mut u8,
    len: usize,
    offset: __kernel_off_t,
) -> StarryResult<isize> {
    let f = file_or_espipe(fd)?;
    if offset < 0 {
        return Err(StarryError::InvalidInput);
    }
    let read = f.inner().read_at(VmBytesMut::new(buf, len), offset as _)?;
    Ok(read as _)
}

pub fn sys_pwrite64(
    fd: c_int,
    buf: *const u8,
    len: usize,
    offset: __kernel_off_t,
) -> StarryResult<isize> {
    if offset < 0 {
        return Err(StarryError::InvalidInput);
    }
    // Route memfd fds through the seal-aware `Memfd::write_at` so
    // `F_SEAL_WRITE`/`F_SEAL_GROW` apply to offset writes the same
    // as they do to seq writes; otherwise `pwrite64` would silently
    // bypass the seal by writing straight to the inner file.
    if let Ok(memfd) = Memfd::from_fd(fd) {
        if len == 0 {
            return Ok(0);
        }
        check_access(buf as usize, len)?;
        memfd.check_write_seal()?;
        let data = copy_user_read_buf(buf, len)?;
        let write = memfd.write_at(data.as_slice(), offset as u64)?;
        return Ok(write as _);
    }
    let f = file_or_espipe_write(fd)?;
    if len == 0 {
        // Linux: 0-byte pwrite is a no-op and must not fail with F_SEAL_WRITE.
        return Ok(0);
    }
    let file_like = get_file_like(fd)?;
    check_access(buf as usize, len)?;
    memfd_checks_before_write_at(&file_like, offset as u64, len as u64)?;
    let data = copy_user_read_buf(buf, len)?;
    let write = f.inner().write_at(data.as_slice(), offset as _)?;
    Ok(write as _)
}

pub fn sys_preadv(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    pos_l: __kernel_off_t,
    pos_h: usize,
) -> StarryResult<isize> {
    let offset = offset_from_hilo(pos_l, pos_h);
    // preadv (unlike preadv2) does not accept offset=-1; reject negative offsets.
    if offset < 0 {
        return Err(StarryError::InvalidInput);
    }
    sys_preadv2(fd, iov, iovcnt, offset, 0, 0)
}

pub fn sys_pwritev(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    pos_l: __kernel_off_t,
    pos_h: usize,
) -> StarryResult<isize> {
    let offset = offset_from_hilo(pos_l, pos_h);
    // pwritev (unlike pwritev2) does not accept offset=-1; reject negative offsets.
    if offset < 0 {
        return Err(StarryError::InvalidInput);
    }
    sys_pwritev2(fd, iov, iovcnt, offset, 0, 0)
}

/// Validate preadv2/pwritev2 flags.
/// Currently no RWF_* flags are supported; any non-zero value is rejected.
fn validate_rwf_flags(flags: u32) -> StarryResult<()> {
    if flags != 0 {
        return Err(StarryError::OperationNotSupported);
    }
    Ok(())
}

pub fn sys_preadv2(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    pos_l: __kernel_off_t,
    pos_h: usize,
    flags: u32,
) -> StarryResult<isize> {
    let offset = offset_from_hilo(pos_l, pos_h);
    debug!("sys_preadv2 <= fd: {fd}, iovcnt: {iovcnt}, offset: {offset}, flags: {flags}");
    validate_rwf_flags(flags)?;
    if offset < -1 {
        return Err(StarryError::InvalidInput);
    }
    let mut io_buf = IoVectorBuf::new(iov, iovcnt)?.into_io();
    if offset == -1 {
        // offset == -1: use current file position (like readv)
        let f = get_file_like(fd)?;
        f.read(&mut io_buf).map(|n| n as _)
    } else {
        let f = file_or_espipe(fd)?;
        Ok(f.inner().read_at(io_buf, offset as _).map(|n| n as _)?)
    }
}

pub fn sys_pwritev2(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    pos_l: __kernel_off_t,
    pos_h: usize,
    flags: u32,
) -> StarryResult<isize> {
    let offset = offset_from_hilo(pos_l, pos_h);
    debug!("sys_pwritev2 <= fd: {fd}, iovcnt: {iovcnt}, offset: {offset}, flags: {flags}");
    validate_rwf_flags(flags)?;
    if offset < -1 {
        return Err(StarryError::InvalidInput);
    }
    if offset == -1 {
        // offset == -1: use current file position (like writev)
        let file_like = get_file_like(fd)?;
        let source = IoVectorBuf::new_with_len_validator(iov, iovcnt, |len| {
            file_like.validate_write_len(len)
        })?;
        memfd_checks_before_stream_write(&file_like, source.byte_len() as u64)?;
        let data = copy_user_iov_read_buf(source)?;
        file_like.write(&mut data.as_slice()).map(|n| n as _)
    } else if let Ok(memfd) = Memfd::from_fd(fd) {
        // Route memfd offset writes through the seal-aware path.
        let source = IoVectorBuf::new(iov, iovcnt)?;
        if source.byte_len() != 0 {
            memfd.check_write_seal()?;
        }
        let data = copy_user_iov_read_buf(source)?;
        memfd
            .write_at(data.as_slice(), offset as u64)
            .map(|n| n as _)
    } else {
        let source = IoVectorBuf::new(iov, iovcnt)?;
        let f = file_or_espipe_write(fd)?;
        let file_like = get_file_like(fd)?;
        memfd_checks_before_write_at(&file_like, offset as u64, source.byte_len() as u64)?;
        let data = copy_user_iov_read_buf(source)?;
        Ok(f.inner()
            .write_at(data.as_slice(), offset as _)
            .map(|n| n as _)?)
    }
}

fn copy_user_read_buf(buf: *const u8, len: usize) -> StarryResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    // Preserve source validation before the potentially large allocation.
    // No user reference is retained: copy-in still revalidates each access.
    validate_user_read_buf(buf, len)?;
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(len)
        .map_err(|_| StarryError::NoMemory)?;
    vm_read_slice(buf, &mut copied.spare_capacity_mut()[..len])?;
    // SAFETY: successful VM copy initialized every requested byte; capacity
    // was reserved before borrowing the spare storage.
    unsafe { copied.set_len(len) };
    Ok(copied)
}

/// `access_ok`-style validation without copying payload (may surface `BadAddress` / EFAULT).
fn validate_user_read_buf(buf: *const u8, len: usize) -> StarryResult<()> {
    if len == 0 {
        return Ok(());
    }
    prepare_user_read(buf as usize, len)?;
    Ok(())
}

fn copy_user_iov_read_buf(source: IoVectorBuf) -> StarryResult<Vec<u8>> {
    source.prepare_read()?;
    let mut src = source.into_io();
    let len = src.remaining();
    let mut data = Vec::new();
    data.try_reserve_exact(len)
        .map_err(|_| StarryError::NoMemory)?;
    data.resize(len, 0);
    src.read_exact(&mut data)?;
    Ok(data)
}

enum SendFile {
    Direct(Arc<dyn FileLike>),
    /// `*mut u64` is the user offset pointer; the `u64` is the kernel file position (not written
    /// back to userspace until a corresponding destination write succeeds).
    Offset(Arc<File>, *mut u64, u64),
    /// Memfd output with an explicit offset. Routed through
    /// `Memfd::write_at` so `F_SEAL_WRITE` / `F_SEAL_GROW` are enforced
    /// the same way as `pwrite`; the plain `Offset` variant unwraps the
    /// memfd to its inner `File` and would silently bypass the seal
    /// check by calling `File::write_at` directly.
    OffsetMemfd(Arc<crate::file::memfd::Memfd>, *mut u64),
}

/// Build the `SendFile` for the *output* end of sendfile / copy_file_range /
/// splice with an explicit offset. When the fd points at a memfd, route
/// writes through the seal-aware [`crate::file::memfd::Memfd`] wrapper
/// instead of unwrapping it to its inner `File` (which would bypass
/// `F_SEAL_WRITE` and `F_SEAL_GROW`).
fn send_offset_out(fd: c_int, offset: *mut u64) -> StarryResult<SendFile> {
    let fl = get_file_like(fd)?;
    if let Ok(memfd) = fl.clone().downcast_arc::<crate::file::memfd::Memfd>() {
        return Ok(SendFile::OffsetMemfd(memfd, offset));
    }
    Ok(SendFile::Offset(
        File::from_fd(fd)?,
        offset,
        offset.vm_read()?,
    ))
}

impl SendFile {
    fn has_data(&self) -> bool {
        match self {
            SendFile::Direct(file) => file.poll(),
            SendFile::Offset(file, ..) => file.poll(),
            SendFile::OffsetMemfd(memfd, ..) => memfd.poll(),
        }
        .contains(IoEvents::IN)
    }

    fn read(&mut self, mut buf: &mut [u8]) -> StarryResult<usize> {
        match self {
            SendFile::Direct(file) => file.read(&mut buf),
            SendFile::Offset(file, _, pos) => Ok(file.inner().read_at(&mut buf, *pos)?),
            SendFile::OffsetMemfd(memfd, offset) => {
                let off = offset.vm_read()?;
                let bytes_read = memfd.inner().inner().read_at(&mut buf, off)?;
                offset.vm_write(off + bytes_read as u64)?;
                Ok(bytes_read)
            }
        }
    }

    fn write(&mut self, mut buf: &[u8]) -> StarryResult<usize> {
        match self {
            SendFile::Direct(file) => {
                super::memfd::memfd_checks_before_stream_write(file, buf.len() as u64)?;
                file.write(&mut buf)
            }
            SendFile::Offset(file, user, pos) => {
                let file_like: Arc<dyn FileLike> = file.clone();
                super::memfd::memfd_checks_before_write_at(&file_like, *pos, buf.len() as u64)?;
                let bytes_written = file.inner().write_at(buf, *pos)?;
                *pos += bytes_written as u64;
                user.vm_write(*pos)?;
                Ok(bytes_written)
            }
            SendFile::OffsetMemfd(memfd, offset) => {
                let off = offset.vm_read()?;
                let bytes_written = memfd.write_at(buf, off)?;
                offset.vm_write(off + bytes_written as u64)?;
                Ok(bytes_written)
            }
        }
    }
}

fn do_send(
    mut src: SendFile,
    mut dst: SendFile,
    len: usize,
    nonblock: bool,
) -> StarryResult<usize> {
    let mut buf = vec![0; 0x1000];
    let mut total_written = 0;
    let mut remaining = len;

    while remaining > 0 {
        if !src.has_data() {
            if total_written > 0 {
                break;
            }
            // splice(2) SPLICE_F_NONBLOCK: if the operation would block on the
            // source (e.g. an empty pipe) and nothing has been transferred yet,
            // fail with EAGAIN instead of blocking on the read below. Without
            // this flag the read blocks, matching Linux's default splice.
            if nonblock {
                return Err(StarryError::WouldBlock);
            }
        }
        let to_read = buf.len().min(remaining);
        let bytes_read = match src.read(&mut buf[..to_read]) {
            Ok(n) => n,
            Err(StarryError::WouldBlock) if total_written > 0 => break,
            Err(e) => return Err(e),
        };
        if bytes_read == 0 {
            break;
        }

        let bytes_written = match dst.write(&buf[..bytes_read]) {
            Ok(n) => n,
            // Socket send buffer full after partial progress: return what we
            // managed to transfer so far rather than propagating EAGAIN.
            // Linux sendfile(2) semantics: return the count of bytes written
            // when a short write happens, NOT -EAGAIN.
            Err(StarryError::WouldBlock) if total_written > 0 => break,
            Err(e) => return Err(e),
        };
        // Advance source offset by bytes actually transferred (partial dst.write
        // must not skip unread source data).
        if let SendFile::Offset(_, user, pos) = &mut src {
            *pos += bytes_written as u64;
            user.vm_write(*pos)?;
        }
        total_written += bytes_written;
        remaining -= bytes_written;

        if bytes_written < bytes_read {
            break;
        }
    }

    Ok(total_written)
}

pub fn sys_sendfile(
    out_fd: c_int,
    in_fd: c_int,
    offset: *mut u64,
    len: usize,
) -> StarryResult<isize> {
    debug!(
        "sys_sendfile <= out_fd: {}, in_fd: {}, offset: {}, len: {}",
        out_fd,
        in_fd,
        !offset.is_null(),
        len
    );
    let out_file = get_file_like(out_fd)?;
    if (out_file.open_flags() & O_APPEND) != 0 {
        return Err(StarryError::InvalidInput);
    }

    let src: SendFile = if !offset.is_null() {
        let pos = offset.vm_read()?;

        if pos > u32::MAX as u64 {
            return Err(StarryError::InvalidInput);
        }

        SendFile::Offset(File::from_fd(in_fd)?, offset, pos)
    } else {
        // 拒绝 pipe 输入：File::from_fd 对 pipe 会失败，但 get_file_like 会成功，后续 read 会返回 EPIPE。Linux sendfile 对 pipe 输入也是 EPIPE。
        let _in_file = File::from_fd(in_fd)?;
        SendFile::Direct(get_file_like(in_fd)?)
    };

    let dst: SendFile = SendFile::Direct(out_file);

    do_send(src, dst, len, false).map(|n: usize| n as _)
}

pub fn sys_copy_file_range(
    fd_in: c_int,
    off_in: *mut u64,
    fd_out: c_int,
    off_out: *mut u64,
    len: usize,
    flags: u32,
) -> StarryResult<isize> {
    debug!(
        "sys_copy_file_range <= fd_in: {}, off_in: {}, fd_out: {}, off_out: {}, len: {}, flags: {}",
        fd_in,
        !off_in.is_null(),
        fd_out,
        !off_out.is_null(),
        len,
        flags
    );

    if flags != 0 {
        return Err(StarryError::InvalidInput);
    }

    if len > isize::MAX as usize {
        return Err(StarryError::from(Errno::EOVERFLOW));
    }

    let remap = |e| match e {
        StarryError::BadFileDescriptor | StarryError::IsADirectory => e,
        _ => StarryError::InvalidInput,
    };

    let file_in = File::from_fd(fd_in).map_err(remap)?;
    let file_out = File::from_fd(fd_out).map_err(remap)?;

    let meta_in = file_in.inner().location().metadata()?;
    let meta_out = file_out.inner().location().metadata()?;

    if meta_in.node_type == NodeType::Directory || meta_out.node_type == NodeType::Directory {
        return Err(StarryError::IsADirectory);
    }

    if meta_in.node_type != NodeType::RegularFile || meta_out.node_type != NodeType::RegularFile {
        return Err(StarryError::InvalidInput);
    }

    if file_out.inner().access(FileFlags::APPEND).is_ok() {
        return Err(StarryError::BadFileDescriptor);
    }

    let pos_in = if off_in.is_null() {
        file_in.inner().seek(SeekFrom::Current(0))?
    } else {
        off_in.vm_read()?
    };

    let pos_out = if off_out.is_null() {
        file_out.inner().seek(SeekFrom::Current(0))?
    } else {
        off_out.vm_read()?
    };

    if len > 0 && meta_in.device == meta_out.device && meta_in.inode == meta_out.inode {
        let copy_last = (len as u64)
            .checked_sub(1)
            .ok_or(StarryError::InvalidInput)?;

        let in_end = pos_in
            .checked_add(copy_last)
            .ok_or(StarryError::InvalidInput)?;

        let out_end = pos_out
            .checked_add(copy_last)
            .ok_or(StarryError::InvalidInput)?;

        if in_end >= pos_out && pos_in <= out_end {
            return Err(StarryError::InvalidInput);
        }
    }

    let src: SendFile = if !off_in.is_null() {
        SendFile::Offset(file_in, off_in, pos_in)
    } else {
        SendFile::Direct(file_in)
    };

    let dst: SendFile = if !off_out.is_null() {
        send_offset_out(fd_out, off_out)?
    } else {
        SendFile::Direct(get_file_like(fd_out)?)
    };

    do_send(src, dst, len, false).map(|n: usize| n as isize)
}

pub fn sys_splice(
    fd_in: c_int,
    off_in: *mut i64,
    fd_out: c_int,
    off_out: *mut i64,
    len: usize,
    flags: u32,
) -> StarryResult<isize> {
    debug!(
        "sys_splice <= fd_in: {}, off_in: {}, fd_out: {}, off_out: {}, len: {}, flags: {}",
        fd_in,
        !off_in.is_null(),
        fd_out,
        !off_out.is_null(),
        len,
        flags
    );

    const SPLICE_F_MOVE: u32 = 0x01;
    const SPLICE_F_NONBLOCK: u32 = 0x02;
    const SPLICE_F_MORE: u32 = 0x04;
    const SPLICE_F_GIFT: u32 = 0x08;
    const SPLICE_F_ALL: u32 = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;

    // 1. 先检查明显非法 fd。
    if DummyFd::from_fd(fd_in).is_ok() || DummyFd::from_fd(fd_out).is_ok() {
        return Err(StarryError::BadFileDescriptor);
    }

    // 2. 检查 flags。未知 flag 返回 EINVAL。
    if flags & !SPLICE_F_ALL != 0 {
        return Err(StarryError::InvalidInput);
    }

    // 3. 防止最终 usize -> isize 溢出。
    if len > isize::MAX as usize {
        return Err(StarryError::InvalidInput);
    }

    // 4. 先识别 pipe。
    let in_pipe = Pipe::from_fd(fd_in).ok();
    let out_pipe = Pipe::from_fd(fd_out).ok();

    // 如果不是 pipe，先确认它至少是合法 file_like。
    // 这样 bad fd 会优先返回 EBADF，而不是被下面的 no-pipe 误判成 EINVAL。
    let in_file = if in_pipe.is_none() {
        Some(get_file_like(fd_in).map_err(|_| StarryError::BadFileDescriptor)?)
    } else {
        None
    };

    let out_file = if out_pipe.is_none() {
        Some(get_file_like(fd_out).map_err(|_| StarryError::BadFileDescriptor)?)
    } else {
        None
    };
    // splice 要求至少一端是 pipe。
    if in_pipe.is_none() && out_pipe.is_none() {
        return Err(StarryError::InvalidInput);
    }

    // 5. pipe 对应的 offset 必须是 NULL，否则返回 ESPIPE。
    if in_pipe.is_some() && !off_in.is_null() {
        return Err(StarryError::from(Errno::ESPIPE));
    }

    if out_pipe.is_some() && !off_out.is_null() {
        return Err(StarryError::from(Errno::ESPIPE));
    }

    // 6. 检查 pipe 方向。
    match &in_pipe {
        Some(pipe) if !pipe.is_read() => return Err(StarryError::BadFileDescriptor),
        _ => {}
    }

    match &out_pipe {
        Some(pipe) if !pipe.is_write() => return Err(StarryError::BadFileDescriptor),
        _ => {}
    }

    // 7. 同一个 pipe 不能同时作为输入和输出。
    match (&in_pipe, &out_pipe) {
        (Some(src), Some(dst)) if alloc::sync::Arc::ptr_eq(src, dst) => {
            return Err(StarryError::InvalidInput);
        }
        _ => {}
    }

    // 8. 读取 off_in。到这里时，如果 off_in 非空，fd_in 一定不是 pipe
    let in_pos = if !off_in.is_null() {
        let pos = off_in.vm_read()?;
        if pos < 0 {
            return Err(StarryError::InvalidInput);
        }
        Some(pos as u64)
    } else {
        None
    };

    // 9. 读取 off_out。
    // 到这里时，如果 off_out 非空，fd_out 一定不是 pipe。
    let out_pos = if !off_out.is_null() {
        let pos = off_out.vm_read()?;
        if pos < 0 {
            return Err(StarryError::InvalidInput);
        }
        Some(pos as u64)
    } else {
        None
    };

    // 10. 输出目标不能是 O_APPEND。注意要覆盖 off_out 为 NULL 和非 NULL 两种情况。
    match File::from_fd(fd_out) {
        Ok(file) if file.inner().access(FileFlags::APPEND).is_ok() => {
            return Err(StarryError::InvalidInput);
        }
        _ => {}
    }

    // 11. 构造输入端。
    let src = if let Some(pos) = in_pos {
        SendFile::Offset(File::from_fd(fd_in)?, off_in.cast(), pos)
    } else if let Some(file) = in_file {
        SendFile::Direct(file)
    } else {
        SendFile::Direct(get_file_like(fd_in)?)
    };
    // 12. 构造输出端。
    let dst: SendFile = if out_pos.is_some() {
        // Route memfd output through the seal-aware wrapper rather
        // than `File::from_fd`'s auto-unwrap.
        send_offset_out(fd_out, off_out.cast())?
    } else {
        let f = if let Some(file) = out_file {
            file
        } else {
            get_file_like(fd_out)?
        };

        f.write(&mut b"".as_slice())?;

        SendFile::Direct(f)
    };

    let n = do_send(src, dst, len, flags & SPLICE_F_NONBLOCK != 0)?;

    isize::try_from(n).map_err(|_| StarryError::InvalidInput)
}

#[cfg(all(test, not(axtest)))]
fn io_rwf_flags_validation_rules_hold_for_test() -> bool {
    // validate_rwf_flags: only flags==0 is accepted.
    validate_rwf_flags(0).is_ok()
        && validate_rwf_flags(1).is_err()
        && validate_rwf_flags(u32::MAX).is_err()
}

#[cfg(all(test, not(axtest)))]
fn io_offset_from_hilo_rules_hold_for_test() -> bool {
    // Test offset_from_hilo function
    // On 64-bit, offset_from_hilo should return pos_l directly
    let result = offset_from_hilo(1000, 0);
    assert!(result == 1000);

    let neg_result = offset_from_hilo(-1, 0);
    assert!(neg_result == -1);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn io_rwf_flags_validation_rules_hold() {
        assert!(super::io_rwf_flags_validation_rules_hold_for_test());
    }

    #[test]
    fn io_offset_from_hilo_rules_hold() {
        assert!(super::io_offset_from_hilo_rules_hold_for_test());
    }
}
