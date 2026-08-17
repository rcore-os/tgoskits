use core::{any::Any, str};

use ax_hal::mem::{PhysAddr, phys_to_virt};
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use bytemuck::AnyBitPattern;
use starry_vm::VmPtr;

use crate::{StarryError, pseudofs::DeviceOps};

const FMUX_PBASE: usize = 0x0300_1000;
const FMUX_SIZE: usize = 0x1D8;

const PINMUX_SET: u32 = 0x01;

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct PinmuxOp {
    offset: u32,
    value: u32,
}

pub struct PinmuxDev;

impl PinmuxDev {
    fn parse_u32(text: &str) -> VfsResult<u32> {
        let text = text.trim();
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            u32::from_str_radix(hex, 16).map_err(|_| VfsError::InvalidInput)
        } else {
            text.parse::<u32>().map_err(|_| VfsError::InvalidInput)
        }
    }

    fn write_fmux(offset: usize, value: u32) -> VfsResult<()> {
        if offset >= FMUX_SIZE || !offset.is_multiple_of(4) {
            return Err(VfsError::InvalidInput);
        }
        let vaddr = phys_to_virt(PhysAddr::from_usize(FMUX_PBASE + offset)).as_usize();
        unsafe {
            core::ptr::write_volatile(vaddr as *mut u32, value);
        }
        Ok(())
    }
}

impl DeviceOps for PinmuxDev {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    /// Text interface for shell scripts: `"0xOFFSET VALUE"`
    /// e.g. `echo "0x64 2" > /dev/pinmux`
    /// offset is relative to FMUX base (0x0300_1000).
    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() || buf.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(0);
        }
        let input = str::from_utf8(buf).map_err(|_| VfsError::InvalidInput)?;
        let mut parts = input.split_whitespace();
        let offset = Self::parse_u32(parts.next().ok_or(VfsError::InvalidInput)?)? as usize;
        let value = Self::parse_u32(parts.next().ok_or(VfsError::InvalidInput)?)?;
        if parts.next().is_some() {
            return Err(VfsError::InvalidInput);
        }
        Self::write_fmux(offset, value)?;
        Ok(buf.len())
    }

    /// Binary IOCTL interface: `ioctl(fd, PINMUX_SET, &PinmuxOp{offset, value})`
    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        if cmd != PINMUX_SET {
            return Err(VfsError::InvalidInput);
        }
        let op: PinmuxOp = (arg as *const PinmuxOp)
            .vm_read()
            .map_err(StarryError::from)?;
        Self::write_fmux(op.offset as usize, op.value)?;
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}
