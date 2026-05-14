use core::{any::Any, str};

use ax_errno::AxError;
use axfs_ng_vfs::{NodeFlags, VfsResult};

use crate::pseudofs::DeviceOps;

pub struct DevMem;

const DEVMEM_HIGH_BASE: u64 = 0xffff_ffc0_0000_0000;

impl DevMem {
    fn parse_u64(text: &str) -> Result<u64, AxError> {
        let text = text.trim();
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16).map_err(|_| AxError::InvalidInput)
        } else {
            text.parse::<u64>().map_err(|_| AxError::InvalidInput)
        }
    }

    fn normalize_addr(addr: u64) -> usize {
        if addr < DEVMEM_HIGH_BASE {
            (DEVMEM_HIGH_BASE | addr) as usize
        } else {
            addr as usize
        }
    }
}

impl DeviceOps for DevMem {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() || buf.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(0);
        }
        let input = str::from_utf8(buf).map_err(|_| AxError::InvalidInput)?;
        let mut parts = input.split_whitespace();
        let addr = parts.next().ok_or(AxError::InvalidInput)?;
        let width = parts.next().ok_or(AxError::InvalidInput)?;
        let value = parts.next().ok_or(AxError::InvalidInput)?;
        if parts.next().is_some() {
            return Err(AxError::InvalidInput.into());
        }

        let addr = Self::normalize_addr(Self::parse_u64(addr)?);
        let width = Self::parse_u64(width)? as u32;
        let value = Self::parse_u64(value)?;

        unsafe {
            match width {
                8  => core::ptr::write_volatile(addr as *mut u8,  value as u8),
                16 => core::ptr::write_volatile(addr as *mut u16, value as u16),
                32 => core::ptr::write_volatile(addr as *mut u32, value as u32),
                _  => return Err(AxError::InvalidInput.into()),
            }
        }

        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any { self }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}
