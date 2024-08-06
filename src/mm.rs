use axerrno::AxResult;
use memory_addr::VirtAddr;

use axhal::mem::phys_to_virt;
use axhal::paging::MappingFlags;
use axhal::trap::{register_trap_handler, PAGE_FAULT};
use axmm::AddrSpace;
use axtask::TaskExtRef;

use crate::loader;

pub fn load_user_app(app_name: &str) -> AxResult<(VirtAddr, VirtAddr, AddrSpace)> {
    let elf_info = loader::load_user_app(app_name);

    let mut uspace = axmm::new_user_aspace()?;
    for segement in elf_info.segments {
        debug!(
            "Mapping ELF segment: [{:#x?}, {:#x?}) flags: {:#x?}",
            segement.start_vaddr,
            segement.start_vaddr + segement.size,
            segement.flags
        );
        uspace.map_alloc(segement.start_vaddr, segement.size, segement.flags, true)?;

        if segement.data.is_empty() {
            continue;
        }

        let segement_page_iter = memory_addr::PageIter4K::new(
            segement.start_vaddr,
            segement.start_vaddr + segement.size,
        )
        .expect("Failed to create page iterator");

        let mut segement_data_offset = 0;

        for (idx, vaddr) in segement_page_iter.enumerate() {
            let (paddr, _, _) = uspace
                .page_table()
                .query(vaddr)
                .unwrap_or_else(|_| panic!("Mapping failed for segment: {:#x?}", vaddr));

            let (start_paddr, copied_size) = if idx == 0 {
                // Align the start of the segment to the start of the page
                (
                    paddr + segement.offset,
                    memory_addr::PAGE_SIZE_4K - segement.offset,
                )
            } else {
                (paddr, memory_addr::PAGE_SIZE_4K)
            };

            debug!(
                "Copying segment data: {:#x?} -> {:#x?} size: {:#x?}",
                segement.start_vaddr + segement_data_offset + segement.offset,
                start_paddr,
                copied_size
            );

            unsafe {
                core::ptr::copy_nonoverlapping(
                    segement.data.as_ptr().add(segement_data_offset),
                    phys_to_virt(start_paddr).as_mut_ptr(),
                    copied_size,
                );
            }

            segement_data_offset += copied_size;
            if segement_data_offset >= segement.data.len() {
                break;
            }
        }
        // TDOO: flush the I-cache
    }

    let ustack_top = uspace.end();
    let ustack_vaddr = ustack_top - crate::USER_STACK_SIZE;
    debug!(
        "Mapping user stack: {:#x?} -> {:#x?}",
        ustack_vaddr, ustack_top
    );
    uspace.map_alloc(
        ustack_vaddr,
        crate::USER_STACK_SIZE,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        false,
    )?;
    info!("New user address space: {:#x?}", uspace);
    Ok((elf_info.entry, ustack_top, uspace))
}

#[register_trap_handler(PAGE_FAULT)]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags, is_user: bool) -> bool {
    if is_user {
        if !axtask::current()
            .task_ext()
            .aspace
            .lock()
            .handle_page_fault(vaddr, access_flags)
        {
            warn!("{}: segmentation fault, exit!", axtask::current().id_name());
            axtask::exit(-1);
        }
        true
    } else {
        false
    }
}
