use uefi::boot::{MemoryDescriptor, MemoryType};

use crate::{
    consts::PAGE_SIZE,
    mem::{add_memory_descriptor, page_size},
};

pub fn setup_memory_map<'a>(mems: impl Iterator<Item = &'a MemoryDescriptor>) {
    for memory in mems {
        let desc = match memory.ty {
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
        add_memory_descriptor(desc).unwrap();
    }
}
