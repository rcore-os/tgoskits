use alloc::{borrow::Cow, sync::Arc, vec};
use core::{
    ffi::{c_char, c_int},
    task::Context,
};

use axerrno::{AxError, AxResult};
use axfs_ng::{FS_CONTEXT, FileFlags, OpenOptions};
use axio::{Seek, SeekFrom};
use axpoll::{IoEvents, Pollable};
use axtask::current;
use linux_raw_sys::general::__kernel_off_t;
use starry_vm::{VmMutPtr, VmPtr};
use syscalls::Sysno;

use crate::{
    file::{File, FileLike, Pipe, SealedBuf, SealedBufMut, get_file_like},
    io::{IoVec, IoVectorBuf},
    mm::{UserConstPtr, VmBytes, VmBytesMut},
};

struct DummyFd;
impl FileLike for DummyFd {
    fn read(&self, _dst: &mut SealedBufMut) -> AxResult<usize> {
        unimplemented!()
    }

    fn write(&self, _src: &mut SealedBuf) -> AxResult<usize> {
        unimplemented!()
    }

    fn stat(&self) -> AxResult<crate::file::Kstat> {
        unimplemented!()
    }

    fn path(&self) -> Cow<str> {
        "anon_inode:[bruh]".into()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
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
    Ok(get_file_like(fd)?.read(&mut VmBytesMut::new(buf, len).into())? as _)
}

pub fn sys_readv(fd: i32, iov: *const IoVec, iovcnt: usize) -> AxResult<isize> {
    debug!("sys_readv <= fd: {fd}, iovcnt: {iovcnt}");
    let f = get_file_like(fd)?;
    f.read(&mut IoVectorBuf::new(iov, iovcnt)?.into_io().into())
        .map(|n| n as _)
}

/// Write data to the file indicated by `fd`.
///
/// Return the written size if success.
pub fn sys_write(fd: i32, buf: *mut u8, len: usize) -> AxResult<isize> {
    debug!("sys_write <= fd: {fd}, buf: {buf:p}, len: {len}");
    Ok(get_file_like(fd)?.write(&mut VmBytes::new(buf, len).into())? as _)
}

pub fn sys_writev(fd: i32, iov: *const IoVec, iovcnt: usize) -> AxResult<isize> {
    debug!("sys_writev <= fd: {fd}, iovcnt: {iovcnt}");
    let f = get_file_like(fd)?;
    f.write(&mut IoVectorBuf::new(iov, iovcnt)?.into_io().into())
        .map(|n| n as _)
}

pub fn sys_lseek(fd: c_int, offset: __kernel_off_t, whence: c_int) -> AxResult<isize> {
    debug!("sys_lseek <= {fd} {offset} {whence}");
    let pos = match whence {
        0 => SeekFrom::Start(offset as _),
        1 => SeekFrom::Current(offset as _),
        2 => SeekFrom::End(offset as _),
        _ => return Err(AxError::InvalidInput),
    };
    let off = File::from_fd(fd)?.inner().seek(pos)?;
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
    let f = File::from_fd(fd)?;
    let inner = f.inner();
    let file = inner.access(FileFlags::WRITE)?;
    file.set_len(file.location().len()?.max(offset as u64 + len as u64))?;
    Ok(0)
}

pub fn sys_fsync(fd: c_int) -> AxResult<isize> {
    debug!("sys_fsync <= {fd}");
    let f = File::from_fd(fd)?;
    f.inner().sync(false)?;
    Ok(0)
}

pub fn sys_fdatasync(fd: c_int) -> AxResult<isize> {
    debug!("sys_fdatasync <= {fd}");
    let f = File::from_fd(fd)?;
    f.inner().sync(true)?;
    Ok(0)
}

pub fn sys_fadvise64(
    fd: c_int,
    offset: __kernel_off_t,
    len: __kernel_off_t,
    advice: u32,
) -> AxResult<isize> {
    debug!("sys_fadvise64 <= fd: {fd}, offset: {offset}, len: {len}, advice: {advice}");
    if Pipe::from_fd(fd).is_ok() {
        return Err(AxError::BrokenPipe);
    }
    if advice > 5 {
        return Err(AxError::InvalidInput);
    }
    Ok(0)
}

pub fn sys_pread64(fd: c_int, buf: *mut u8, len: usize, offset: __kernel_off_t) -> AxResult<isize> {
    let f = File::from_fd(fd)?;
    if offset < 0 {
        return Err(AxError::InvalidInput);
    }
    let read = f
        .inner()
        .read_at(&mut VmBytesMut::new(buf, len), offset as _)?;
    Ok(read as _)
}

pub fn sys_pwrite64(
    fd: c_int,
    buf: *const u8,
    len: usize,
    offset: __kernel_off_t,
) -> AxResult<isize> {
    if len == 0 {
        return Ok(0);
    }
    let f = File::from_fd(fd)?;
    let write = f
        .inner()
        .write_at(&mut VmBytes::new(buf, len), offset as _)?;
    Ok(write as _)
}

pub fn sys_preadv(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
) -> AxResult<isize> {
    sys_preadv2(fd, iov, iovcnt, offset, 0)
}

pub fn sys_pwritev(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
) -> AxResult<isize> {
    sys_pwritev2(fd, iov, iovcnt, offset, 0)
}

pub fn sys_preadv2(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
    _flags: u32,
) -> AxResult<isize> {
    debug!("sys_preadv2 <= fd: {fd}, iovcnt: {iovcnt}, offset: {offset}, flags: {_flags}");
    let f = File::from_fd(fd)?;
    f.inner()
        .read_at(&mut IoVectorBuf::new(iov, iovcnt)?.into_io(), offset as _)
        .map(|n| n as _)
}

pub fn sys_pwritev2(
    fd: c_int,
    iov: *const IoVec,
    iovcnt: usize,
    offset: __kernel_off_t,
    _flags: u32,
) -> AxResult<isize> {
    debug!("sys_pwritev2 <= fd: {fd}, iovcnt: {iovcnt}, offset: {offset}, flags: {_flags}");
    let f = File::from_fd(fd)?;
    f.inner()
        .read_at(&mut IoVectorBuf::new(iov, iovcnt)?.into_io(), offset as _)
        .map(|n| n as _)
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
            SendFile::Direct(file) => file.read(&mut buf.into()),
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
            SendFile::Direct(file) => file.write(&mut buf.into()),
            SendFile::Offset(file, offset) => {
                let off = offset.vm_read()?;
                let bytes_written = file.inner().write_at(&mut buf, off)?;
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
    _flags: u32,
) -> AxResult<isize> {
    debug!(
        "sys_copy_file_range <= fd_in: {}, off_in: {}, fd_out: {}, off_out: {}, len: {}, flags: {}",
        fd_in,
        !off_in.is_null(),
        fd_out,
        !off_out.is_null(),
        len,
        _flags
    );

    // TODO: check flags
    // TODO: check both regular files
    // TODO: check same file and overlap

    let src = if !off_in.is_null() {
        SendFile::Offset(File::from_fd(fd_in)?, off_in)
    } else {
        SendFile::Direct(get_file_like(fd_in)?)
    };

    let dst = if !off_out.is_null() {
        SendFile::Offset(File::from_fd(fd_out)?, off_out)
    } else {
        SendFile::Direct(get_file_like(fd_out)?)
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
        f.write(&mut b"".as_slice().into())?;
        SendFile::Direct(f)
    };

    if !has_pipe {
        return Err(AxError::InvalidInput);
    }

    do_send(src, dst, len).map(|n| n as _)
}
