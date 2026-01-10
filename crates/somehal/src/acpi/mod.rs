use acpi::AcpiTables;
use core::{ffi::c_void, ptr::NonNull};

pub(crate) mod earlycon;
mod handle;

use crate::mem::phys_to_virt;
pub(crate) use handle::AcpiHandle;

/// RSDP存储
static mut RSDP: usize = 0;

/// 设置RSDP地址
#[allow(unused)]
pub(crate) fn set_rsdp(addr: *const c_void) {
    unsafe {
        RSDP = addr as usize;
    }
}

/// 获取RSDP地址
fn rsdp() -> Option<NonNull<u8>> {
    let rsdp = unsafe { RSDP };
    if rsdp == 0 {
        return None;
    }

    let ptr = phys_to_virt(rsdp);

    NonNull::new(ptr)
}

pub fn tables() -> Result<AcpiTables<AcpiHandle>, acpi::AcpiError> {
    let ptr = rsdp().ok_or(acpi::AcpiError::NoValidRsdp)?;
    let h = AcpiHandle;
    unsafe { ::acpi::AcpiTables::from_rsdp(h, ptr.as_ptr() as usize) }
}
