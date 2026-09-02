use alloc::{sync::Arc, vec, vec::Vec};

use linux_raw_sys::net::{SCM_RIGHTS, SOL_SOCKET, cmsghdr};
use starry_vm::vm_write_slice;

use crate::{
    StarryError, StarryResult,
    file::{FileLike, get_file_like},
};

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
    pub fn parse(hdr: &cmsghdr, data: &[u8]) -> StarryResult<Self> {
        if hdr.cmsg_len < size_of::<cmsghdr>() {
            return Err(StarryError::InvalidInput);
        }
        if data.len() != hdr.cmsg_len - size_of::<cmsghdr>() {
            return Err(StarryError::InvalidInput);
        }
        Ok(match (hdr.cmsg_level as u32, hdr.cmsg_type as u32) {
            (SOL_SOCKET, SCM_RIGHTS) => {
                if !data.len().is_multiple_of(size_of::<i32>()) {
                    return Err(StarryError::InvalidInput);
                }
                // Linux caps a single SCM_RIGHTS at SCM_MAX_FD (253) fds;
                // more fails with EINVAL (net/core/scm.c scm_fp_copy).
                if data.len() / size_of::<i32>() > 253 {
                    return Err(StarryError::InvalidInput);
                }
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

pub struct CMsgBuilder<'a> {
    user_buffer: *mut u8,
    len: &'a mut usize,
    capacity: usize,
    data: Vec<u8>,
}
impl<'a> CMsgBuilder<'a> {
    pub fn new(msg: *mut cmsghdr, len: &'a mut usize) -> Self {
        let capacity = *len;
        Self {
            user_buffer: msg.cast(),
            len,
            capacity,
            data: Vec::new(),
        }
    }

    pub fn finish(self) -> StarryResult<()> {
        vm_write_slice(self.user_buffer, &self.data)?;
        *self.len = self.data.len();
        Ok(())
    }

    /// Number of SCM_RIGHTS fds that still fit in the remaining control space.
    /// Used to deliver as many fds as fit and flag MSG_CTRUNC for the rest,
    /// matching Linux net/core/scm.c scm_detach_fds.
    pub fn rights_capacity(&self) -> usize {
        self.capacity
            .checked_sub(self.data.len())
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
            .checked_sub(self.data.len())
            .and_then(|remaining| cmsg_align_down(remaining).checked_sub(size_of::<cmsghdr>()))
        else {
            return Ok(false);
        };
        if body_capacity < body_len {
            return Ok(false);
        }

        let mut body_data = vec![0; body_len];
        let written = body(&mut body_data)?;
        debug_assert_eq!(written, body_len);

        let Some(cmsg_len) = size_of::<cmsghdr>().checked_add(body_len) else {
            return Err(StarryError::InvalidInput);
        };
        let hdr = cmsghdr {
            cmsg_len,
            cmsg_level: level as _,
            cmsg_type: ty as _,
        };
        let cmsg_space = cmsg_align(cmsg_len);
        // SAFETY: `hdr` remains alive for the copy and `cmsghdr` is a C ABI
        // record made only of initialized integer fields.
        self.data.extend_from_slice(unsafe {
            core::slice::from_raw_parts(
                (&hdr as *const cmsghdr).cast::<u8>(),
                size_of::<cmsghdr>(),
            )
        });
        self.data.extend_from_slice(&body_data);
        self.data.resize(self.data.len() + cmsg_space - cmsg_len, 0);
        Ok(true)
    }
}

#[cfg(all(test, not(axtest)))]
fn cmsg_alignment_and_space_rules_hold_for_test() -> bool {
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
    assert!(space0 >= hdr_size && space0.is_multiple_of(align));

    // Overflow case: very large len should return None.
    let overflow = cmsg_space(usize::MAX);
    assert!(overflow.is_none());

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn cmsg_alignment_and_space_rules_hold() {
        assert!(super::cmsg_alignment_and_space_rules_hold_for_test());
    }
}
