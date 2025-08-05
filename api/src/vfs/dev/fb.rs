use core::{any::Any, slice};

use axdriver::prelude::*;
use axerrno::LinuxError;
use axfs_ng_vfs::{VfsError, VfsResult};
use axhal::mem::virt_to_phys;
use memory_addr::{PhysAddrRange, VirtAddr};
use starry_core::vfs::{DeviceMmap, DeviceOps};

pub struct FrameBuffer {
    base: VirtAddr,
    size: usize,
}
impl FrameBuffer {
    pub fn new() -> Self {
        let info = axdisplay::main_display().info();
        Self {
            base: VirtAddr::from(info.fb_base_vaddr),
            size: info.fb_size,
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.base.as_mut_ptr(), self.size) }
    }
}
impl DeviceOps for FrameBuffer {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let slice = self.as_mut_slice();
        let len = buf
            .len()
            .min((slice.len() as u64).saturating_sub(offset) as usize);
        buf[..len].copy_from_slice(&slice[..len]);
        Ok(len)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let slice = self.as_mut_slice();
        if offset >= slice.len() as u64 {
            return Err(VfsError::ENOSPC);
        }
        let len = buf.len().min(slice.len() - offset as usize);
        slice[..len].copy_from_slice(&buf[..len]);
        if let Err(err) = axdisplay::main_display().flush() {
            warn!("Failed to flush framebuffer: {err:?}");
        }
        Ok(len)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        warn!("ioctl {cmd} {arg}");
        Err(LinuxError::ENOTTY)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mmap(&self) -> DeviceMmap {
        DeviceMmap::Physical(PhysAddrRange::from_start_size(
            virt_to_phys(self.base),
            self.size,
        ))
    }
}
