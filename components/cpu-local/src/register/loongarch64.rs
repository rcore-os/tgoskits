use super::*;
use crate::RegisterModeV1;

pub(super) fn validate_arch_binding(_binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_current(binding: CpuBindingV1) {
    let area_base = binding.area_base;
    let shadow = area_base;
    unsafe {
        core::arch::asm!(
            "csrwr {shadow}, 0x33",
            shadow = inout(reg) shadow => _,
            options(nostack),
        );
        core::arch::asm!("move $r21, {base}", base = in(reg) area_base, options(nostack));
    }
    if binding.register_mode() == Some(RegisterModeV1::LinuxCurrent) {
        unsafe {
            core::arch::asm!(
                "move $tp, {current}",
                current = in(reg) binding.boot_thread,
                options(nostack),
            )
        };
    }
}

pub(super) unsafe fn read_current_area_base() -> usize {
    let area_base: usize;
    let shadow: usize;
    unsafe {
        core::arch::asm!(
            "move {base}, $r21",
            "csrrd {shadow}, 0x33",
            base = out(reg) area_base,
            shadow = out(reg) shadow,
            options(nostack),
        )
    };
    assert_eq!(area_base, shadow, "LoongArch live r21 differs from KS3");
    area_base
}

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        let current_thread: usize;
        unsafe { core::arch::asm!("move {current}, $tp", current = out(reg) current_thread) };
        current_thread
    } else {
        unsafe { runtime_anchor(area_base) }.current_thread_raw()
    }
}

pub(super) unsafe fn get_task_pointer() -> usize {
    let value: usize;
    unsafe { core::arch::asm!("move {value}, $tp", value = out(reg) value) };
    value
}

pub(super) unsafe fn set_task_pointer(value: usize) {
    unsafe { core::arch::asm!("move $tp, {value}", value = in(reg) value) };
}
