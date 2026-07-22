use super::*;
use crate::{CPU_AREA_CURRENT_THREAD_OFFSET, CPU_AREA_SELF_BASE_OFFSET};

const IA32_GS_BASE: u32 = 0xc000_0101;
#[cfg(feature = "tls")]
const IA32_FS_BASE: u32 = 0xc000_0100;

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, _boot_thread: usize) {
    let area_base = area_base as u64;
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

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    let area_base: usize;
    unsafe {
        core::arch::asm!(
            "mov {base}, gs:[{offset}]",
            base = out(reg) area_base,
            offset = const CPU_AREA_SELF_BASE_OFFSET,
            options(nostack, preserves_flags),
        );
    }
    Ok(area_base)
}

pub(super) unsafe fn read_current_thread(_area_base: usize) -> usize {
    let current_thread: usize;
    unsafe {
        core::arch::asm!(
            "mov {current}, gs:[{offset}]",
            current = out(reg) current_thread,
            offset = const CPU_AREA_CURRENT_THREAD_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    current_thread
}

// x86_64 stores current directly in the GS runtime anchor. The shared atomic
// publication is therefore the architecture commit; there is no second task
// pointer register to update.
pub(super) unsafe fn write_current_thread(_value: usize) {}

#[cfg(feature = "tls")]
pub(super) unsafe fn read_kernel_tls() -> usize {
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

#[cfg(feature = "tls")]
pub(super) unsafe fn write_kernel_tls(value: usize) {
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
