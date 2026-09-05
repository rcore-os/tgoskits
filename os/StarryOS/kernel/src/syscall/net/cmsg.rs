use alloc::{sync::Arc, vec, vec::Vec};

use linux_raw_sys::net::{SCM_RIGHTS, SOL_SOCKET, cmsghdr};
use starry_vm::vm_write_slice;

use crate::{
    StarryError, StarryResult,
    file::{FileLike, get_file_like, prepare_file_like},
};

pub fn cmsg_space(len: usize) -> Option<usize> {
    let align = size_of::<usize>();
    size_of::<cmsghdr>()
        .checked_add(len)?
        .checked_add(align - 1)
        .map(|length| length & !(align - 1))
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
    written: usize,
}
impl<'a> CMsgBuilder<'a> {
    pub fn new(msg: *mut cmsghdr, len: &'a mut usize) -> Self {
        Self {
            user_buffer: msg.cast(),
            capacity: *len,
            len,
            written: 0,
        }
    }

    pub fn finish(self) {
        *self.len = self.written;
    }

    fn user_address(&self, offset: usize) -> StarryResult<*mut u8> {
        self.user_buffer
            .addr()
            .checked_add(offset)
            .map(|address| self.user_buffer.with_addr(address))
            .ok_or(StarryError::BadAddress)
    }

    fn body_capacity(&self) -> Option<usize> {
        self.capacity
            .checked_sub(self.written)?
            .checked_sub(size_of::<cmsghdr>())
    }

    fn write_header(&self, level: u32, ty: u32, body_len: usize) -> StarryResult<()> {
        let length = size_of::<cmsghdr>()
            .checked_add(body_len)
            .ok_or(StarryError::InvalidInput)?;
        // Linux cmsghdr is a native usize followed by two native i32 fields.
        // Byte copies permit unaligned user control buffers without creating
        // a reference to user memory or exposing struct padding.
        let mut bytes = [0u8; size_of::<cmsghdr>()];
        let word = size_of::<usize>();
        bytes[..word].copy_from_slice(&length.to_ne_bytes());
        bytes[word..word + 4].copy_from_slice(&level.to_ne_bytes());
        bytes[word + 4..word + 8].copy_from_slice(&ty.to_ne_bytes());
        vm_write_slice(self.user_address(self.written)?, &bytes)?;
        Ok(())
    }

    fn advance(&mut self, body_len: usize) -> StarryResult<()> {
        let space = cmsg_space(body_len).ok_or(StarryError::InvalidInput)?;
        self.written += space.min(self.capacity - self.written);
        Ok(())
    }

    /// Copies each reserved fd number before installation, like Linux
    /// scm_recv_one_fd(). Dropping an unsuccessful reservation cannot remove
    /// a concurrently reused descriptor and never leaves an invisible fd.
    pub fn push_rights(
        &mut self,
        fds: Vec<Arc<dyn FileLike>>,
        cloexec: bool,
    ) -> StarryResult<usize> {
        let maximum = self.body_capacity().unwrap_or(0) / size_of::<i32>();
        let mut installed = 0;
        for file in fds.into_iter().take(maximum) {
            let Ok(prepared) = prepare_file_like(file, cloexec) else {
                break;
            };
            let offset = self
                .written
                .checked_add(size_of::<cmsghdr>())
                .and_then(|offset| offset.checked_add(installed * size_of::<i32>()));
            let Some(pointer) = offset.and_then(|offset| self.user_address(offset).ok()) else {
                break;
            };
            if vm_write_slice(pointer, &prepared.fd().to_ne_bytes()).is_err() {
                break;
            }
            prepared.install();
            installed += 1;
        }
        // Linux keeps successfully delivered descriptors even if the later
        // header copy fails; only a complete header advances msg_controllen.
        if installed != 0
            && self
                .write_header(SOL_SOCKET, SCM_RIGHTS, installed * size_of::<i32>())
                .is_ok()
        {
            self.advance(installed * size_of::<i32>())?;
        }
        Ok(installed)
    }

    pub fn push_sized(
        &mut self,
        level: u32,
        ty: u32,
        body_len: usize,
        body: impl FnOnce(&mut [u8]) -> StarryResult<usize>,
    ) -> StarryResult<bool> {
        if self
            .body_capacity()
            .is_none_or(|capacity| capacity < body_len)
        {
            return Ok(false);
        }
        let mut bytes = vec![0; body_len];
        let written = body(&mut bytes)?;
        debug_assert_eq!(written, body_len);
        self.write_header(level, ty, body_len)?;
        let offset = self
            .written
            .checked_add(size_of::<cmsghdr>())
            .ok_or(StarryError::BadAddress)?;
        vm_write_slice(self.user_address(offset)?, &bytes)?;
        self.advance(body_len)?;
        Ok(true)
    }
}

#[cfg(all(test, not(axtest)))]
fn cmsg_alignment_and_space_rules_hold_for_test() -> bool {
    let align = size_of::<usize>();
    // cmsg_space: returns Some(len + hdr_size) aligned, or None on overflow.
    let hdr_size = size_of::<cmsghdr>();
    let space0 = cmsg_space(0).unwrap();
    assert!(space0 >= hdr_size && space0.is_multiple_of(align));

    // Overflow case: very large len should return None.
    let overflow = cmsg_space(usize::MAX);
    assert!(overflow.is_none());
    assert!(cmsg_space(usize::MAX - hdr_size).is_none());

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn cmsg_alignment_and_space_rules_hold() {
        assert!(super::cmsg_alignment_and_space_rules_hold_for_test());
    }
}
