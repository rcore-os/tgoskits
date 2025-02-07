use alloc::string::ToString;
use axerrno::AxResult;
use memory_addr::VirtAddr;

use axhal::{
    paging::MappingFlags,
    trap::{PAGE_FAULT, register_trap_handler},
};

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
    let mut uspace = axmm::new_user_aspace(
        VirtAddr::from_usize(axconfig::plat::USER_SPACE_BASE),
        axconfig::plat::USER_SPACE_SIZE,
    )?;
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

        uspace.write(segement.start_vaddr + segement.offset, segement.data)?;

        // TDOO: flush the I-cache
    }

    // The user stack is divided into two parts:
    // `ustack_start` -> `ustack_pointer`: It is the stack space that users actually read and write.
    // `ustack_pointer` -> `ustack_end`: It is the space that contains the arguments, environment variables and auxv passed to the app.
    //  When the app starts running, the stack pointer points to `ustack_pointer`.
    let ustack_end = VirtAddr::from_usize(axconfig::plat::USER_STACK_TOP);
    let ustack_size = axconfig::plat::USER_STACK_SIZE;
    let ustack_start = ustack_end - ustack_size;
    debug!(
        "Mapping user stack: {:#x?} -> {:#x?}",
        ustack_start, ustack_end
    );
    // FIXME: Add more arguments and environment variables
    let (stack_data, ustack_pointer) = kernel_elf_parser::get_app_stack_region(
        &[app_name.to_string()],
        &[],
        &elf_info.auxv,
        ustack_start,
        ustack_size,
    );
    uspace.map_alloc(
        ustack_start,
        ustack_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        true,
    )?;

    uspace.write(VirtAddr::from_usize(ustack_pointer), stack_data.as_slice())?;
    Ok((elf_info.entry, VirtAddr::from(ustack_pointer), uspace))
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
            warn!(
                "{}: segmentation fault at {:#x}, exit!",
                axtask::current().id_name(),
                vaddr
            );
            axtask::exit(-1);
        }
        true
    } else {
        false
    }
}
