use core::{any::Any, str};

use ax_config::plat::PHYS_VIRT_OFFSET;
use ax_errno::AxError;
use axfs_ng_vfs::{NodeFlags, VfsResult};
use bytemuck::AnyBitPattern;
use starry_vm::VmPtr;

use crate::pseudofs::DeviceOps;

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
    fn parse_u32(text: &str) -> Result<u32, AxError> {
        let text = text.trim();
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            u32::from_str_radix(hex, 16).map_err(|_| AxError::InvalidInput)
        } else {
            text.parse::<u32>().map_err(|_| AxError::InvalidInput)
        }
    }

    fn write_fmux(offset: usize, value: u32) -> VfsResult<()> {
        if offset >= FMUX_SIZE || !offset.is_multiple_of(4) {
            return Err(AxError::InvalidInput);
        }
        let vaddr = FMUX_PBASE + PHYS_VIRT_OFFSET + offset;
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
        let input = str::from_utf8(buf).map_err(|_| AxError::InvalidInput)?;
        let mut parts = input.split_whitespace();
        let offset = Self::parse_u32(parts.next().ok_or(AxError::InvalidInput)?)? as usize;
        let value = Self::parse_u32(parts.next().ok_or(AxError::InvalidInput)?)?;
        if parts.next().is_some() {
            return Err(AxError::InvalidInput);
        }
        Self::write_fmux(offset, value)?;
        Ok(buf.len())
    }

    /// Binary IOCTL interface: `ioctl(fd, PINMUX_SET, &PinmuxOp{offset, value})`
    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        if cmd != PINMUX_SET {
            return Err(AxError::InvalidInput);
        }
        let op: PinmuxOp = (arg as *const PinmuxOp).vm_read()?;
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
