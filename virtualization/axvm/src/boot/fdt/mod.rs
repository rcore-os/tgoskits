//! Guest device-tree artifact and selected architecture compatibility facade.

use alloc::vec::Vec;

pub use crate::arch::fdt::*;

#[cfg(test)]
#[path = "core/mod.rs"]
pub mod test_core;

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

#[cfg(test)]
fn guest_fdt_policy() -> test_core::GuestFdtPolicy {
    test_core::GuestFdtPolicy {
        patch_runtime: test_runtime_patch,
        patch_provided: test_provided_patch,
        decode_interrupt: |specifier| specifier.first().copied(),
        prepare_host_irq_routes: test_core::forwarded_irq::prepare_aarch64_hybrid_routes,
        enrich_guest_interrupts: test_enrich_guest_interrupts,
    }
}

#[cfg(test)]
fn test_enrich_guest_interrupts(
    config: &mut crate::config::AxVMConfig,
    dtb: &[u8],
) -> crate::AxVmResult {
    if config.interrupt_mode() == axvm_types::VMInterruptMode::Hybrid {
        Ok(())
    } else {
        test_core::parse_vm_interrupt(config, dtb)
    }
}

#[cfg(test)]
fn host_fdt_bootarg() -> usize {
    0
}

#[cfg(test)]
fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_memory_addr::VirtAddr::from(paddr.as_usize())
}

#[cfg(test)]
fn test_runtime_patch(
    fdt: &[u8],
    _vm: &crate::AxVMRef,
    _config: &axvmconfig::AxVMCrateConfig,
) -> crate::AxVmResult<Vec<u8>> {
    Ok(fdt.to_vec())
}

#[cfg(test)]
fn test_provided_patch(
    fdt: &[u8],
    _host_fdt: Option<&[u8]>,
    _config: &axvmconfig::AxVMCrateConfig,
) -> crate::AxVmResult<Vec<u8>> {
    Ok(fdt.to_vec())
}

#[cfg(test)]
mod forwarded_irq_tests;
