use axhal::{
    mem::VirtAddr,
    paging::MappingFlags,
    trap::{PAGE_FAULT, register_trap_handler},
};
use axtask::{TaskExtRef, current};
use linux_raw_sys::general::SIGSEGV;
use starry_api::do_exit;
use starry_core::mm::is_accessing_user_memory;

#[register_trap_handler(PAGE_FAULT)]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags, is_user: bool) -> bool {
    warn!(
        "Page fault at {:#x}, access_flags: {:#x?}",
        vaddr, access_flags
    );
    if !is_user && !is_accessing_user_memory() {
        return false;
    }

    let curr = current();
    if !curr
        .task_ext()
        .process_data()
        .aspace
        .lock()
        .handle_page_fault(vaddr, access_flags)
    {
        warn!(
            "{} ({:?}): segmentation fault at {:#x}, exit!",
            curr.id_name(),
            curr.task_ext().thread,
            vaddr
        );
        do_exit(SIGSEGV as _, true);
    }
    true
}
