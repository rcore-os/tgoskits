//! Generic client facade for the linked [`crate::CpuLocalPlatformV1`] provider.

use crate::{CPU_LOCAL_ABI_VERSION, CpuBindingV1, CpuLocalStatus, image_register_mode};

/// Returns the linked platform's validated current CPU binding.
pub fn current_cpu_binding() -> Result<CpuBindingV1, CpuLocalStatus> {
    let result = crate::abi::cpu_local_platform_v_1::current_cpu_binding();
    match result.status {
        CpuLocalStatus::Ok => validate_current_binding(result.binding),
        status => Err(status),
    }
}

fn validate_current_binding(binding: CpuBindingV1) -> Result<CpuBindingV1, CpuLocalStatus> {
    if binding.abi_version != CPU_LOCAL_ABI_VERSION {
        return Err(CpuLocalStatus::AbiMismatch);
    }
    let register_mode = binding
        .register_mode()
        .ok_or(CpuLocalStatus::InvalidBinding)?;
    if register_mode != image_register_mode() {
        return Err(CpuLocalStatus::AbiMismatch);
    }
    binding.validated().ok_or(CpuLocalStatus::InvalidBinding)
}

/// Reads current-header identity or kernel TLS according to the image mode.
pub fn get_tp() -> usize {
    crate::abi::cpu_local_platform_v_1::get_tp()
}

/// Installs current-header identity or kernel TLS according to the image mode.
///
/// # Safety
///
/// The caller must satisfy [`crate::CpuLocalPlatformV1::set_tp`].
pub unsafe fn set_tp(value: usize) -> Result<(), CpuLocalStatus> {
    match unsafe { crate::abi::cpu_local_platform_v_1::set_tp(value) } {
        CpuLocalStatus::Ok => Ok(()),
        status => Err(status),
    }
}

/// Returns the raw pinned current-thread header, or zero before initialization.
pub fn current_thread() -> usize {
    crate::abi::cpu_local_platform_v_1::current_thread()
}
