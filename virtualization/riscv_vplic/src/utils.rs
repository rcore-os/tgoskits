//! MMIO utility functions.
//!
//! Internal helper functions for performing memory-mapped I/O operations.

use core::result::Result::Ok;

use axdevice_base::AccessWidth;
use axvm_types::HostPhysAddr;

use crate::{VplicResult, host};

/// Performs a volatile MMIO write operation.
pub(crate) fn perform_mmio_write(
    addr: HostPhysAddr,
    width: AccessWidth,
    val: usize,
) -> VplicResult<()> {
    let addr = host::phys_to_virt(addr).as_mut_ptr();

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
