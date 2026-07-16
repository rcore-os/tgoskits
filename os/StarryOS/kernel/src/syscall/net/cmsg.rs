use alloc::{sync::Arc, vec::Vec};
use core::mem::{offset_of, size_of};

use ax_errno::{AxError, AxResult};
use linux_raw_sys::net::{SCM_RIGHTS, SOL_SOCKET, cmsghdr};

use crate::{
    file::{FileLike, get_file_like},
    mm::{UserConstPtr, UserPtr},
};

// Linux limits one SCM_RIGHTS control message to SCM_MAX_FD descriptors.
const SCM_MAX_FD: usize = 253;

fn cmsg_align(len: usize) -> usize {
    let align = size_of::<usize>();
    (len + align - 1) & !(align - 1)
}

fn cmsg_align_down(len: usize) -> usize {
    let align = size_of::<usize>();
    len & !(align - 1)
}

pub fn cmsg_space(len: usize) -> Option<usize> {
    size_of::<cmsghdr>().checked_add(len).map(cmsg_align)
}

pub enum CMsg {
    Rights { fds: Vec<Arc<dyn FileLike>> },
}
impl CMsg {
    pub fn parse(hdr_addr: usize, hdr: &cmsghdr) -> AxResult<Self> {
        if hdr.cmsg_len < size_of::<cmsghdr>() {
            return Err(AxError::InvalidInput);
        }

        let data_len = hdr.cmsg_len - size_of::<cmsghdr>();
        Ok(match (hdr.cmsg_level as u32, hdr.cmsg_type as u32) {
            (SOL_SOCKET, SCM_RIGHTS) => {
                if !data_len.is_multiple_of(size_of::<i32>())
                    || data_len / size_of::<i32>() > SCM_MAX_FD
                {
                    return Err(AxError::InvalidInput);
                }
                let data = UserConstPtr::<u8>::from(hdr_addr + size_of::<cmsghdr>())
                    .read_slice(data_len)?;
                let mut fds = Vec::new();
                for fd in data.chunks_exact(size_of::<i32>()) {
                    let fd = i32::from_ne_bytes(fd.try_into().unwrap());
                    if fd < 0 {
                        return Err(AxError::BadFileDescriptor);
                    }
                    let f = get_file_like(fd)?;
                    fds.push(f);
                }
                Self::Rights { fds }
            }
            _ => {
                return Err(AxError::InvalidInput);
            }
        })
    }
}

pub struct CMsgBuilder<'a> {
    hdr: UserPtr<cmsghdr>,
    len: &'a mut usize,
    capacity: usize,
    written: usize,
}
impl<'a> CMsgBuilder<'a> {
    pub fn new(msg: UserPtr<cmsghdr>, len: &'a mut usize) -> Self {
        let capacity = *len;
        Self {
            hdr: msg,
            len,
            capacity,
            written: 0,
        }
    }

    pub fn finish(self) {
        *self.len = self.written;
    }

    pub fn push_sized(
        &mut self,
        level: u32,
        ty: u32,
        body_len: usize,
        body: impl FnOnce(&mut [u8]) -> AxResult<usize>,
    ) -> AxResult<bool> {
        let Some(body_capacity) = self
            .capacity
            .checked_sub(self.written)
            .and_then(|remaining| cmsg_align_down(remaining).checked_sub(size_of::<cmsghdr>()))
        else {
            return Ok(false);
        };
        if body_capacity < body_len {
            return Ok(false);
        }

        let hdr_addr = self.hdr.address().as_usize();
        let mut data = alloc::vec![0; body_len];
        let written = body(&mut data)?;
        debug_assert_eq!(written, body_len);

        let Some(cmsg_len) = size_of::<cmsghdr>().checked_add(body_len) else {
            return Err(AxError::InvalidInput);
        };
        self.hdr
            .write_field(offset_of!(cmsghdr, cmsg_len), cmsg_len)?;
        self.hdr
            .write_field(offset_of!(cmsghdr, cmsg_level), level as i32)?;
        self.hdr
            .write_field(offset_of!(cmsghdr, cmsg_type), ty as i32)?;
        UserPtr::<u8>::from(hdr_addr + size_of::<cmsghdr>()).write_slice(&data)?;
        let cmsg_space = cmsg_align(cmsg_len);
        self.hdr = UserPtr::from(hdr_addr + cmsg_space);
        self.written += cmsg_space;
        Ok(true)
    }
}
