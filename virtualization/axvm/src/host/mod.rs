//! Narrow host services owned by the AxVM runtime.

pub(crate) mod arceos;
pub(crate) mod paging;
#[cfg(any(test, target_arch = "aarch64"))]
pub(crate) mod percpu_irq;
#[cfg(any(test, target_arch = "aarch64"))]
pub(crate) mod shared_mmio;
pub(crate) mod task;
pub(crate) mod traits;

pub(crate) fn default_host() -> &'static arceos::ArceOsHost {
    arceos::arceos_host()
}

pub(crate) use paging::PagingHandler;
#[cfg(target_arch = "aarch64")]
pub(crate) use traits::HostHardTimerAction;
#[cfg(target_arch = "x86_64")]
pub(crate) use traits::HostTimerAction;
pub(crate) use traits::{HostCpu, HostMemory, HostPlatform, HostTime, HostTimer};

/// Physical host-CPU information required by an AxVM application.
pub mod cpu {
    use super::HostCpu;

    /// Returns the number of physical host CPUs.
    pub fn count() -> usize {
        super::default_host().cpu_count()
    }

    /// Returns the current physical host CPU ID.
    pub fn current_id() -> usize {
        super::default_host().this_cpu_id()
    }
}

/// Shut down host filesystems before their devices are transferred to a guest.
#[cfg(any(feature = "fs", feature = "host-fs"))]
pub fn shutdown_filesystems() -> crate::AxVmResult {
    arceos::shutdown_host_filesystems()
}

/// Register any host interrupt route required by the selected block-passthrough profile.
#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
pub fn register_block_passthrough_irq(vm: &crate::AxVMRef) -> crate::AxVmResult {
    arceos::register_qemu_block_passthrough_irq(vm)
}

/// Other architectures do not require a host block interrupt forwarding route.
#[cfg(all(feature = "host-fs", not(target_arch = "x86_64")))]
pub fn register_block_passthrough_irq(_vm: &crate::AxVMRef) -> crate::AxVmResult {
    Ok(())
}

/// Detach any host block device selected for guest passthrough.
#[cfg(feature = "host-fs")]
pub fn prepare_block_passthrough_device() {
    #[cfg(target_arch = "x86_64")]
    arceos::prepare_qemu_block_passthrough_device();
}
