//! Generic client facade for the linked [`crate::CpuLocalPlatformV1`] provider.

use crate::{CpuBindingV1, CpuLocalStatus};

/// Returns the linked platform's validated current CPU binding.
pub fn current_cpu_binding() -> Result<CpuBindingV1, CpuLocalStatus> {
    let result = crate::abi::cpu_local_platform_v_1::current_cpu_binding();
    match result.status {
        CpuLocalStatus::Ok => Ok(result.binding),
        status => Err(status),
    }
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
