use core::ffi::c_int;

use alloc::{sync::Arc, vec};
use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FileFlags;
use axio::{Seek, SeekFrom};
use linux_raw_sys::general::{__kernel_off_t, iovec};

use crate::{
    file::{File, FileLike, get_file_like},
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
    iocnt: usize,
    mut f: impl FnMut(&mut [u8]) -> LinuxResult<usize>,
) -> LinuxResult<isize> {
    if !(0..=1024).contains(&iocnt) {
        return Err(LinuxError::EINVAL);
    }

    let iovs = iov.get_as_mut_slice(iocnt)?;
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
    iocnt: usize,
    mut f: impl FnMut(&[u8]) -> LinuxResult<usize>,
) -> LinuxResult<isize> {
    if !(0..=1024).contains(&iocnt) {
        return Err(LinuxError::EINVAL);
    }

    let iovs = iov.get_as_slice(iocnt)?;
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

pub fn sys_readv(fd: i32, iov: UserPtr<iovec>, iocnt: usize) -> LinuxResult<isize> {
    let f = get_file_like(fd)?;
    readv_impl(iov, iocnt, |buf| f.read(buf))
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

pub fn sys_writev(fd: i32, iov: UserConstPtr<iovec>, iocnt: usize) -> LinuxResult<isize> {
    let f = get_file_like(fd)?;
    writev_impl(iov, iocnt, |buf| f.write(buf))
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
    let f = get_as_fs_file(fd)?;
    f.inner().access(FileFlags::WRITE)?.set_len(length as _)?;
    Ok(0)
}

pub fn sys_fsync(fd: c_int) -> LinuxResult<isize> {
    debug!("sys_fsync <= {}", fd);
    let f = get_as_fs_file(fd)?;
    f.inner().sync(false)?;
    Ok(0)
}

pub fn sys_fdatasync(fd: c_int) -> LinuxResult<isize> {
    debug!("sys_fdatasync <= {}", fd);
    let f = get_as_fs_file(fd)?;
    f.inner().sync(true)?;
    Ok(0)
}

pub(crate) fn get_as_fs_file(fd: c_int) -> LinuxResult<Arc<File>> {
    get_file_like(fd)?
        .into_any()
        .downcast::<File>()
        .map_err(|_| LinuxError::EBADF)
}

pub fn sys_pread64(
    fd: c_int,
    buf: UserPtr<u8>,
    len: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
    let buf = buf.get_as_mut_slice(len)?;
    let f = get_as_fs_file(fd)?;
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
    let f = get_as_fs_file(fd)?;
    let write = f.inner().write_at(buf, offset as _)?;
    Ok(write as _)
}

pub fn sys_preadv(
    fd: c_int,
    iov: UserPtr<iovec>,
    iocnt: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
    sys_preadv2(fd, iov, iocnt, offset, 0)
}

pub fn sys_pwritev(
    fd: c_int,
    iov: UserConstPtr<iovec>,
    iocnt: usize,
    offset: __kernel_off_t,
) -> LinuxResult<isize> {
    sys_pwritev2(fd, iov, iocnt, offset, 0)
}

pub fn sys_preadv2(
    fd: c_int,
    iov: UserPtr<iovec>,
    iocnt: usize,
    mut offset: __kernel_off_t,
    _flags: u32,
) -> LinuxResult<isize> {
    let f = get_as_fs_file(fd)?;
    readv_impl(iov, iocnt, |buf| {
        let read = f.inner().read_at(buf, offset as _)?;
        offset += read as __kernel_off_t;
        Ok(read)
    })
}

pub fn sys_pwritev2(
    fd: c_int,
    iov: UserConstPtr<iovec>,
    iocnt: usize,
    mut offset: __kernel_off_t,
    _flags: u32,
) -> LinuxResult<isize> {
    let f = get_as_fs_file(fd)?;
    writev_impl(iov, iocnt, |buf| {
        let write = f.inner().write_at(buf, offset as _)?;
        offset += write as __kernel_off_t;
        Ok(write)
    })
}

fn do_sendfile<F, D>(mut read: F, dest: &D) -> LinuxResult<usize>
where
    F: FnMut(&mut [u8]) -> LinuxResult<usize>,
    D: FileLike + ?Sized,
{
    let mut buf = vec![0; 0x4000];
    let mut total_written = 0;
    loop {
        let bytes_read = read(&mut buf)?;
        if bytes_read == 0 {
            break;
        }

        let bytes_written = dest.write(&buf[..bytes_read])?;
        if bytes_written < bytes_read {
            break;
        }
        total_written += bytes_written;
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

    let src = get_file_like(in_fd)?;
    let dest = get_file_like(out_fd)?;
    let offset = nullable!(offset.get_as_mut())?;

    if let Some(offset) = offset {
        let src = src
            .into_any()
            .downcast::<File>()
            .map_err(|_| LinuxError::ESPIPE)?;

        do_sendfile(
            |buf| {
                let bytes_read = src.inner().read_at(buf, *offset)?;
                *offset += bytes_read as u64;
                Ok(bytes_read)
            },
            dest.as_ref(),
        )
    } else {
        do_sendfile(|buf| src.read(buf), dest.as_ref())
    }
    .map(|n| n as _)
}
