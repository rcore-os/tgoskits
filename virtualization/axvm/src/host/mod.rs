//! Narrow host services owned by the AxVM runtime.

pub(crate) mod arceos;
pub(crate) mod paging;
pub(crate) mod task;
pub(crate) mod traits;

pub(crate) fn default_host() -> &'static arceos::ArceOsHost {
    arceos::arceos_host()
}

pub(crate) use paging::PagingHandler;
pub(crate) use traits::{HostCpu, HostMemory, HostPlatform, HostTime};

/// Physical host-console operations required by an AxVM application.
///
/// Callers must keep a single owner for console input. These operations are
/// intended for task context; they do not provide an IRQ-safe buffering layer.
pub mod console {
    /// Enables or disables physical host-console input interrupts.
    pub fn set_input_irq_enabled(enabled: bool) {
        super::arceos::set_console_input_irq_enabled(enabled);
    }

    /// Reads available bytes from the physical host console.
    pub fn read_bytes(bytes: &mut [u8]) -> usize {
        super::arceos::read_console_bytes(bytes)
    }

    /// Writes bytes to the physical host console.
    pub fn write_bytes(bytes: &[u8]) {
        super::arceos::write_console_bytes(bytes);
    }
}

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

/// x86 host-device handoff required by AxVM's QEMU block passthrough profile.
#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
pub mod x86 {
    use crate::{AxVMRef, AxVmResult};

    /// Resolves and registers the host INTx route for the QEMU block device.
    pub fn register_qemu_block_passthrough_irq(vm: &AxVMRef) -> AxVmResult {
        super::arceos::register_qemu_block_passthrough_irq(vm)
    }

    /// Detaches the QEMU block device from its host driver before guest start.
    pub fn prepare_qemu_block_passthrough_device() {
        super::arceos::prepare_qemu_block_passthrough_device();
    }
}
