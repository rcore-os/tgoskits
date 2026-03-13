//! MMIO utility functions.
//!
//! Internal helper functions for performing memory-mapped I/O operations.

use axaddrspace::{device::AccessWidth, HostPhysAddr};
use axerrno::AxResult;
use core::result::Result::Ok;

/// Performs a volatile MMIO read operation.
pub(crate) fn perform_mmio_read(addr: HostPhysAddr, width: AccessWidth) -> AxResult<usize> {
    let addr = axvisor_api::memory::phys_to_virt(addr).as_ptr();

    match width {
        AccessWidth::Byte => Ok(unsafe { addr.read_volatile() as _ }),
        AccessWidth::Word => Ok(unsafe { (addr as *const u16).read_volatile() as _ }),
        AccessWidth::Dword => Ok(unsafe { (addr as *const u32).read_volatile() as _ }),
        AccessWidth::Qword => Ok(unsafe { (addr as *const u64).read_volatile() as _ }),
    }
}

/// Performs a volatile MMIO write operation.
pub(crate) fn perform_mmio_write(
    addr: HostPhysAddr,
    width: AccessWidth,
    val: usize,
) -> AxResult<()> {
    let addr = axvisor_api::memory::phys_to_virt(addr).as_mut_ptr();

    match width {
        AccessWidth::Byte => unsafe {
            addr.write_volatile(val as _);
        },
        AccessWidth::Word => unsafe {
            (addr as *mut u16).write_volatile(val as _);
        },
        AccessWidth::Dword => unsafe {
            (addr as *mut u32).write_volatile(val as _);
        },
        AccessWidth::Qword => unsafe {
            (addr as *mut u64).write_volatile(val as _);
        },
    }

    Ok(())
}
