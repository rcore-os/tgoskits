//! VM-backed guest memory accessor for portable VirtIO device models.
//!
//! [`AxvmGuestMemoryAccessor`] implements [`axaddrspace::GuestMemoryAccessor`]
//! by delegating every object and buffer read/write to the owning [`AxVM`]'s
//! locked copy API ([`AxVM::read_from_guest`] / [`AxVM::write_to_guest`] and the
//! typed `_of` variants). It overrides the trait's default accessors so it
//! never dereferences a translated host physical address outside the VM
//! resource lock: that keeps it consistent with dynamic stage-2 map/unmap and
//! avoids the unsafe `HPA == HVA` assumption baked into the trait's default
//! volatile accessors.
//!
//! The accessor owns only a [`Weak<AxVM>`](alloc::sync::Weak), which breaks the
//! `AxVM -> device -> accessor -> AxVM` reference cycle and lets reads/writes
//! fail gracefully once the VM is dropped.

use alloc::sync::{Arc, Weak};

use ax_memory_addr::PhysAddr;
use axaddrspace::{AddrSpaceError, AddrSpaceResult, GuestMemoryAccessor};
use axvm_types::GuestPhysAddr;

use crate::AxVM;

/// Guest memory accessor that copies through the owning VM's locked APIs.
///
/// Cheap to clone; clones share no mutable state. The accessor is safe to use
/// from any host task, including VirtIO data-path workers that are not bound to
/// a vCPU.
#[derive(Clone)]
pub struct AxvmGuestMemoryAccessor {
    vm: Weak<AxVM>,
}

impl AxvmGuestMemoryAccessor {
    /// Creates an accessor that reaches guest memory through `vm`.
    pub fn new(vm: Weak<AxVM>) -> Self {
        Self { vm }
    }

    /// Upgrades the VM reference, mapping a dropped VM to an unmapped error at
    /// the faulting guest address.
    fn upgrade(&self, address: GuestPhysAddr) -> Result<Arc<AxVM>, AddrSpaceError> {
        self.vm
            .upgrade()
            .ok_or(AddrSpaceError::Unmapped { address })
    }

    /// Collapses a VM copy error into a stable address-space error.
    ///
    /// The VM copy APIs report both unmapped addresses and short reads through
    /// the same domain error; both mean "this guest address is not fully
    /// accessible", so they are reported as [`AddrSpaceError::Unmapped`].
    fn copy_error(address: GuestPhysAddr) -> AddrSpaceError {
        AddrSpaceError::Unmapped { address }
    }
}

impl GuestMemoryAccessor for AxvmGuestMemoryAccessor {
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        let vm = self.vm.upgrade()?;
        vm.translate_guest_phys_addr(guest_addr)
    }

    fn read_obj<V: Copy>(&self, guest_addr: GuestPhysAddr) -> AddrSpaceResult<V> {
        let vm = self.upgrade(guest_addr)?;
        vm.read_from_guest_of::<V>(guest_addr)
            .map_err(|_| Self::copy_error(guest_addr))
    }

    fn write_obj<V: Copy>(&self, guest_addr: GuestPhysAddr, val: V) -> AddrSpaceResult {
        let vm = self.upgrade(guest_addr)?;
        vm.write_to_guest_of::<V>(guest_addr, &val)
            .map_err(|_| Self::copy_error(guest_addr))
    }

    fn read_buffer(&self, guest_addr: GuestPhysAddr, buffer: &mut [u8]) -> AddrSpaceResult {
        if buffer.is_empty() {
            return Ok(());
        }
        let vm = self.upgrade(guest_addr)?;
        vm.read_from_guest(guest_addr, buffer)
            .map_err(|_| Self::copy_error(guest_addr))
    }

    fn write_buffer(&self, guest_addr: GuestPhysAddr, buffer: &[u8]) -> AddrSpaceResult {
        if buffer.is_empty() {
            return Ok(());
        }
        let vm = self.upgrade(guest_addr)?;
        vm.write_to_guest(guest_addr, buffer)
            .map_err(|_| Self::copy_error(guest_addr))
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use super::*;

    /// An accessor whose backing VM has been dropped must report every access
    /// as unmapped instead of panicking or dereferencing stale state.
    #[test]
    fn dropped_vm_reports_unmapped_access() {
        let accessor = AxvmGuestMemoryAccessor::new(Weak::<AxVM>::new());
        let addr = GuestPhysAddr::from_usize(0x1000);

        assert!(accessor.translate_and_get_limit(addr).is_none());

        let mut buf = [0u8; 4];
        assert!(matches!(
            accessor.read_buffer(addr, &mut buf),
            Err(AddrSpaceError::Unmapped { .. })
        ));
        assert!(matches!(
            accessor.write_buffer(addr, &[1, 2, 3, 4]),
            Err(AddrSpaceError::Unmapped { .. })
        ));
        assert!(matches!(
            accessor.read_obj::<u32>(addr),
            Err(AddrSpaceError::Unmapped { .. })
        ));
        assert!(matches!(
            accessor.write_obj(addr, 0u32),
            Err(AddrSpaceError::Unmapped { .. })
        ));
    }

    /// Empty buffer accesses must succeed without touching the VM, matching the
    /// trait contract used by zero-length VirtIO payloads.
    #[test]
    fn empty_buffer_access_succeeds_without_vm() {
        let accessor = AxvmGuestMemoryAccessor::new(Weak::<AxVM>::new());
        let addr = GuestPhysAddr::from_usize(0x1000);

        let mut buf: [u8; 0] = [];
        assert!(accessor.read_buffer(addr, &mut buf).is_ok());
        assert!(accessor.write_buffer(addr, &[]).is_ok());
    }
}
