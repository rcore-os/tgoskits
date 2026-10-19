//! LoongArch EFI address aliases and inherited handoff.

use core::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

pub use uefi::runtime::ResetType;
use uefi::{Handle, Status, system::with_config_table, table};
use uefi_raw::table::system::SystemTable;

use crate::{ArchTrait, arch::Arch, boot_payload, handoff::received::loongarch64 as payload};

#[cfg(feature = "efi-handoff")]
pub(crate) mod handoff;

static INHERITED_EFI_HANDOFF: AtomicBool = AtomicBool::new(false);

pub(super) fn prepare_service_context(_system_table: *const c_void) -> bool {
    let inherited_handoff = inherited_handoff_available();
    #[cfg(feature = "boot-resources")]
    if inherited_handoff {
        let record = payload::inherited_payload().expect("missing inherited resources");
        // SAFETY: boot CPU only, before any allocation or secondary CPU starts.
        unsafe {
            crate::boot_resources::prepare_inherited_plan(record.resource_slice().unwrap())
                .expect("inherited resource plan is not satisfied");
        }
    } else {
        crate::boot_resources::prepare_static_plan();
    }
    #[cfg(feature = "efi-handoff")]
    handoff::prepare_context(_system_table, inherited_handoff);
    inherited_handoff
}

pub(super) fn prepare_entry_payload(image_handle: Handle) {
    let inherited_handoff = inherited_handoff_available();
    INHERITED_EFI_HANDOFF.store(inherited_handoff, Ordering::Release);
    if inherited_handoff {
        load_host_boot_payload();
    } else {
        super::load_firmware_entry_payload(image_handle);
    }
}

pub(super) fn runtime_system_table(system_table: *mut SystemTable) -> *mut SystemTable {
    system_table
}

pub(super) fn reserve_boot_memory() {}

pub(super) fn init_boot_memory_map() {
    payload::restore_inherited_memory_map();
    if !inherited_handoff() {
        crate::fdt::init_memory_map();
    }
}

/// Converts firmware table pointers to the LoongArch direct-map addresses used
/// after the first ArceOS EFI image has enabled its MMU.
pub(super) fn normalize_system_table(system_table: *const c_void) -> *const c_void {
    if Arch::is_mmu_enabled()
        && let Some(physical) = physical_alias(system_table as usize)
    {
        let mapped = Arch::_va(physical).cast_const().cast::<c_void>();
        // SAFETY: `mapped` is the direct-map address of the firmware table
        // supplied by the EFI caller.
        unsafe { table::set_system_table(mapped.cast()) };
        let raw = table::system_table_raw()
            .expect("normalized EFI system table is unavailable")
            .as_ptr();
        // SAFETY: table storage pointers use the same firmware physical alias;
        // function pointers remain firmware addresses and are untouched.
        unsafe {
            (*raw).firmware_vendor = normalize_const_pointer((*raw).firmware_vendor);
            (*raw).stdin = normalize_mut_pointer((*raw).stdin);
            (*raw).stdout = normalize_mut_pointer((*raw).stdout);
            (*raw).stderr = normalize_mut_pointer((*raw).stderr);
            (*raw).runtime_services = normalize_mut_pointer((*raw).runtime_services);
            (*raw).boot_services = normalize_mut_pointer((*raw).boot_services);
            (*raw).configuration_table = normalize_mut_pointer((*raw).configuration_table);
        }
        return mapped;
    }
    system_table
}

fn normalize_mut_pointer<T>(pointer: *mut T) -> *mut T {
    if Arch::is_mmu_enabled()
        && let Some(physical) = physical_alias(pointer as usize)
    {
        Arch::_va(physical).cast()
    } else {
        pointer
    }
}

fn normalize_const_pointer<T>(pointer: *const T) -> *const T {
    normalize_mut_pointer(pointer.cast_mut()).cast_const()
}

pub(crate) fn physical_alias(address: usize) -> Option<usize> {
    const PHYSICAL_ADDRESS_LIMIT: usize = 1 << 48;
    const PHYSICAL_MASK: usize = PHYSICAL_ADDRESS_LIMIT - 1;
    const ALIAS_BASES: [usize; 4] = [
        0x2000_0000_0000_0000,
        0x8000_0000_0000_0000,
        0x9000_0000_0000_0000,
        0xa000_0000_0000_0000,
    ];
    if address < PHYSICAL_ADDRESS_LIMIT {
        Some(address)
    } else if ALIAS_BASES.contains(&(address & !PHYSICAL_MASK)) {
        Some(address & PHYSICAL_MASK)
    } else {
        None
    }
}

