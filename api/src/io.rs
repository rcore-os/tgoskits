use axerrno::{LinuxError, LinuxResult};
use axio::buf::{Buf, BufMut};
use linux_raw_sys::general::iovec;

use crate::mm::{UserConstPtr, UserPtr};

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

// TODO: make a generic poll implementation here
