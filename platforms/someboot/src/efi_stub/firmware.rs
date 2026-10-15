//! Standard firmware entry path shared by x86_64 and AArch64.

use core::ffi::c_void;

use uefi::Handle;
use uefi_raw::table::system::SystemTable;

pub(super) fn normalize_system_table(system_table: *const c_void) -> *const c_void {
    system_table
}

pub(super) fn prepare_service_context(_system_table: *const c_void) -> bool {
    false
}

pub(super) fn prepare_entry_payload(image_handle: Handle) {
    super::load_firmware_entry_payload(image_handle);
}

pub(super) fn inherited_handoff() -> bool {
    false
}

pub(super) fn runtime_system_table(system_table: *mut SystemTable) -> *mut SystemTable {
    crate::mem::__va(system_table as usize).cast()
}

// Standard firmware boots need no reservations beyond the shared EFI map and archive.
pub(super) fn reserve_boot_memory() {}

pub(super) fn init_boot_memory_map() {
    crate::fdt::init_memory_map();
}
