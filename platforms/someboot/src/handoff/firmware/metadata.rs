//! Firmware publication with rollback to a previous configuration table.
use core::{ffi::c_void, ptr::NonNull};

use uefi::boot::{self, MemoryType};

use crate::handoff::{
    HandoffError, HandoffSpec,
    protocol::{HANDOFF_GUID, HandoffRecord},
};

pub(super) struct Publication {
    allocation: NonNull<u8>,
    previous: *mut c_void,
}
impl Publication {
    pub(super) fn install(spec: HandoffSpec<'_>) -> Result<Self, HandoffError> {
        let record = HandoffRecord::for_firmware(spec)?;
        let previous = uefi::system::with_config_table(|tables| {
            tables
                .iter()
                .find(|table| table.guid == HANDOFF_GUID)
                .map_or(core::ptr::null_mut(), |table| table.address.cast_mut())
        });
        let allocation = boot::allocate_pool(
            MemoryType::RUNTIME_SERVICES_DATA,
            size_of::<HandoffRecord>(),
        )
        .map_err(|error| HandoffError::Firmware(error.status().0))?;
        // SAFETY: the EFI pool is aligned, exclusive, and large enough for the record.
        unsafe {
            allocation.cast::<HandoffRecord>().as_ptr().write(record);
        }
        // SAFETY: this guard retains the complete record until publication is revoked.
        if let Err(error) =
            unsafe { boot::install_configuration_table(&HANDOFF_GUID, allocation.as_ptr().cast()) }
        {
            // SAFETY: failed publication never exposed this allocation.
            let _ = unsafe { boot::free_pool(allocation) };
            return Err(HandoffError::Firmware(error.status().0));
        }
        Ok(Self {
            allocation,
            previous,
        })
    }
}
impl Drop for Publication {
    fn drop(&mut self) {
        if !super::boot_services_active() {
            return;
        }
        // SAFETY: the child returned, and this guard owns the installed table.
        if unsafe { boot::install_configuration_table(&HANDOFF_GUID, self.previous) }.is_ok() {
            // SAFETY: the table no longer references this exclusive allocation.
            let _ = unsafe { boot::free_pool(self.allocation) };
        }
        // Retain the allocation if firmware cannot revoke its published pointer.
    }
}
