use core::ffi::c_int;

use alloc::{sync::Arc, vec};
use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FileFlags;
use axio::{Seek, SeekFrom};
use linux_raw_sys::general::{__kernel_off_t, iovec};

use crate::{
    file::{File, FileLike, Pipe, get_file_like},
    ptr::{UserConstPtr, UserPtr, nullable},
};

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

fn readv_impl(
    iov: UserPtr<iovec>,
    iovcnt: usize,
    mut f: impl FnMut(&mut [u8]) -> LinuxResult<usize>,
) -> LinuxResult<isize> {
    if iovcnt == 0 {
        return Ok(0);
    } else if iovcnt > 1024 {
        return Err(LinuxError::EINVAL);
    }

    let iovs = iov.get_as_mut_slice(iovcnt)?;
    let mut ret = 0;
    for iov in iovs {
        if iov.iov_len == 0 {
            continue;
        }
        let buf = UserPtr::<u8>::from(iov.iov_base as usize);
        let buf = buf.get_as_mut_slice(iov.iov_len as _)?;

        let read = f(buf)?;
        ret += read;

        if read < buf.len() {
            break;
        }
    }

    Ok(ret as isize)
}

fn writev_impl(
    iov: UserConstPtr<iovec>,
    iovcnt: usize,
    mut f: impl FnMut(&[u8]) -> LinuxResult<usize>,
) -> LinuxResult<isize> {
    if iovcnt == 0 {
        return Ok(0);
    } else if iovcnt > 1024 {
        return Err(LinuxError::EINVAL);
    }

    let iovs = iov.get_as_slice(iovcnt)?;
    let mut ret = 0;
    for iov in iovs {
        if iov.iov_len == 0 {
            continue;
        }
        let buf = UserConstPtr::<u8>::from(iov.iov_base as usize);
        let buf = buf.get_as_slice(iov.iov_len as _)?;

        let write = f(buf)?;
        ret += write;

        if write < buf.len() {
            break;
        }
    }

    Ok(ret as isize)
}

pub fn sys_readv(fd: i32, iov: UserPtr<iovec>, iovcnt: usize) -> LinuxResult<isize> {
    debug!("sys_readv <= fd: {}, iovcnt: {}", fd, iovcnt);
    let f = get_file_like(fd)?;
    readv_impl(iov, iovcnt, |buf| f.read(buf))
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
    writev_impl(iov, iovcnt, |buf| f.write(buf))
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
    file.set_len(file.len()?.max(offset as u64 + len as u64))?;
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
    let read = f.inner().read_at(buf, offset as _)?;
    Ok(read as _)
}

pub fn sys_pwrite64(
    fd: c_int,
    buf: UserConstPtr<u8>,
    len: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
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
    readv_impl(iov, iovcnt, |buf| {
        let read = f.inner().read_at(buf, offset as _)?;
        offset += read as __kernel_off_t;
        Ok(read)
    })
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
    writev_impl(iov, iovcnt, |buf| {
        let write = f.inner().write_at(buf, offset as _)?;
        offset += write as __kernel_off_t;
        Ok(write)
    })
}

enum SendFile<'a> {
    Direct(Arc<dyn FileLike>),
    Offset(Arc<File>, &'a mut u64),
}

impl<'a> SendFile<'a> {
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
        let to_read = buf.len().min(remaining);
        let bytes_read = src.read(&mut buf[..to_read])?;
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
        SendFile::Offset(File::from_fd(in_fd)?, offset)
    } else {
        SendFile::Direct(get_file_like(in_fd)?)
    };

    let dst = SendFile::Direct(get_file_like(out_fd)?);

    do_send(src, dst, len).map(|n| n as _)
}

pub fn sys_splice(
    fd_in: c_int,
    off_in: UserPtr<u64>,
    fd_out: c_int,
    off_out: UserPtr<u64>,
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

    let off_in = nullable!(off_in.get_as_mut())?;
    let src = if let Some(off_in) = off_in {
        SendFile::Offset(File::from_fd(fd_in)?, off_in)
    } else {
        if let Ok(src) = Pipe::from_fd(fd_in) {
            if !src.readable() {
                return Err(LinuxError::EBADF);
            }
            if !src.poll()?.readable {
                return Err(LinuxError::EINVAL);
            }
        }
        SendFile::Direct(get_file_like(fd_in)?)
    };

    let off_out = nullable!(off_out.get_as_mut())?;
    let dst = if let Some(off_out) = off_out {
        SendFile::Offset(File::from_fd(fd_out)?, off_out)
    } else {
        if let Ok(src) = Pipe::from_fd(fd_in)
            && !src.writable()
        {
            return Err(LinuxError::EBADF);
        }
        SendFile::Direct(get_file_like(fd_out)?)
    };

    do_send(src, dst, len).map(|n| n as _)
}
