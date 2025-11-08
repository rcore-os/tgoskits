use heapless::Vec;

use crate::mem::MemoryDescriptor;

pub fn setup_memory_map() -> Option<()> {
    let fdt = super::fdt_base()?;
    let mut ram = Vec::<MemoryDescriptor, 32>::new();

    for memory in fdt.memory().flatten() {
        for region in memory.regions().flatten() {
            if ram
                .push(crate::mem::MemoryDescriptor {
                    physical_start: region.address as usize,
                    size_in_bytes: region.size,
                    memory_type: crate::mem::MemoryType::Usable,
                })
                .is_err()
            {
                println!("Warning: memory regions exceed the max supported count");
            }
        }
    }

    Some(())
}
