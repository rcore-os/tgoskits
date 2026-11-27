use uefi::boot::{MemoryDescriptor, MemoryType};

use crate::{
    consts::PAGE_SIZE,
    mem::{add_memory_descriptor, page_size},
};

pub fn setup_memory_map<'a>(mems: impl Iterator<Item = &'a MemoryDescriptor>) {
    for memory in mems {
        add_memory_descriptor(classify_memory(memory)).unwrap();
    }
}

#[cfg(target_arch = "riscv64")]
pub(super) fn prepare_fdt_memory_map(
    map: &mut crate::mem::MemoryMap,
    fdt: &fdt_raw::Fdt<'_>,
    kernel: core::ops::Range<usize>,
    firmware_map: &crate::efi_memory_map::RawBootMemoryMap<'_>,
    dtb_layout: core::alloc::Layout,
    cpu_layout: core::alloc::Layout,
) -> Result<crate::mem::PreparedBootMemory, uefi::Status> {
    use ranges_ext::RangeError;
    use uefi::Status;

    use crate::{
        efi_memory_map::covers_initramfs_ram,
        mem::{NoMapError, insert_memory_descriptor},
    };

    map.clear();
    for memory in firmware_map.entries() {
        insert_memory_descriptor(map, classify_memory(&memory)).map_err(|error| match error {
            RangeError::Capacity => Status::OUT_OF_RESOURCES,
            RangeError::Conflict { .. } => Status::COMPROMISED_DATA,
        })?;
    }
    let archive = if crate::boot_payload::staged_uefi().is_none() {
        crate::fdt::initramfs_from_fdt(fdt.clone(), |range| {
            covers_initramfs_ram(firmware_map.entries(), range)
        })
        .map_err(|_| Status::COMPROMISED_DATA)?
    } else {
        None
    };
    crate::mem::check_fdt_boot_map(
        map,
        fdt,
        kernel,
        super::boot_data_reservations(),
        archive,
        |map, excluded, kernel| {
            crate::arch::select_efi_boot_ram(map, excluded, kernel, dtb_layout, cpu_layout)
        },
    )
    .map_err(|error| match error {
        NoMapError::Capacity => Status::OUT_OF_RESOURCES,
        _ => Status::COMPROMISED_DATA,
    })
}

fn classify_memory(memory: &MemoryDescriptor) -> crate::mem::MemoryDescriptor {
    use crate::mem::{MemoryDescriptor as Descriptor, MemoryType as Kind};

    let start = memory.phys_start as usize;
    let size = memory.page_count as usize * page_size();
    match memory.ty {
        MemoryType::CONVENTIONAL
        | MemoryType::BOOT_SERVICES_CODE
        | MemoryType::BOOT_SERVICES_DATA
        | MemoryType::LOADER_CODE
        | MemoryType::LOADER_DATA => Descriptor {
            physical_start: start,
            size_in_bytes: size,
            // Retain complete descriptors backing firmware configuration tables.
            memory_type: if retains_firmware_storage(memory) {
                Kind::Reserved
            } else {
                Kind::Free
            },
        },
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => {
            Descriptor::new_aligned(start, size, Kind::Mmio, PAGE_SIZE)
        }
        _ => Descriptor::new_aligned(start, size, Kind::Reserved, PAGE_SIZE),
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
    // SAFETY: this is the installed firmware table before its addresses are
    // virtualized; only storage pointers are inspected here.
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
