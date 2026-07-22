use super::*;

pub(super) fn validate_arch_binding(_binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_current(binding: CpuBindingV1) {
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {base}, c13, c0, 3",
            base = in(reg) binding.area_base,
        )
    }
}

pub(super) unsafe fn read_current_area_base() -> usize {
    let area_base: usize;
    unsafe { core::arch::asm!("mrc p15, 0, {base}, c13, c0, 3", base = out(reg) area_base) };
    area_base
}

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    unsafe { runtime_anchor(area_base) }.current_thread_raw()
}

pub(super) unsafe fn get_task_pointer() -> usize {
    0
}

pub(super) unsafe fn set_task_pointer(_value: usize) {}
