//! Architecture-neutral guest device-tree artifact.

use std::vec::Vec;

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "loongarch64",
    test
))]
pub(crate) mod device;

#[cfg(test)]
#[doc(hidden)]
pub mod core;
#[cfg(all(not(test), any(target_arch = "aarch64", target_arch = "riscv64")))]
pub(crate) mod core;

/// Guest DTB artifact produced or patched before AxVM owns it.
#[derive(Debug, Clone)]
pub struct GuestDtbImage {
    bytes: Vec<u8>,
}

impl GuestDtbImage {
    /// Wraps finalized guest DTB bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Returns the encoded guest DTB.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn guest_fdt_policy() -> core::GuestFdtPolicy {
    crate::arch::current::guest_fdt_policy()
}

#[cfg(all(test, not(any(target_arch = "aarch64", target_arch = "riscv64"))))]
fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        patch_runtime: test_runtime_patch,
        patch_provided: test_provided_patch,
        decode_interrupt: |specifier| {
            specifier
                .first()
                .copied()
                .map(|source| core::DecodedInterrupt {
                    source,
                    trigger: axdevice_base::InterruptTriggerMode::LevelTriggered,
                })
        },
        resolve_cpu_index: Some,
        host_cpu_count: || usize::BITS as usize,
    }
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn host_fdt_bootarg() -> usize {
    crate::arch::current::host_fdt_bootarg()
}

#[cfg(all(test, not(any(target_arch = "aarch64", target_arch = "riscv64"))))]
fn host_fdt_bootarg() -> usize {
    0
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    crate::arch::current::host_phys_to_virt(paddr)
}

#[cfg(all(test, not(any(target_arch = "aarch64", target_arch = "riscv64"))))]
fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_memory_addr::VirtAddr::from(paddr.as_usize())
}

#[cfg(all(test, not(any(target_arch = "aarch64", target_arch = "riscv64"))))]
fn test_runtime_patch(
    fdt: &[u8],
    _vm: &crate::AxVMRef,
    _config: &axvmconfig::GuestConfig,
) -> crate::AxVmResult<Vec<u8>> {
    Ok(fdt.to_vec())
}

#[cfg(all(test, not(any(target_arch = "aarch64", target_arch = "riscv64"))))]
fn test_provided_patch(
    fdt: &[u8],
    _host_fdt: Option<&[u8]>,
    _config: &axvmconfig::GuestConfig,
) -> crate::AxVmResult<Vec<u8>> {
    Ok(fdt.to_vec())
}
