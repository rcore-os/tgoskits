use core::cell::Cell;

use super::*;
use crate::RegisterModeV1;

std::thread_local! {
    static CURRENT_BINDING: Cell<Option<CpuBindingV1>> = const { Cell::new(None) };
    static TASK_POINTER: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn validate_arch_binding(_binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_current(binding: CpuBindingV1) {
    CURRENT_BINDING.set(Some(binding));
}

pub(super) unsafe fn read_current_area_base() -> usize {
    CURRENT_BINDING.get().map_or(0, |binding| binding.area_base)
}

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    unsafe { runtime_anchor(area_base) }.current_thread_raw()
}

pub(super) unsafe fn get_task_pointer() -> usize {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        let area_base = unsafe { read_current_area_base() };
        unsafe { runtime_anchor(area_base) }.current_thread_raw()
    } else {
        TASK_POINTER.get()
    }
}

pub(super) unsafe fn set_task_pointer(value: usize) {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        let area_base = unsafe { read_current_area_base() };
        unsafe { runtime_anchor(area_base).publish_current_thread_raw(value) };
    } else {
        TASK_POINTER.set(value);
    }
}
