use alloc::{borrow::Cow, sync::Arc, vec};
use core::{
    ffi::{c_char, c_int},
    task::Context,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs::{FS_CONTEXT, FileFlags, OpenOptions};
use ax_io::{Seek, SeekFrom};
use ax_task::current;
use axfs_ng_vfs::NodeType;
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::__kernel_off_t;
use starry_vm::{VmMutPtr, VmPtr};
use syscalls::Sysno;

use crate::{
    file::{File, FileLike, Pipe, get_file_like},
    mm::{IoVec, IoVectorBuf, UserConstPtr, VmBytes, VmBytesMut},
};

/// Get a [`File`] from fd, converting type-mismatch errors to ESPIPE.
/// Use this for syscalls that require a regular file fd and should return
/// ESPIPE for pipes/sockets (lseek, pread, pwrite, fallocate, etc.).
fn file_or_espipe(fd: c_int) -> AxResult<Arc<File>> {
    File::from_fd(fd).map_err(|e| {
        if e == AxError::IsADirectory || e == AxError::BadFileDescriptor {
            e
        } else {
            AxError::from(LinuxError::ESPIPE)
        }
    })
}

/// Like `file_or_espipe`, but for write operations: converts IsADirectory
/// to BadFileDescriptor because directories cannot be opened for writing.
fn file_or_espipe_write(fd: c_int) -> AxResult<Arc<File>> {
    file_or_espipe(fd).map_err(|e| {
        if e == AxError::IsADirectory {
            AxError::BadFileDescriptor
        } else {
            e
        }
    })
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

pub fn sys_dummy_fd(sysno: Sysno) -> AxResult<isize> {
    if current().name().starts_with("qemu-") {
        // We need to be honest to qemu, since it can automatically fallback to
        // other strategies.
        return Err(AxError::Unsupported);
    }
    warn!("Dummy fd created: {sysno}");
    DummyFd.add_to_fd_table(false).map(|fd| fd as isize)
}

/// Read data from the file indicated by `fd`.
///
/// Return the read size if success.
pub fn sys_read(fd: i32, buf: *mut u8, len: usize) -> AxResult<isize> {
    debug!("sys_read <= fd: {fd}, buf: {buf:p}, len: {len}");
    Ok(get_file_like(fd)?.read(&mut VmBytesMut::new(buf, len))? as _)
}

pub fn sys_readv(fd: i32, iov: *const IoVec, iovcnt: usize) -> AxResult<isize> {
    debug!("sys_readv <= fd: {fd}, iovcnt: {iovcnt}");
    let f = get_file_like(fd)?;
    f.read(&mut IoVectorBuf::new(iov, iovcnt)?.into_io())
        .map(|n| n as _)
}

/// Write data to the file indicated by `fd`.
///
/// Return the written size if success.
pub fn sys_write(fd: i32, buf: *mut u8, len: usize) -> AxResult<isize> {
    debug!("sys_write <= fd: {fd}, buf: {buf:p}, len: {len}");
    Ok(get_file_like(fd)?.write(&mut VmBytes::new(buf, len))? as _)
}

pub fn sys_writev(fd: i32, iov: *const IoVec, iovcnt: usize) -> AxResult<isize> {
    debug!("sys_writev <= fd: {fd}, iovcnt: {iovcnt}");
    let f = get_file_like(fd)?;
    f.write(&mut IoVectorBuf::new(iov, iovcnt)?.into_io())
        .map(|n| n as _)
}

pub fn sys_lseek(fd: c_int, offset: __kernel_off_t, whence: c_int) -> AxResult<isize> {
    debug!("sys_lseek <= {fd} {offset} {whence}");
    let pos = match whence {
        0 => {
            if offset < 0 {
                return Err(AxError::InvalidInput);
            }
            SeekFrom::Start(offset as _)
        }
        1 => SeekFrom::Current(offset as _),
        2 => SeekFrom::End(offset as _),
        _ => return Err(AxError::InvalidInput),
    };
    let off = file_or_espipe(fd)?.inner().seek(pos)?;
    Ok(off as _)
}

pub fn sys_truncate(path: UserConstPtr<c_char>, length: __kernel_off_t) -> AxResult<isize> {
    let path = path.get_as_str()?;
    debug!("sys_truncate <= {path:?} {length}");
    if length < 0 {
        return Err(AxError::InvalidInput);
    }
    let file = OpenOptions::new()
        .write(true)
        .open(&FS_CONTEXT.lock(), path)?
        .into_file()?;
    file.access(FileFlags::WRITE)?.set_len(length as _)?;
    Ok(0)
}

pub fn sys_ftruncate(fd: c_int, length: __kernel_off_t) -> AxResult<isize> {
    debug!("sys_ftruncate <= {fd} {length}");
    let f = File::from_fd(fd)?;
    f.inner().access(FileFlags::WRITE)?.set_len(length as _)?;
    Ok(0)
}

pub fn sys_fallocate(
    fd: c_int,
    mode: u32,
    offset: __kernel_off_t,
    len: __kernel_off_t,
) -> AxResult<isize> {
    debug!("sys_fallocate <= fd: {fd}, mode: {mode}, offset: {offset}, len: {len}");
    if mode != 0 {
        return Err(AxError::InvalidInput);
    }
    let f = file_or_espipe(fd)?;
    let inner = f.inner();
    let file = inner.access(FileFlags::WRITE)?;
    file.set_len(file.location().len()?.max(offset as u64 + len as u64))?;
    Ok(0)
}

pub fn sys_fsync(fd: c_int) -> AxResult<isize> {
    debug!("sys_fsync <= {fd}");
    match File::from_fd(fd) {
        Ok(f) => {
            f.inner().sync(false)?;
            Ok(0)
        }
        Err(AxError::IsADirectory) => {
            // Linux allows fsync() on directory fds to flush directory
            // metadata. We don't have directory-level sync, but returning
            // Ok matches Linux behavior for applications like PostgreSQL.
            Ok(0)
        }
        Err(e) => Err(e),
    }
}

pub fn sys_fdatasync(fd: c_int) -> AxResult<isize> {
    debug!("sys_fdatasync <= {fd}");
    match File::from_fd(fd) {
        Ok(f) => {
            f.inner().sync(true)?;
            Ok(0)
        }
        Err(AxError::IsADirectory) => Ok(0),
        Err(e) => Err(e),
    }
}

pub fn sys_sync_file_range(fd: c_int, _offset: i64, _nbytes: i64, _flags: u32) -> AxResult<isize> {
    debug!("sys_sync_file_range <= fd: {fd}");
    // sync_file_range(2) is an advisory hint to initiate writeback for a
    // byte range. Until range-based writeback is implemented, keep this as
    // a no-op after basic fd validation rather than turning it into a
    // stronger whole-file fdatasync-style flush (matches the advisory
    // nature documented in the man page). Invalid fds still surface the
    // underlying error (EBADF). Directory fds are accepted to match fsync.
    match File::from_fd(fd) {
        Ok(_) | Err(AxError::IsADirectory) => Ok(0),
        Err(e) => Err(e),
    }
}

pub fn sys_fadvise64(
    fd: c_int,
    offset: __kernel_off_t,
    len: __kernel_off_t,
    advice: u32,
) -> AxResult<isize> {
    debug!("sys_fadvise64 <= fd: {fd}, offset: {offset}, len: {len}, advice: {advice}");
    if Pipe::from_fd(fd).is_ok() {
        return Err(AxError::from(LinuxError::ESPIPE));
    }
    if advice > 5 {
        return Err(AxError::InvalidInput);
    }
    Ok(0)
}

pub fn sys_pread64(fd: c_int, buf: *mut u8, len: usize, offset: __kernel_off_t) -> AxResult<isize> {
    let f = file_or_espipe(fd)?;
    if offset < 0 {
        return Err(AxError::InvalidInput);
    }
    let read = f.inner().read_at(VmBytesMut::new(buf, len), offset as _)?;
    Ok(read as _)
}

pub fn sys_pwrite64(
    fd: c_int,
    buf: *const u8,
    len: usize,
    offset: __kernel_off_t,
) -> AxResult<isize> {
    if offset < 0 {
        return Err(AxError::InvalidInput);
    }
    if len == 0 {
        return Ok(0);
    }
    let f = file_or_espipe_write(fd)?;
    let write = f.inner().write_at(VmBytes::new(buf, len), offset as _)?;
    Ok(write as _)
}

pub fn sys_preadv(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
) -> AxResult<isize> {
    // preadv (unlike preadv2) does not accept offset=-1; reject negative offsets.
    if offset < 0 {
        return Err(AxError::InvalidInput);
    }
    sys_preadv2(fd, iov, iovcnt, offset, 0)
}

pub fn sys_pwritev(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
) -> AxResult<isize> {
    // pwritev (unlike pwritev2) does not accept offset=-1; reject negative offsets.
    if offset < 0 {
        return Err(AxError::InvalidInput);
    }
    sys_pwritev2(fd, iov, iovcnt, offset, 0)
}

/// Validate preadv2/pwritev2 flags.
/// Currently no RWF_* flags are supported; any non-zero value is rejected.
fn validate_rwf_flags(flags: u32) -> AxResult<()> {
    if flags != 0 {
        return Err(AxError::OperationNotSupported);
    }
    Ok(())
}

pub fn sys_preadv2(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
    flags: u32,
) -> AxResult<isize> {
    debug!("sys_preadv2 <= fd: {fd}, iovcnt: {iovcnt}, offset: {offset}, flags: {flags}");
    validate_rwf_flags(flags)?;
    if offset < -1 {
        return Err(AxError::InvalidInput);
    }
    let mut io_buf = IoVectorBuf::new(iov, iovcnt)?.into_io();
    if offset == -1 {
        // offset == -1: use current file position (like readv)
        let f = get_file_like(fd)?;
        f.read(&mut io_buf).map(|n| n as _)
    } else {
        let f = file_or_espipe(fd)?;
        f.inner().read_at(io_buf, offset as _).map(|n| n as _)
    }
}

pub fn sys_pwritev2(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
    flags: u32,
) -> AxResult<isize> {
    debug!("sys_pwritev2 <= fd: {fd}, iovcnt: {iovcnt}, offset: {offset}, flags: {flags}");
    validate_rwf_flags(flags)?;
    if offset < -1 {
        return Err(AxError::InvalidInput);
    }
    let mut io_buf = IoVectorBuf::new(iov, iovcnt)?.into_io();
    if offset == -1 {
        // offset == -1: use current file position (like writev)
        let f = get_file_like(fd)?;
        f.write(&mut io_buf).map(|n| n as _)
    } else {
        let f = file_or_espipe_write(fd)?;
        f.inner().write_at(io_buf, offset as _).map(|n| n as _)
    }
}

enum SendFile {
    Direct(Arc<dyn FileLike>),
    Offset(Arc<File>, *mut u64),
}

impl SendFile {
    fn has_data(&self) -> bool {
        match self {
            SendFile::Direct(file) => file.poll(),
            SendFile::Offset(file, ..) => file.poll(),
        }
        .contains(IoEvents::IN)
    }

    fn read(&mut self, mut buf: &mut [u8]) -> AxResult<usize> {
        match self {
            SendFile::Direct(file) => file.read(&mut buf),
            SendFile::Offset(file, offset) => {
                let off = offset.vm_read()?;
                let bytes_read = file.inner().read_at(&mut buf, off)?;
                offset.vm_write(off + bytes_read as u64)?;
                Ok(bytes_read)
            }
        }
    }

    fn write(&mut self, mut buf: &[u8]) -> AxResult<usize> {
        match self {
            SendFile::Direct(file) => file.write(&mut buf),
            SendFile::Offset(file, offset) => {
                let off = offset.vm_read()?;
                let bytes_written = file.inner().write_at(buf, off)?;
                offset.vm_write(off + bytes_written as u64)?;
                Ok(bytes_written)
            }
        }
    }
}

fn do_send(mut src: SendFile, mut dst: SendFile, len: usize) -> AxResult<usize> {
    let mut buf = vec![0; 0x1000];
    let mut total_written = 0;
    let mut remaining = len;

    while remaining > 0 {
        if total_written > 0 && !src.has_data() {
            break;
        }
        let to_read = buf.len().min(remaining);
        let bytes_read = match src.read(&mut buf[..to_read]) {
            Ok(n) => n,
            Err(AxError::WouldBlock) if total_written > 0 => break,
            Err(e) => return Err(e),
        };
        if bytes_read == 0 {
            break;
        }

        let bytes_written = dst.write(&buf[..bytes_read])?;
        if bytes_written < bytes_read {
            break;
        }

        total_written += bytes_written;
        remaining -= bytes_written;
    }

    Ok(total_written)
}

pub fn sys_sendfile(out_fd: c_int, in_fd: c_int, offset: *mut u64, len: usize) -> AxResult<isize> {
    debug!(
        "sys_sendfile <= out_fd: {}, in_fd: {}, offset: {}, len: {}",
        out_fd,
        in_fd,
        !offset.is_null(),
        len
    );

    let src = if !offset.is_null() {
        if offset.vm_read()? > u32::MAX as u64 {
            return Err(AxError::InvalidInput);
        }
        SendFile::Offset(File::from_fd(in_fd)?, offset)
    } else {
        SendFile::Direct(get_file_like(in_fd)?)
    };

    let dst = SendFile::Direct(get_file_like(out_fd)?);

    do_send(src, dst, len).map(|n| n as _)
}

pub fn sys_copy_file_range(
    fd_in: c_int,
    off_in: *mut u64,
    fd_out: c_int,
    off_out: *mut u64,
    len: usize,
    flags: u32,
) -> AxResult<isize> {
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
        return Err(AxError::InvalidInput);
    }

    let remap = |e| match e {
        AxError::BadFileDescriptor | AxError::IsADirectory => e,
        _ => AxError::InvalidInput,
    };
    let file_in = File::from_fd(fd_in).map_err(remap)?;
    let file_out = File::from_fd(fd_out).map_err(remap)?;
    let meta_in = file_in.inner().location().metadata()?;
    let meta_out = file_out.inner().location().metadata()?;

    if meta_in.node_type != NodeType::RegularFile || meta_out.node_type != NodeType::RegularFile {
        return Err(AxError::InvalidInput);
    }
    if file_out.inner().access(FileFlags::APPEND).is_ok() {
        return Err(AxError::BadFileDescriptor);
    }

    if len > 0 && meta_in.device == meta_out.device && meta_in.inode == meta_out.inode {
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
        if let Some(copy_end) = (len as u64).checked_sub(1) {
            let in_end = pos_in.checked_add(copy_end).ok_or(AxError::InvalidInput)?;
            let out_end = pos_out.checked_add(copy_end).ok_or(AxError::InvalidInput)?;
            if in_end >= pos_out && pos_in <= out_end {
                return Err(AxError::InvalidInput);
            }
        }
    }

    let src = if !off_in.is_null() {
        SendFile::Offset(file_in, off_in)
    } else {
        SendFile::Direct(file_in)
    };

    let dst = if !off_out.is_null() {
        SendFile::Offset(file_out, off_out)
    } else {
        SendFile::Direct(file_out)
    };

    do_send(src, dst, len).map(|n| n as _)
}

pub fn sys_splice(
    fd_in: c_int,
    off_in: *mut i64,
    fd_out: c_int,
    off_out: *mut i64,
    len: usize,
    _flags: u32,
) -> AxResult<isize> {
    debug!(
        "sys_splice <= fd_in: {}, off_in: {}, fd_out: {}, off_out: {}, len: {}, flags: {}",
        fd_in,
        !off_in.is_null(),
        fd_out,
        !off_out.is_null(),
        len,
        _flags
    );

    let mut has_pipe = false;

    if DummyFd::from_fd(fd_in).is_ok() || DummyFd::from_fd(fd_out).is_ok() {
        return Err(AxError::BadFileDescriptor);
    }

    let src = if !off_in.is_null() {
        if off_in.vm_read()? < 0 {
            return Err(AxError::InvalidInput);
        }
        SendFile::Offset(File::from_fd(fd_in)?, off_in.cast())
    } else {
        if let Ok(src) = Pipe::from_fd(fd_in) {
            if !src.is_read() {
                return Err(AxError::BadFileDescriptor);
            }
            has_pipe = true;
        }
        if let Ok(file) = File::from_fd(fd_in)
            && file.inner().is_path()
        {
            return Err(AxError::InvalidInput);
        }
        SendFile::Direct(get_file_like(fd_in)?)
    };

    let dst = if !off_out.is_null() {
        if off_out.vm_read()? < 0 {
            return Err(AxError::InvalidInput);
        }
        SendFile::Offset(File::from_fd(fd_out)?, off_out.cast())
    } else {
        if let Ok(dst) = Pipe::from_fd(fd_out) {
            if !dst.is_write() {
                return Err(AxError::BadFileDescriptor);
            }
            has_pipe = true;
        }
        if let Ok(file) = File::from_fd(fd_out)
            && file.inner().access(FileFlags::APPEND).is_ok()
        {
            return Err(AxError::InvalidInput);
        }
        let f = get_file_like(fd_out)?;
        f.write(&mut b"".as_slice())?;
        SendFile::Direct(f)
    };

    if !has_pipe {
        return Err(AxError::InvalidInput);
    }

    do_send(src, dst, len).map(|n| n as _)
}
