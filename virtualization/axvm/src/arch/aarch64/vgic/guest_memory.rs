//! Checked AxVM guest-memory adapter used by the software ITS.

use arm_vgic::{GuestMemory, GuestMemoryError};

use crate::{GuestPhysAddr, VMId};

pub(super) struct AxvmGuestMemory {
    vm_id: VMId,
}

impl AxvmGuestMemory {
    pub(super) const fn new(vm_id: VMId) -> Self {
        Self { vm_id }
    }
}

impl GuestMemory for AxvmGuestMemory {
    fn read(&self, address: u64, destination: &mut [u8]) -> Result<(), GuestMemoryError> {
        let address = usize::try_from(address).map_err(|_| {
            GuestMemoryError::new("read ITS table", "guest address does not fit usize")
        })?;
        let vm = crate::get_vm_by_id(self.vm_id).ok_or_else(|| {
            GuestMemoryError::new("read ITS table", "guest VM is no longer registered")
        })?;
        vm.read_from_guest(GuestPhysAddr::from(address), destination)
            .map_err(|error| GuestMemoryError::new("read ITS table", std::format!("{error}")))
    }
}
