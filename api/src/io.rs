use core::mem::{self, MaybeUninit};

use axerrno::{AxError, AxResult};
use axio::{Buf, BufMut, Read, Write};
use bytemuck::AnyBitPattern;
use starry_vm::{VmPtr, vm_read_slice, vm_write_slice};

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
    pub fn new(iovs: *const IoVec, iovcnt: usize) -> AxResult<Self> {
        if iovcnt > 1024 {
            return Err(AxError::InvalidInput);
        }
        let mut len = 0;
        for i in 0..iovcnt {
            let iov = iovs.wrapping_add(i).vm_read()?;
            if iov.iov_len < 0 {
                return Err(AxError::InvalidInput);
            }
            len += iov.iov_len as usize;
        }
        Ok(Self { iovs, iovcnt, len })
    }

    pub fn read_with(
        self,
        mut f: impl FnMut(*const u8, usize) -> AxResult<usize>,
    ) -> AxResult<usize> {
        let mut count = 0;
        for i in 0..self.iovcnt {
            let iov = self.iovs.wrapping_add(i).vm_read()?;
            if iov.iov_len == 0 {
                continue;
            }
            let read = f(iov.iov_base, iov.iov_len as usize)?;
            if read == 0 {
                break;
            }
            count += read;
        }
        Ok(count)
    }

    pub fn fill_with(
        self,
        mut f: impl FnMut(*mut u8, usize) -> AxResult<usize>,
    ) -> AxResult<usize> {
        let mut count = 0;
        for i in 0..self.iovcnt {
            let iov = self.iovs.wrapping_add(i).vm_read()?;
            if iov.iov_len == 0 {
                continue;
            }
            let written = f(iov.iov_base, iov.iov_len as usize)?;
            if written == 0 {
                break;
            }
            count += written;
        }
        Ok(count)
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
    fn skip_empty(&mut self) -> AxResult<()> {
        while self.start < self.inner.iovcnt {
            let iov = self.inner.iovs.wrapping_add(self.start).vm_read()?;
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
    fn read(&mut self, buf: &mut [u8]) -> AxResult<usize> {
        let mut count = 0;
        loop {
            self.skip_empty()?;
            if self.start >= self.inner.iovcnt {
                break;
            }
            let iov = self.inner.iovs.wrapping_add(self.start).vm_read()?;
            let len = (iov.iov_len as usize - self.offset).min(buf.len() - count);
            if len == 0 {
                break;
            }
            vm_read_slice(iov.iov_base.wrapping_add(self.offset), unsafe {
                mem::transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(&mut buf[count..count + len])
            })?;
            self.offset += len;
            self.inner.len -= len;
            count += len;
        }
        Ok(count)
    }
}

impl Buf for IoVectorBufIo {
    fn remaining(&self) -> usize {
        self.inner.len
    }
}

impl Write for IoVectorBufIo {
    fn write(&mut self, buf: &[u8]) -> AxResult<usize> {
        let mut count = 0;
        loop {
            self.skip_empty()?;
            if self.start >= self.inner.iovcnt {
                break;
            }
            let iov = self.inner.iovs.wrapping_add(self.start).vm_read()?;
            let len = (iov.iov_len as usize - self.offset).min(buf.len() - count);
            if len == 0 {
                break;
            }
            vm_write_slice(
                iov.iov_base.wrapping_add(self.offset),
                &buf[count..count + len],
            )?;
            self.offset += len;
            self.inner.len -= len;
            count += len;
        }
        Ok(count)
    }

    fn flush(&mut self) -> AxResult {
        Ok(())
    }
}

impl BufMut for IoVectorBufIo {
    fn remaining_mut(&self) -> usize {
        self.inner.len
    }
}
