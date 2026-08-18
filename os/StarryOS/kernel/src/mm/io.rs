use core::mem::{self, MaybeUninit};

use ax_io::{IoError, IoResult, prelude::*};
use bytemuck::{AnyBitPattern, NoUninit};
use starry_vm::VmError;

use super::check_access;
use crate::{
    StarryError,
    mm::{VmPtr, vm_read_slice, vm_write_slice},
    task::UserTaskRef,
};

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern, NoUninit)]
pub struct IoVec {
    pub iov_base: *mut u8,
    pub iov_len: isize,
}

pub struct IoVectorBuf<'task> {
    task: &'task UserTaskRef,
    iovs: *const IoVec,
    iovcnt: usize,
    len: usize,
}

impl<'task> IoVectorBuf<'task> {
    pub fn new(
        task: &'task UserTaskRef,
        iovs: *const IoVec,
        iovcnt: usize,
    ) -> crate::StarryResult<Self> {
        if iovcnt > 1024 {
            return Err(StarryError::InvalidInput);
        }
        let mut len = 0usize;
        for i in 0..iovcnt {
            let iov = iovs.wrapping_add(i).vm_read(task)?;
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
        Ok(Self {
            task,
            iovs,
            iovcnt,
            len,
        })
    }

    pub fn into_io(self) -> IoVectorBufIo<'task> {
        IoVectorBufIo {
            inner: self,
            start: 0,
            offset: 0,
        }
    }
}

pub struct IoVectorBufIo<'task> {
    inner: IoVectorBuf<'task>,
    start: usize,
    offset: usize,
}

impl IoVectorBufIo<'_> {
    fn skip_empty(&mut self) -> IoResult<()> {
        while self.start < self.inner.iovcnt {
            let iov = self
                .inner
                .iovs
                .wrapping_add(self.start)
                .vm_read(self.inner.task)
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

impl Read for IoVectorBufIo<'_> {
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
                .vm_read(self.inner.task)
                .map_err(vm_error_to_io_error)?;
            let len = (iov.iov_len as usize - self.offset).min(buf.len() - count);
            if len == 0 {
                break;
            }
            vm_read_slice(
                self.inner.task,
                iov.iov_base.wrapping_add(self.offset),
                unsafe {
                    mem::transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(
                        &mut buf[count..count + len],
                    )
                },
            )
            .map_err(vm_error_to_io_error)?;
            self.offset += len;
            self.inner.len -= len;
            count += len;
        }
        Ok(count)
    }
}

impl Write for IoVectorBufIo<'_> {
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
                .vm_read(self.inner.task)
                .map_err(vm_error_to_io_error)?;
            let len = (iov.iov_len as usize - self.offset).min(buf.len() - count);
            if len == 0 {
                break;
            }
            vm_write_slice(
                self.inner.task,
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

pub(super) fn vm_error_to_io_error(error: VmError) -> IoError {
    match error {
        VmError::BadAddress | VmError::AccessDenied => IoError::BadAddress,
        VmError::TooLong => IoError::NameTooLong,
    }
}

#[cfg(test)]
pub(crate) fn vm_error_to_io_error_preserves_length_for_test() -> bool {
    vm_error_to_io_error(VmError::TooLong) == IoError::NameTooLong
}

impl IoBuf for IoVectorBufIo<'_> {
    fn remaining(&self) -> usize {
        self.inner.len
    }
}

impl IoBufMut for IoVectorBufIo<'_> {
    fn remaining_mut(&self) -> usize {
        self.inner.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_length_error_remains_name_too_long() {
        assert_eq!(vm_error_to_io_error(VmError::TooLong), IoError::NameTooLong);
    }
}
