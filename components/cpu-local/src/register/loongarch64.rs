use super::*;

pub(super) const CURRENT_MODEL: ArchitectureCurrentModel = ArchitectureCurrentModel {
    linux_current: CurrentContextSource::ArchitectureRegister,
    unikernel_tls: CurrentContextSource::RuntimeAnchor,
};

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, boot_context: usize) {
    let shadow = area_base;
    unsafe {
        core::arch::asm!(
            "csrwr {shadow}, 0x33",
            shadow = inout(reg) shadow => _,
            options(nostack),
        );
        core::arch::asm!("move $r21, {base}", base = in(reg) area_base, options(nostack));
    }
    if !cfg!(feature = "tls") {
        unsafe {
            core::arch::asm!("move $tp, {current}", current = in(reg) boot_context, options(nostack))
        };
    }
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
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
    if area_base != shadow {
        super::fatal_register_invariant();
    }
    Ok(area_base)
}

pub(super) unsafe fn read_current_context(area_base: usize) -> usize {
    if cfg!(feature = "tls") {
        unsafe { area_runtime_anchor(area_base) }.current_context_raw()
    } else {
        let current: usize;
        unsafe { core::arch::asm!("move {current}, $tp", current = out(reg) current) };
        current
    }
}

pub(super) unsafe fn write_current_context(value: usize) {
    if !cfg!(feature = "tls") {
        unsafe { core::arch::asm!("move $tp, {value}", value = in(reg) value) };
    }
}

#[cfg(feature = "tls")]
pub(super) unsafe fn read_kernel_tls() -> usize {
    let value: usize;
    unsafe { core::arch::asm!("move {value}, $tp", value = out(reg) value) };
    value
}

#[cfg(feature = "tls")]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    unsafe { core::arch::asm!("move $tp, {value}", value = in(reg) value) };
}

unsafe fn area_runtime_anchor(area_base: usize) -> &'static crate::CpuRuntimeAnchor {
    unsafe {
        &*((area_base + crate::CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const crate::CpuRuntimeAnchor)
    }
}
