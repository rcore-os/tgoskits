use alloc::{sync::Arc, vec::Vec};
use core::mem::{offset_of, size_of};

use linux_raw_sys::net::{SCM_RIGHTS, SOL_SOCKET, cmsghdr};

use crate::{
    StarryError, StarryResult,
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

#[derive(Clone)]
pub enum CMsg {
    Rights { fds: Vec<Arc<dyn FileLike>> },
}
impl CMsg {
    pub fn parse(
        current: &crate::task::UserTaskRef,
        hdr_addr: usize,
        hdr: &cmsghdr,
    ) -> crate::StarryResult<Self> {
        if hdr.cmsg_len < size_of::<cmsghdr>() {
            return Err(StarryError::InvalidInput);
        }

        let data_len = hdr.cmsg_len - size_of::<cmsghdr>();
        Ok(match (hdr.cmsg_level as u32, hdr.cmsg_type as u32) {
            (SOL_SOCKET, SCM_RIGHTS) => {
                if !data_len.is_multiple_of(size_of::<i32>())
                    || data_len / size_of::<i32>() > SCM_MAX_FD
                {
                    return Err(crate::StarryError::InvalidInput);
                }
                let data = UserConstPtr::<u8>::from(hdr_addr + size_of::<cmsghdr>())
                    .read_slice(current, data_len)?;
                let mut fds = Vec::new();
                for fd in data.as_chunks::<{ size_of::<i32>() }>().0 {
                    let fd = i32::from_ne_bytes(*fd);
                    if fd < 0 {
                        return Err(StarryError::BadFileDescriptor);
                    }
                    let f = get_file_like(fd)?;
                    fds.push(f);
                }
                Self::Rights { fds }
            }
            _ => {
                return Err(StarryError::InvalidInput);
            }
        })
    }
}

pub struct CMsgBuilder<'task, 'len> {
    current: &'task crate::task::UserTaskRef,
    hdr: UserPtr<cmsghdr>,
    len: &'len mut usize,
    capacity: usize,
    written: usize,
}
impl<'task, 'len> CMsgBuilder<'task, 'len> {
    pub fn new(
        current: &'task crate::task::UserTaskRef,
        msg: UserPtr<cmsghdr>,
        len: &'len mut usize,
    ) -> Self {
        let capacity = *len;
        Self {
            current,
            hdr: msg,
            len,
            capacity,
            written: 0,
        }
    }

    pub fn finish(self) {
        *self.len = self.written;
    }

    /// Number of SCM_RIGHTS fds that still fit in the remaining control space.
    /// Used to deliver as many fds as fit and flag MSG_CTRUNC for the rest,
    /// matching Linux net/core/scm.c scm_detach_fds.
    pub fn rights_capacity(&self) -> usize {
        self.capacity
            .checked_sub(self.written)
            .and_then(|remaining| cmsg_align_down(remaining).checked_sub(size_of::<cmsghdr>()))
            .map_or(0, |body_cap| body_cap / size_of::<i32>())
    }

    pub fn push_sized(
        &mut self,
        level: u32,
        ty: u32,
        body_len: usize,
        body: impl FnOnce(&mut [u8]) -> StarryResult<usize>,
    ) -> StarryResult<bool> {
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
            return Err(StarryError::InvalidInput);
        };
        self.hdr
            .write_field(self.current, offset_of!(cmsghdr, cmsg_len), cmsg_len)?;
        self.hdr
            .write_field(self.current, offset_of!(cmsghdr, cmsg_level), level as i32)?;
        self.hdr
            .write_field(self.current, offset_of!(cmsghdr, cmsg_type), ty as i32)?;
        UserPtr::<u8>::from(hdr_addr + size_of::<cmsghdr>()).write_slice(self.current, &data)?;
        let cmsg_space = cmsg_align(cmsg_len);
        self.hdr = UserPtr::from(hdr_addr + cmsg_space);
        self.written += cmsg_space;
        Ok(true)
    }
}

#[cfg(axtest)]
pub(crate) fn cmsg_alignment_and_space_rules_hold_for_test() -> bool {
    // cmsg_align: rounds up to alignment boundary (usize-aligned).
    let align = size_of::<usize>();
    assert!(cmsg_align(0) == 0);
    assert!(cmsg_align(1) == align);
    assert!(cmsg_align(align) == align);
    assert!(cmsg_align(align + 1) == 2 * align);

    // cmsg_align_down: rounds down to alignment boundary.
    assert!(cmsg_align_down(0) == 0);
    assert!(cmsg_align_down(1) == 0);
    assert!(cmsg_align_down(align) == align);
    assert!(cmsg_align_down(align + 1) == align);

    // cmsg_space: returns Some(len + hdr_size) aligned, or None on overflow.
    let hdr_size = size_of::<cmsghdr>();
    let space0 = cmsg_space(0).unwrap();
    assert!(space0 >= hdr_size && space0 % align == 0);

    // Overflow case: very large len should return None.
    let overflow = cmsg_space(usize::MAX);
    assert!(overflow.is_none());

    true
}
