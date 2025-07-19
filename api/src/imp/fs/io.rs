use alloc::{sync::Arc, vec};
use core::ffi::{c_char, c_int};

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::{FS_CONTEXT, FileFlags, OpenOptions};
use axio::{
    Seek, SeekFrom,
    buf::{Buf, BufMut},
};
use linux_raw_sys::general::{__kernel_off_t, iovec};

use crate::{
    file::{File, FileLike, Pipe, get_file_like},
    ptr::{UserConstPtr, UserPtr, nullable},
};

#[derive(Default)]
pub struct IoVectorBuf<'a> {
    iovs: &'a [iovec],
    offset: usize,
}
impl<'a> IoVectorBuf<'a> {
    fn new(iovs: &'a [iovec]) -> Self {
        let mut result = Self { iovs, offset: 0 };
        result.skip_empty();
        result
    }

    pub fn new_mut(iov: UserPtr<iovec>, iovcnt: usize) -> LinuxResult<Self> {
        if iovcnt == 0 {
            return Ok(Self::default());
        } else if iovcnt > 1024 {
            return Err(LinuxError::EINVAL);
        }
        let iovs = iov.get_as_mut_slice(iovcnt)?;
        for iov in iovs.iter_mut() {
            if iov.iov_len as i64 > 0 {
                UserPtr::<u8>::from(iov.iov_base as *mut _).get_as_mut_slice(iov.iov_len as _)?;
            }
        }
        Ok(Self::new(iovs))
    }

    pub fn new_const(iov: UserConstPtr<iovec>, iovcnt: usize) -> LinuxResult<Self> {
        if iovcnt == 0 {
            return Ok(Self::default());
        } else if iovcnt > 1024 {
            return Err(LinuxError::EINVAL);
        }
        let iovs = iov.get_as_slice(iovcnt)?;
        for iov in iovs {
            if iov.iov_len as i64 > 0 {
                UserConstPtr::<u8>::from(iov.iov_base as *const _)
                    .get_as_slice(iov.iov_len as _)?;
            }
        }
        Ok(Self::new(iovs))
    }

    fn skip_empty(&mut self) {
        while self
            .iovs
            .first()
            .is_some_and(|it| it.iov_len as i64 <= self.offset as i64)
        {
            self.iovs = &self.iovs[1..];
            self.offset = 0;
        }
    }
}
impl Buf for IoVectorBuf<'_> {
    fn remaining(&self) -> usize {
        self.iovs
            .iter()
            .filter_map(|iov| {
                if iov.iov_len as i64 > 0 {
                    Some(iov.iov_len as usize)
                } else {
                    None
                }
            })
            .sum::<usize>()
            - self.offset
    }

    fn chunk(&self) -> &[u8] {
        let Some(iov) = self.iovs.first() else {
            return &[];
        };
        let chunk =
            unsafe { core::slice::from_raw_parts(iov.iov_base as *const u8, iov.iov_len as usize) };
        &chunk[self.offset..]
    }

    fn advance(&mut self, mut n: usize) {
        while n > 0 {
            let Some(iov) = self.iovs.first() else {
                break;
            };
            let adv = n.min(iov.iov_len as usize - self.offset);
            n -= adv;
            self.offset += adv;
            self.skip_empty();
        }
    }
}
impl BufMut for IoVectorBuf<'_> {
    fn chunk_mut(&mut self) -> &mut [u8] {
        let Some(iov) = self.iovs.first() else {
            return &mut [];
        };
        unsafe { core::slice::from_raw_parts_mut(iov.iov_base as *mut u8, iov.iov_len as usize) }
    }
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
    OpenOptions::new()
        .write(true)
        .open(&FS_CONTEXT.lock(), path)?
        .into_file()?
        .access(FileFlags::WRITE)?
        .set_len(length as _)?;
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
            if !src.is_read() {
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
            && !src.is_write()
        {
            return Err(LinuxError::EBADF);
        }
        SendFile::Direct(get_file_like(fd_out)?)
    };

    do_send(src, dst, len).map(|n| n as _)
}
