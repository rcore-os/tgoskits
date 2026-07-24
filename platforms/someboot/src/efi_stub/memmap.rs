use uefi::boot::{MemoryDescriptor, MemoryType};

use crate::{
    consts::PAGE_SIZE,
    mem::{add_memory_descriptor, page_size},
};

pub fn setup_memory_map<'a>(mems: impl Iterator<Item = &'a MemoryDescriptor>) {
    for memory in mems {
        let memory_type = match memory.ty {
            MemoryType::CONVENTIONAL
            | MemoryType::BOOT_SERVICES_CODE
            | MemoryType::BOOT_SERVICES_DATA
            | MemoryType::LOADER_CODE
            | MemoryType::LOADER_DATA => crate::mem::MemoryType::Free,
            MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => crate::mem::MemoryType::Mmio,
            _ => crate::mem::MemoryType::Reserved,
        };
        let size = usize::try_from(memory.page_count)
            .ok()
            .and_then(|pages| pages.checked_mul(page_size()))
            .expect("UEFI memory descriptor size must fit the address space");
        let desc = crate::mem::MemoryDescriptor::new_aligned(
            usize::try_from(memory.phys_start)
                .expect("UEFI physical address must fit the address space"),
            size,
            memory_type,
            PAGE_SIZE,
        )
        .expect("UEFI memory descriptor must have a valid aligned range");
        add_memory_descriptor(desc).unwrap();
    }
}
