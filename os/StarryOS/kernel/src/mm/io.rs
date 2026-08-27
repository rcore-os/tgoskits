use core::mem::{self, MaybeUninit};

use ax_io::{IoError, IoResult, prelude::*};
use bytemuck::AnyBitPattern;
use starry_vm::{VmError, VmPtr, vm_read_slice, vm_write_slice};

use super::check_access;
use crate::{StarryError, StarryResult};

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern)]
pub struct IoVec {
    pub iov_base: *mut u8,
    pub iov_len: isize,
}

#[derive(Default)]
pub struct IoVectorBuf {
    iovs: *const IoVec,
    iovcnt: usize,
    len: usize,
}

impl IoVectorBuf {
    pub fn new(iovs: *const IoVec, iovcnt: usize) -> StarryResult<Self> {
        if iovcnt > 1024 {
            return Err(StarryError::InvalidInput);
        }
        let mut len = 0usize;
        for i in 0..iovcnt {
            let iov = iovs.wrapping_add(i).vm_read()?;
            if iov.iov_len < 0 {
                return Err(StarryError::InvalidInput);
            }
            let iov_len = iov.iov_len as usize;
            if iov_len > 0 {
                check_access(iov.iov_base as usize, iov_len)
                    .map_err(|_| StarryError::BadAddress)?;
            }
            len = len
                .checked_add(iov_len)
                .filter(|len| *len <= isize::MAX as usize)
                .ok_or(StarryError::InvalidInput)?;
        }
        Ok(Self { iovs, iovcnt, len })
    }

    pub fn into_io(self) -> IoVectorBufIo {
        IoVectorBufIo {
            inner: self,
            start: 0,
            offset: 0,
        }
    }
}

pub struct IoVectorBufIo {
    inner: IoVectorBuf,
    start: usize,
    offset: usize,
}

impl IoVectorBufIo {
    fn skip_empty(&mut self) -> IoResult<()> {
        while self.start < self.inner.iovcnt {
            let iov = self
                .inner
                .iovs
                .wrapping_add(self.start)
                .vm_read()
                .map_err(vm_error_to_io_error)?;
            if iov.iov_len as usize > self.offset {
                break;
            }
            self.offset = 0;
            self.start += 1;
        }
        Ok(())
    }
}

impl Read for IoVectorBufIo {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        let mut count = 0;
        loop {
            self.skip_empty()?;
            if self.start >= self.inner.iovcnt {
                break;
            }
            let iov = self
                .inner
                .iovs
                .wrapping_add(self.start)
                .vm_read()
                .map_err(vm_error_to_io_error)?;
            let len = (iov.iov_len as usize - self.offset).min(buf.len() - count);
            if len == 0 {
                break;
            }
            vm_read_slice(iov.iov_base.wrapping_add(self.offset), unsafe {
                mem::transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(&mut buf[count..count + len])
            })
            .map_err(vm_error_to_io_error)?;
            self.offset += len;
            self.inner.len -= len;
            count += len;
        }
        Ok(count)
    }
}

impl Write for IoVectorBufIo {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let mut count = 0;
        loop {
            self.skip_empty()?;
            if self.start >= self.inner.iovcnt {
                break;
            }
            let iov = self
                .inner
                .iovs
                .wrapping_add(self.start)
                .vm_read()
                .map_err(vm_error_to_io_error)?;
            let len = (iov.iov_len as usize - self.offset).min(buf.len() - count);
            if len == 0 {
                break;
            }
            vm_write_slice(
                iov.iov_base.wrapping_add(self.offset),
                &buf[count..count + len],
            )
            .map_err(vm_error_to_io_error)?;
            self.offset += len;
            self.inner.len -= len;
            count += len;
        }
        Ok(count)
    }

    fn flush(&mut self) -> IoResult {
        Ok(())
    }
}

fn vm_error_to_io_error(error: VmError) -> IoError {
    match error {
        VmError::BadAddress | VmError::AccessDenied => IoError::BadAddress,
        VmError::TooLong => IoError::NameTooLong,
    }
}

impl IoBuf for IoVectorBufIo {
    fn remaining(&self) -> usize {
        self.inner.len
    }
}

impl IoBufMut for IoVectorBufIo {
    fn remaining_mut(&self) -> usize {
        self.inner.len
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn vm_length_error_remains_name_too_long() {
        assert_eq!(vm_error_to_io_error(VmError::TooLong), IoError::NameTooLong);
    }
}
