use alloc::string::ToString;
use axerrno::AxResult;
use memory_addr::{MemoryAddr, PageIter4K, VirtAddr, PAGE_SIZE_4K};

use axhal::mem::phys_to_virt;
use axhal::paging::MappingFlags;
use axhal::trap::{register_trap_handler, PAGE_FAULT};
use axmm::AddrSpace;
use axtask::TaskExtRef;

use crate::loader;

/// Load a user app.
///
/// # Returns
/// - The first return value is the entry point of the user app.
/// - The second return value is the top of the user stack.
/// - The third return value is the address space of the user app.
pub fn load_user_app(app_name: &str) -> AxResult<(VirtAddr, VirtAddr, AddrSpace)> {
    let mut uspace = axmm::new_user_aspace()?;
    let elf_info = loader::load_elf(app_name, uspace.base());
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

        let mut segement_data_offset = 0;

        for (idx, vaddr) in
            PageIter4K::new(segement.start_vaddr, segement.start_vaddr + segement.size)
                .expect("Failed to create page iterator")
                .enumerate()
        {
            let (paddr, _, _) = uspace
                .page_table()
                .query(vaddr)
                .unwrap_or_else(|_| panic!("Mapping failed for segment: {:#x?}", vaddr));

            let (start_paddr, mut copied_size) = if idx == 0 {
                // Align the start of the segment to the start of the page
                (paddr + segement.offset, PAGE_SIZE_4K - segement.offset)
            } else {
                (paddr, PAGE_SIZE_4K)
            };

            if copied_size + segement_data_offset > segement.data.len() {
                copied_size = segement.data.len() - segement_data_offset;
            }

            unsafe {
                core::ptr::copy_nonoverlapping(
                    segement.data.as_ptr().add(segement_data_offset),
                    phys_to_virt(start_paddr).as_mut_ptr(),
                    copied_size,
                );
            }
            assert!(uspace.page_table().query(vaddr).is_ok());
            segement_data_offset += copied_size;
            if segement_data_offset >= segement.data.len() {
                break;
            }
        }
        // TDOO: flush the I-cache
    }

    let ustack_base = uspace.end();
    let ustack_size = crate::USER_STACK_SIZE;
    let ustack_top = ustack_base - ustack_size;
    debug!(
        "Mapping user stack: {:#x?} -> {:#x?}",
        ustack_top, ustack_base
    );
    // FIXME: Add more arguments and environment variables
    let (stack_data, ustack_bottom) = elf_parser::get_app_stack_region(
        &[app_name.to_string()],
        &[],
        &elf_info.auxv,
        ustack_top,
        ustack_size,
    );
    uspace.map_alloc(
        ustack_top,
        ustack_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        true,
    )?;
    {
        // Copy the stack data to the user stack
        let ustack_bottom_align = VirtAddr::from_usize(ustack_bottom).align_down_4k();
        let stack_data_offset = ustack_bottom_align - ustack_top;
        // Only copy data which contains args, envs and auxv.
        for (idx, vaddr) in PageIter4K::new(ustack_bottom_align, ustack_base)
            .expect("Failed to create page iterator")
            .enumerate()
        {
            let (paddr, _, _) = uspace.page_table().query(vaddr).unwrap_or_else(|e| {
                panic!("Mapping failed for stack: {:#x?} error: {:?}", vaddr, e)
            });
            unsafe {
                core::ptr::copy_nonoverlapping(
                    stack_data
                        .as_ptr()
                        .add(stack_data_offset + idx * PAGE_SIZE_4K),
                    phys_to_virt(paddr).as_mut_ptr(),
                    PAGE_SIZE_4K,
                );
            }
        }
    }

    Ok((elf_info.entry, VirtAddr::from(ustack_bottom), uspace))
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
