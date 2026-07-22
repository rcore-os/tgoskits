use super::*;
use crate::{CPU_AREA_CURRENT_THREAD_OFFSET, CPU_AREA_SELF_BASE_OFFSET, RegisterModeV1};

const IA32_GS_BASE: u32 = 0xc000_0101;
const IA32_FS_BASE: u32 = 0xc000_0100;

pub(super) fn validate_arch_binding(_binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_current(binding: CpuBindingV1) {
    let area_base = binding.area_base as u64;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_GS_BASE,
            in("eax") area_base as u32,
            in("edx") (area_base >> 32) as u32,
            options(nostack, preserves_flags),
        );
    }
}

pub(super) unsafe fn read_current_area_base() -> usize {
    let area_base: usize;
    unsafe {
        core::arch::asm!(
            "mov {area_base}, gs:[{self_base_offset}]",
            area_base = out(reg) area_base,
            self_base_offset = const CPU_AREA_SELF_BASE_OFFSET,
            options(nostack, preserves_flags),
        );
    }
    area_base
}

pub(super) unsafe fn read_current_thread(_area_base: usize) -> usize {
    let current_thread: usize;
    unsafe {
        core::arch::asm!(
            "mov {current_thread}, gs:[{current_thread_offset}]",
            current_thread = out(reg) current_thread,
            current_thread_offset = const CPU_AREA_CURRENT_THREAD_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    current_thread
}

pub(super) unsafe fn get_task_pointer() -> usize {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        unsafe { read_current_thread(0) }
    } else {
        let low: u32;
        let high: u32;
        unsafe {
            core::arch::asm!(
                "rdmsr",
                in("ecx") IA32_FS_BASE,
                out("eax") low,
                out("edx") high,
                options(nostack, preserves_flags),
            )
        };
        ((high as usize) << 32) | low as usize
    }
}

pub(super) unsafe fn set_task_pointer(value: usize) {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        unsafe {
            core::arch::asm!(
                "mov gs:[{current_thread_offset}], {value}",
                current_thread_offset = const CPU_AREA_CURRENT_THREAD_OFFSET,
                value = in(reg) value,
                options(nostack, preserves_flags),
            )
        };
    } else {
        let value = value as u64;
        unsafe {
            core::arch::asm!(
                "wrmsr",
                in("ecx") IA32_FS_BASE,
                in("eax") value as u32,
                in("edx") (value >> 32) as u32,
                options(nostack, preserves_flags),
            )
        };
    }
}
