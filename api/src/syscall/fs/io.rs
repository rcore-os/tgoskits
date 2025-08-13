use alloc::{sync::Arc, vec};
use core::{
    ffi::{c_char, c_int},
    task::Context,
};

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::{FS_CONTEXT, FileFlags, OpenOptions};
use axio::{
    IoEvents, Pollable, Seek, SeekFrom,
    buf::{BufExt, BufMutExt},
};
use linux_raw_sys::general::{__kernel_off_t, iovec};

use crate::{
    file::{File, FileLike, Pipe, get_file_like},
    io::IoVectorBuf,
    mm::{UserConstPtr, UserPtr, nullable},
};

struct DummyFd;
impl FileLike for DummyFd {
    fn read(&self, _buf: &mut [u8]) -> LinuxResult<usize> {
        unimplemented!()
    }

    fn write(&self, _buf: &[u8]) -> LinuxResult<usize> {
        unimplemented!()
    }

    fn stat(&self) -> LinuxResult<crate::file::Kstat> {
        unimplemented!()
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

pub fn sys_dummy_fd() -> LinuxResult<isize> {
    DummyFd.add_to_fd_table(false).map(|fd| fd as isize)
}

/// Read data from the file indicated by `fd`.
///
/// Return the read size if success.
pub fn sys_read(fd: i32, buf: UserPtr<u8>, len: usize) -> LinuxResult<isize> {
    let buf = buf.get_as_mut_slice(len)?;
    debug!(
        "sys_read <= fd: {}, buf: {:p}, len: {}",
        fd,
        buf.as_ptr(),
        buf.len()
    );
    Ok(get_file_like(fd)?.read(buf)? as _)
}

pub fn sys_readv(fd: i32, iov: UserPtr<iovec>, iovcnt: usize) -> LinuxResult<isize> {
    debug!("sys_readv <= fd: {}, iovcnt: {}", fd, iovcnt);
    let f = get_file_like(fd)?;
    IoVectorBuf::new_mut(iov, iovcnt)?
        .fill_with(|buf| f.read(buf))
        .map(|n| n as _)
}

/// Write data to the file indicated by `fd`.
///
/// Return the written size if success.
pub fn sys_write(fd: i32, buf: UserConstPtr<u8>, len: usize) -> LinuxResult<isize> {
    let buf = buf.get_as_slice(len)?;
    debug!(
        "sys_write <= fd: {}, buf: {:p}, len: {}",
        fd,
        buf.as_ptr(),
        buf.len()
    );
    Ok(get_file_like(fd)?.write(buf)? as _)
}

pub fn sys_writev(fd: i32, iov: UserConstPtr<iovec>, iovcnt: usize) -> LinuxResult<isize> {
    debug!("sys_writev <= fd: {}, iovcnt: {}", fd, iovcnt);
    let f = get_file_like(fd)?;
    IoVectorBuf::new_const(iov, iovcnt)?
        .read_with(|buf| f.write(buf))
        .map(|n| n as _)
}

pub fn sys_lseek(fd: c_int, offset: __kernel_off_t, whence: c_int) -> LinuxResult<isize> {
    debug!("sys_lseek <= {} {} {}", fd, offset, whence);
    let pos = match whence {
        0 => SeekFrom::Start(offset as _),
        1 => SeekFrom::Current(offset as _),
        2 => SeekFrom::End(offset as _),
        _ => return Err(LinuxError::EINVAL),
    };
    let off = File::from_fd(fd)?.inner().seek(pos)?;
    Ok(off as _)
}

pub fn sys_truncate(path: UserConstPtr<c_char>, length: __kernel_off_t) -> LinuxResult<isize> {
    let path = path.get_as_str()?;
    debug!("sys_truncate <= {:?} {}", path, length);
    if length < 0 {
        return Err(LinuxError::EINVAL);
    }
    let file = OpenOptions::new()
        .write(true)
        .open(&FS_CONTEXT.lock(), path)?
        .into_file()?;
    file.access(FileFlags::WRITE)?.set_len(length as _)?;
    Ok(0)
}

pub fn sys_ftruncate(fd: c_int, length: __kernel_off_t) -> LinuxResult<isize> {
    debug!("sys_ftruncate <= {} {}", fd, length);
    let f = File::from_fd(fd)?;
    f.inner().access(FileFlags::WRITE)?.set_len(length as _)?;
    Ok(0)
}

pub fn sys_fallocate(
    fd: c_int,
    mode: u32,
    offset: __kernel_off_t,
    len: __kernel_off_t,
) -> LinuxResult<isize> {
    debug!(
        "sys_fallocate <= fd: {}, mode: {}, offset: {}, len: {}",
        fd, mode, offset, len
    );
    if mode != 0 {
        return Err(LinuxError::EINVAL);
    }
    let f = File::from_fd(fd)?;
    let inner = f.inner();
    let file = inner.access(FileFlags::WRITE)?;
    file.set_len(file.location().len()?.max(offset as u64 + len as u64))?;
    Ok(0)
}

pub fn sys_fsync(fd: c_int) -> LinuxResult<isize> {
    debug!("sys_fsync <= {}", fd);
    let f = File::from_fd(fd)?;
    f.inner().sync(false)?;
    Ok(0)
}

pub fn sys_fdatasync(fd: c_int) -> LinuxResult<isize> {
    debug!("sys_fdatasync <= {}", fd);
    let f = File::from_fd(fd)?;
    f.inner().sync(true)?;
    Ok(0)
}

pub fn sys_pread64(
    fd: c_int,
    buf: UserPtr<u8>,
    len: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
    let buf = buf.get_as_mut_slice(len)?;
    let f = File::from_fd(fd)?;
    if offset < 0 {
        return Err(LinuxError::EINVAL);
    }
    let read = f.inner().read_at(buf, offset as _)?;
    Ok(read as _)
}

pub fn sys_pwrite64(
    fd: c_int,
    buf: UserConstPtr<u8>,
    len: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
    if len == 0 {
        return Ok(0);
    }
    let buf = buf.get_as_slice(len)?;
    let f = File::from_fd(fd)?;
    let write = f.inner().write_at(buf, offset as _)?;
    Ok(write as _)
}

pub fn sys_preadv(
    fd: c_int,
    iov: UserPtr<iovec>,
    iovcnt: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
    sys_preadv2(fd, iov, iovcnt, offset, 0)
}

pub fn sys_pwritev(
    fd: c_int,
    iov: UserConstPtr<iovec>,
    iovcnt: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
    sys_pwritev2(fd, iov, iovcnt, offset, 0)
}

pub fn sys_preadv2(
    fd: c_int,
    iov: UserPtr<iovec>,
    iovcnt: usize,
    mut offset: __kernel_off_t,
    _flags: u32,
) -> LinuxResult<isize> {
    debug!(
        "sys_preadv2 <= fd: {}, iovcnt: {}, offset: {}, flags: {}",
        fd, iovcnt, offset, _flags
    );
    let f = File::from_fd(fd)?;
    IoVectorBuf::new_mut(iov, iovcnt)?
        .fill_with(|buf| {
            let read = f.inner().read_at(buf, offset as _)?;
            offset += read as __kernel_off_t;
            Ok(read)
        })
        .map(|n| n as _)
}

pub fn sys_pwritev2(
    fd: c_int,
    iov: UserConstPtr<iovec>,
    iovcnt: usize,
    mut offset: __kernel_off_t,
    _flags: u32,
) -> LinuxResult<isize> {
    debug!(
        "sys_pwritev2 <= fd: {}, iovcnt: {}, offset: {}, flags: {}",
        fd, iovcnt, offset, _flags
    );
    let f = File::from_fd(fd)?;
    IoVectorBuf::new_const(iov, iovcnt)?
        .read_with(|buf| {
            let write = f.inner().write_at(buf, offset as _)?;
            offset += write as __kernel_off_t;
            Ok(write)
        })
        .map(|n| n as _)
}

enum SendFile<'a> {
    Direct(Arc<dyn FileLike>),
    Offset(Arc<File>, &'a mut u64),
}

impl<'a> SendFile<'a> {
    fn has_data(&self) -> bool {
        match self {
            SendFile::Direct(file) => file.poll(),
            SendFile::Offset(file, _) => file.poll(),
        }
        .contains(IoEvents::IN)
    }

    fn read(&mut self, buf: &mut [u8]) -> LinuxResult<usize> {
        match self {
            SendFile::Direct(file) => file.read(buf),
            SendFile::Offset(file, offset) => {
                let bytes_read = file.inner().read_at(buf, **offset)?;
                **offset += bytes_read as u64;
                Ok(bytes_read)
            }
        }
    }

    fn write(&mut self, buf: &[u8]) -> LinuxResult<usize> {
        match self {
            SendFile::Direct(file) => file.write(buf),
            SendFile::Offset(file, offset) => {
                let bytes_written = file.inner().write_at(buf, **offset)?;
                **offset += bytes_written as u64;
                Ok(bytes_written)
            }
        }
    }
}

fn do_send(mut src: SendFile<'_>, mut dst: SendFile<'_>, len: usize) -> LinuxResult<usize> {
    let mut buf = vec![0; 0x4000];
    let mut total_written = 0;
    let mut remaining = len;

    while remaining > 0 {
        if total_written > 0 && !src.has_data() {
            break;
        }
        let to_read = buf.len().min(remaining);
        let bytes_read = match src.read(&mut buf[..to_read]) {
            Ok(n) => n,
            Err(LinuxError::EAGAIN) if total_written > 0 => break,
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

pub fn sys_sendfile(
    out_fd: c_int,
    in_fd: c_int,
    offset: UserPtr<u64>,
    len: usize,
) -> LinuxResult<isize> {
    debug!(
        "sys_sendfile <= out_fd: {}, in_fd: {}, offset: {}, len: {}",
        out_fd,
        in_fd,
        !offset.is_null(),
        len
    );

    let offset = nullable!(offset.get_as_mut())?;
    let src = if let Some(offset) = offset {
        if *offset > u32::MAX as u64 {
            return Err(LinuxError::EINVAL);
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
    off_in: UserPtr<u64>,
    fd_out: c_int,
    off_out: UserPtr<u64>,
    len: usize,
    _flags: u32,
) -> LinuxResult<isize> {
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

    let off_in = nullable!(off_in.get_as_mut())?;
    let src = if let Some(off_in) = off_in {
        SendFile::Offset(File::from_fd(fd_in)?, off_in)
    } else {
        SendFile::Direct(get_file_like(fd_in)?)
    };

    let off_out = nullable!(off_out.get_as_mut())?;
    let dst = if let Some(off_out) = off_out {
        SendFile::Offset(File::from_fd(fd_out)?, off_out)
    } else {
        SendFile::Direct(get_file_like(fd_out)?)
    };

    do_send(src, dst, len).map(|n| n as _)
}

pub fn sys_splice(
    fd_in: c_int,
    off_in: UserPtr<i64>,
    fd_out: c_int,
    off_out: UserPtr<i64>,
    len: usize,
    _flags: u32,
) -> LinuxResult<isize> {
    debug!(
        "sys_splice <= fd_in: {}, off_in: {}, fd_out: {}, off_out: {}, len: {}, flags: {}",
        fd_in,
        !off_in.is_null(),
        fd_out,
        !off_out.is_null(),
        len,
        _flags
    );

    if !(Pipe::from_fd(fd_in).is_ok() || Pipe::from_fd(fd_out).is_ok()) {
        return Err(LinuxError::EINVAL);
    }

    let mut has_pipe = false;

    if DummyFd::from_fd(fd_in).is_ok() || DummyFd::from_fd(fd_out).is_ok() {
        return Err(LinuxError::EBADF);
    }

    let src = if let Some(off) = nullable!(off_in.get_as_mut())? {
        if *off < 0 {
            return Err(LinuxError::EINVAL);
        }
        SendFile::Offset(File::from_fd(fd_in)?, off_in.cast().get_as_mut()?)
    } else {
        if let Ok(src) = Pipe::from_fd(fd_in) {
            if !src.is_read() {
                return Err(LinuxError::EBADF);
            }
            has_pipe = true;
        }
        if let Ok(file) = File::from_fd(fd_in)
            && file.inner().is_path()
        {
            return Err(LinuxError::EINVAL);
        }
        SendFile::Direct(get_file_like(fd_in)?)
    };

    let dst = if let Some(off) = nullable!(off_out.get_as_mut())? {
        if *off < 0 {
            return Err(LinuxError::EINVAL);
        }
        SendFile::Offset(File::from_fd(fd_out)?, off_out.cast().get_as_mut()?)
    } else {
        if let Ok(dst) = Pipe::from_fd(fd_out) {
            if !dst.is_write() {
                return Err(LinuxError::EBADF);
            }
            has_pipe = true;
        }
        if let Ok(file) = File::from_fd(fd_out)
            && file.inner().access(FileFlags::APPEND).is_ok()
        {
            return Err(LinuxError::EINVAL);
        }
        let f = get_file_like(fd_out)?;
        f.write(b"")?;
        SendFile::Direct(f)
    };

    if !has_pipe {
        return Err(LinuxError::EINVAL);
    }

    do_send(src, dst, len).map(|n| n as _)
}
