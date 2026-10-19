use uefi::boot::{MemoryDescriptor, MemoryType};

use crate::{
    consts::PAGE_SIZE,
    mem::{add_memory_descriptor, page_size},
};

pub fn setup_memory_map<'a>(mems: impl Iterator<Item = &'a MemoryDescriptor>) {
    for memory in mems {
        let mut desc = match memory.ty {
            MemoryType::CONVENTIONAL
            | MemoryType::BOOT_SERVICES_CODE
            | MemoryType::BOOT_SERVICES_DATA
            | MemoryType::LOADER_CODE
            | MemoryType::LOADER_DATA => crate::mem::MemoryDescriptor {
                physical_start: memory.phys_start as _,
                size_in_bytes: memory.page_count as usize * page_size(),
                memory_type: crate::mem::MemoryType::Free,
            },
            MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => {
                crate::mem::MemoryDescriptor::new_aligned(
                    memory.phys_start as _,
                    memory.page_count as usize * page_size(),
                    crate::mem::MemoryType::Mmio,
                    PAGE_SIZE,
                )
            }
            _ => crate::mem::MemoryDescriptor::new_aligned(
                memory.phys_start as _,
                memory.page_count as usize * page_size(),
                crate::mem::MemoryType::Reserved,
                PAGE_SIZE,
            ),
        };
        // Config entries have no common payload size. Preserve their containing
        // firmware descriptors, including boot-services allocations, rather
        // than guessing a header length and freeing reachable trailing bytes.
        if desc.memory_type == crate::mem::MemoryType::Free && retains_firmware_storage(memory) {
            desc.memory_type = crate::mem::MemoryType::Reserved;
        }
        add_memory_descriptor(desc).unwrap();
    }
}

fn retains_firmware_storage(memory: &MemoryDescriptor) -> bool {
    let Some(system) = uefi::table::system_table_raw() else {
        return false;
    };
    let normalize = |address: usize| crate::mem::virt_to_phys(address as *const u8) as u64;
    let overlaps = |address: usize, bytes: usize| {
        let physical = normalize(address);
        let end = physical
            .checked_add(bytes as u64)
            .expect("EFI table range overflows");
        let memory_end = memory
            .phys_start
            .checked_add(
                memory
                    .page_count
                    .checked_mul(4096)
                    .expect("EFI map length overflows"),
            )
            .expect("EFI descriptor overflows");
        address != 0 && physical < memory_end && memory.phys_start < end
    };
    let contains = |address: usize| overlaps(address, 1);
    if overlaps(
        system.as_ptr() as usize,
        core::mem::size_of::<uefi_raw::table::system::SystemTable>(),
    ) {
        return true;
    }
    // SAFETY: this is the installed firmware table, retained after EBS; only
    // storage pointers are inspected and no firmware function is called.
    let system = unsafe { system.as_ref() };
    if [
        system.firmware_vendor as usize,
        system.runtime_services as usize,
        system.configuration_table as usize,
    ]
    .into_iter()
    .any(contains)
    {
        return true;
    }
    let table_bytes = system
        .number_of_configuration_table_entries
        .checked_mul(core::mem::size_of::<
            uefi_raw::table::configuration::ConfigurationTable,
        >())
        .expect("EFI configuration array overflows");
    if overlaps(system.configuration_table as usize, table_bytes) {
        return true;
    }
    // SAFETY: EFI owns this table array. Its full backing is retained above and
    // it remains readable while this final firmware map is copied.
    if system.number_of_configuration_table_entries == 0 {
        return false;
    }
    assert!(
        !system.configuration_table.is_null(),
        "invalid EFI configuration table"
    );
    let entries = unsafe {
        core::slice::from_raw_parts(
            system.configuration_table,
            system.number_of_configuration_table_entries,
        )
    };
    entries.iter().any(|entry| {
        let address = entry.vendor_table as usize;
        if address == 0 {
            return false;
        }
        let bytes = if entry.vendor_guid == super::FDT_TABLE_GUID {
            let pointer = crate::mem::phys_to_virt(normalize(address) as usize);
            // SAFETY: EFI supplies the trusted FDT allocation; entry discovery
            // already validates its header before EBS. Keep its entire extent,
            // even if firmware splits its backing into multiple descriptors.
            unsafe { fdt_raw::Header::from_ptr(pointer) }
                .map_or(1, |header| header.totalsize as usize)
        } else if entry.vendor_guid == crate::handoff::protocol::HANDOFF_GUID {
            core::mem::size_of::<crate::handoff::protocol::HandoffRecord>()
        } else {
            // Other table formats define their own persistent memory types.
            // Runtime, ACPI and NVS descriptors remain reserved independently;
            // additionally retain the descriptor containing every opaque root.
            1
        };
        overlaps(address, bytes)
    })
}
