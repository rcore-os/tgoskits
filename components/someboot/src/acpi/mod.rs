use core::{ffi::c_void, ptr::NonNull};

use acpi::AcpiTables;

pub mod cpu;
pub(crate) mod earlycon;
mod handle;
pub mod power;
// pub mod ram;

pub(crate) use handle::AcpiHandle;

use crate::mem::phys_to_virt;

/// RSDP存储
static mut RSDP: usize = 0;

/// 设置RSDP地址
#[allow(unused)]
pub(crate) fn set_rsdp(addr: *const c_void) {
    unsafe {
        RSDP = addr as usize;
    }
}

#[allow(unused)]
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
    unsafe {
        let rsdp = if RSDP == 0 {
            return Err(acpi::AcpiError::NoValidRsdp);
        } else {
            RSDP
        };

        let h = AcpiHandle;
        ::acpi::AcpiTables::from_rsdp(h, rsdp)
    }
}

/// 获取 CPU ID 列表
///
/// 根据目标架构使用不同的解析方式：
/// - x86_64: 解析 MADT 中的 LocalApic/LocalX2Apic 条目
/// - AArch64: 解析 MADT 中的 Gicc 条目
/// - RISC-V 64: 解析 MADT 中的 RINTC 条目
/// - LoongArch64: 解析 MADT 中的 Core PIC 条目
///
/// # Returns
/// - `Some(iterator)`: 成功获取 CPU ID 列表
/// - `None`: 无法获取 MADT 表或没有已启用的 CPU
pub fn cpu_id_list() -> Option<impl Iterator<Item = usize>> {
    cpu::cpu_id_list()
}
