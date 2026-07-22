use super::*;
use crate::{HostLevelV1, RegisterModeV1};

fn live_host_level() -> Option<HostLevelV1> {
    let current_el: usize;
    // CurrentEL, rather than a Cargo feature, selects TPIDR_EL1/TPIDR_EL2.
    unsafe { core::arch::asm!("mrs {value}, CurrentEL", value = out(reg) current_el) };
    match (current_el >> 2) & 0b11 {
        1 => Some(HostLevelV1::Supervisor),
        2 => Some(HostLevelV1::Hypervisor),
        _ => None,
    }
}

pub(super) fn validate_arch_binding(binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    if live_host_level() == binding.host_level() {
        Ok(())
    } else {
        Err(CpuLocalError::HostLevelMismatch)
    }
}

pub(super) unsafe fn install_current(binding: CpuBindingV1) {
    let expected = binding
        .host_level()
        .expect("binding host level was validated");
    match expected {
        HostLevelV1::Supervisor => unsafe {
            core::arch::asm!("msr TPIDR_EL1, {base}", base = in(reg) binding.area_base)
        },
        HostLevelV1::Hypervisor => unsafe {
            core::arch::asm!("msr TPIDR_EL2, {base}", base = in(reg) binding.area_base)
        },
    }
    if binding.register_mode() == Some(RegisterModeV1::LinuxCurrent) {
        unsafe { core::arch::asm!("msr SP_EL0, {current}", current = in(reg) binding.boot_thread) };
    }
}

pub(super) unsafe fn read_current_area_base() -> usize {
    let area_base: usize;
    match live_host_level().unwrap_or_else(|| super::fatal_register_invariant()) {
        HostLevelV1::Supervisor => unsafe {
            core::arch::asm!("mrs {base}, TPIDR_EL1", base = out(reg) area_base)
        },
        HostLevelV1::Hypervisor => unsafe {
            core::arch::asm!("mrs {base}, TPIDR_EL2", base = out(reg) area_base)
        },
    }
    area_base
}

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        let current: usize;
        unsafe { core::arch::asm!("mrs {current}, SP_EL0", current = out(reg) current) };
        current
    } else {
        unsafe { runtime_anchor(area_base) }.current_thread_raw()
    }
}

pub(super) unsafe fn get_task_pointer() -> usize {
    let value: usize;
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        unsafe { core::arch::asm!("mrs {value}, SP_EL0", value = out(reg) value) };
    } else {
        unsafe { core::arch::asm!("mrs {value}, TPIDR_EL0", value = out(reg) value) };
    }
    value
}

pub(super) unsafe fn set_task_pointer(value: usize) {
    if image_register_mode() == RegisterModeV1::LinuxCurrent {
        unsafe { core::arch::asm!("msr SP_EL0, {value}", value = in(reg) value) };
    } else {
        unsafe { core::arch::asm!("msr TPIDR_EL0, {value}", value = in(reg) value) };
    }
}
