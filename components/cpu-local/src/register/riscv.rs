use super::*;
use crate::RegisterModeV1;

pub(super) fn validate_arch_binding(_binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_current(binding: CpuBindingV1) {
    match binding
        .register_mode()
        .expect("register mode was validated")
    {
        RegisterModeV1::LinuxCurrent => unsafe {
            core::arch::asm!(
                "mv tp, {current}",
                "csrw sscratch, zero",
                current = in(reg) binding.boot_thread,
                options(nostack),
            )
        },
        RegisterModeV1::UnikernelTls => unsafe {
            core::arch::asm!(
                "csrw sscratch, {base}",
                base = in(reg) binding.area_base,
                options(nostack),
            )
        },
    }
}

pub(super) unsafe fn read_current_area_base() -> usize {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        let current_thread: usize;
        unsafe { core::arch::asm!("mv {current}, tp", current = out(reg) current_thread) };
        unsafe { area_base_from_current_thread(current_thread) }
    } else {
        let area_base: usize;
        unsafe { core::arch::asm!("csrr {base}, sscratch", base = out(reg) area_base) };
        area_base
    }
}

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        let current_thread: usize;
        unsafe { core::arch::asm!("mv {current}, tp", current = out(reg) current_thread) };
        current_thread
    } else {
        unsafe { runtime_anchor(area_base) }.current_thread_raw()
    }
}

pub(super) unsafe fn get_task_pointer() -> usize {
    let value: usize;
    unsafe { core::arch::asm!("mv {value}, tp", value = out(reg) value) };
    value
}

pub(super) unsafe fn set_task_pointer(value: usize) {
    unsafe { core::arch::asm!("mv tp, {value}", value = in(reg) value) };
}

unsafe fn area_base_from_current_thread(current_thread: usize) -> usize {
    if current_thread == 0 {
        return 0;
    }
    // SAFETY: LinuxCurrent architecture state may only contain a pinned
    // CurrentThreadHeader published by the scheduler or boot prefix.
    unsafe { &*(current_thread as *const CurrentThreadHeader) }
        .cpu_binding()
        .map_or(0, |binding| binding.area_base())
}
