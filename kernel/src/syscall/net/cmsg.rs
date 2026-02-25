use alloc::{sync::Arc, vec::Vec};

use axerrno::{AxError, AxResult};
use linux_raw_sys::net::{SCM_RIGHTS, SOL_SOCKET, cmsghdr};

use crate::{
    file::{FileLike, get_file_like},
    mm::{UserConstPtr, UserPtr},
};

pub enum CMsg {
    Rights { fds: Vec<Arc<dyn FileLike>> },
}
impl CMsg {
    pub fn parse(hdr: &cmsghdr) -> AxResult<Self> {
        if hdr.cmsg_len < size_of::<cmsghdr>() {
            return Err(AxError::InvalidInput);
        }

        let data =
            UserConstPtr::<u8>::from((hdr as *const cmsghdr as usize) + size_of::<cmsghdr>())
                .get_as_slice(hdr.cmsg_len - size_of::<cmsghdr>())?;
        Ok(match (hdr.cmsg_level as u32, hdr.cmsg_type as u32) {
            (SOL_SOCKET, SCM_RIGHTS) => {
                if data.len() % size_of::<i32>() != 0 {
                    return Err(AxError::InvalidInput);
                }
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
}
impl<'a> CMsgBuilder<'a> {
    pub fn new(msg: UserPtr<cmsghdr>, len: &'a mut usize) -> Self {
        let capacity = *len;
        *len = 0;
        Self {
            hdr: msg,
            len,
            capacity,
        }
    }

    pub fn push(
        &mut self,
        level: u32,
        ty: u32,
        body: impl FnOnce(&mut [u8]) -> AxResult<usize>,
    ) -> AxResult<bool> {
        let Some(body_capacity) = (self.capacity - *self.len).checked_sub(size_of::<cmsghdr>())
        else {
            return Ok(false);
        };

        let hdr = self.hdr.get_as_mut()?;
        hdr.cmsg_level = level as _;
        hdr.cmsg_type = ty as _;

        let data = UserPtr::<u8>::from(self.hdr.address().as_usize() + size_of::<cmsghdr>())
            .get_as_mut_slice(body_capacity)?;
        let body_len = body(data)?;

        let cmsg_len = size_of::<cmsghdr>() + body_len;
        hdr.cmsg_len = cmsg_len;
        self.hdr = UserPtr::from(hdr as *const _ as usize + cmsg_len);
        *self.len += cmsg_len;
        Ok(true)
    }
}