pub(super) fn inherited_handoff() -> bool {
    INHERITED_EFI_HANDOFF.load(Ordering::Acquire)
}

fn inherited_handoff_available() -> bool {
    with_config_table(|tables| {
        assert!(
            !tables
                .iter()
                .any(|entry| entry.guid == payload::RETIRED_HANDOFF_GUID),
            "unsupported retired EFI handoff ABI"
        );
        tables
            .iter()
            .any(|entry| entry.guid == payload::HANDOFF_GUID && !entry.address.is_null())
    })
}

fn load_host_boot_payload() {
    let installed = with_config_table(|tables| {
        tables
            .iter()
            .find(|entry| entry.guid == payload::HANDOFF_GUID)
            .map(|entry| entry.address)
    });
    let Some(address) = installed else {
        panic!("missing LoongArch inherited EFI payload");
    };
    assert!(!address.is_null(), "null LoongArch EFI payload table");
    let physical = physical_alias(address as usize).expect("invalid EFI payload address");
    let address = if Arch::is_mmu_enabled() {
        Arch::_va(physical).cast_const().cast::<c_void>()
    } else {
        address
    };
    // SAFETY: the first loader owns this runtime configuration-table allocation.
    // Validate the header before borrowing any variable content.
    let payload = unsafe { crate::handoff::protocol::HandoffRecord::from_ptr(address.cast()) }
        .expect("invalid inherited EFI handoff");
    // SAFETY: early boot CPU, before any producer or SMP.
    unsafe {
        payload::remember_inherited_payload(payload);
    }
    if payload.cmdline_len != 0 && !crate::cmdline::has_handoff_cmdline() {
        crate::cmdline::set_cmdline(payload.cmdline().expect("validated cmdline"));
    }
    if payload.archive.len != 0 {
        let start = usize::try_from(payload.archive.start).expect("archive address overflows");
        let end = usize::try_from(
            payload
                .archive
                .start
                .checked_add(payload.archive.len)
                .expect("archive range overflows"),
        )
        .expect("archive end overflows");
        boot_payload::publish(start, end, false);
    }
}

pub fn reset(reset_type: ResetType, status: Status, data: Option<&[u8]>) -> ! {
    info!("Resetting system via UEFI...");
    let system_table_address = uefi::table::system_table_raw()
        .map(|table| table.as_ptr() as usize)
        .unwrap_or_else(|| panic!("UEFI system table is unavailable during reset"));
    let system_table_address = if Arch::is_mmu_enabled()
        && let Some(physical) = physical_alias(system_table_address)
    {
        Arch::_va(physical) as usize
    } else {
        system_table_address
    };
    // The firmware may leave the runtime-services table at a physical or
    // firmware alias address after `SetVirtualAddressMap`; normalize that
    // storage pointer before reading its function entries.
    let runtime_services_address = unsafe {
        (*(system_table_address as *const uefi_raw::table::system::SystemTable)).runtime_services
            as usize
    };
    let runtime_services_address = if Arch::is_mmu_enabled()
        && let Some(physical) = physical_alias(runtime_services_address)
    {
        Arch::_va(physical) as usize
    } else {
        runtime_services_address
    };
    // SAFETY: the normalized pointer comes from the EFI system table and the
    // runtime-services table remains live after ExitBootServices.
    let runtime_services = unsafe {
        (runtime_services_address as *const uefi_raw::table::runtime::RuntimeServices)
            .as_ref()
            .unwrap_or_else(|| panic!("UEFI runtime services are unavailable during reset"))
    };
    let (data_size, data_ptr) =
        data.map_or((0, core::ptr::null()), |data| (data.len(), data.as_ptr()));
    // LoongArch keeps EFI function pointers in the firmware's active address
    // space. The table pointer was normalized above, but the entry point is
    // deliberately called unchanged; converting it without a firmware ABI
    // guarantee can jump into a different alias.
    // SAFETY: the runtime-services table owns this entry point and the data
    // slice stays alive for the duration of this non-returning call.
    unsafe { (runtime_services.reset_system)(reset_type, status, data_size, data_ptr) }
}
