use axhal::{
    mem::VirtAddr,
    paging::MappingFlags,
    trap::{PAGE_FAULT, register_trap_handler},
};
use axsignal::{SignalInfo, Signo};
use axtask::{TaskExtRef, current};
use linux_raw_sys::general::{RLIMIT_STACK, SI_KERNEL, SIGSEGV};
use starry_api::{do_exit, signal::send_signal_process};
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
    if (axconfig::plat::USER_STACK_TOP - axconfig::plat::USER_STACK_SIZE
        ..axconfig::plat::USER_STACK_TOP)
        .contains(&vaddr.as_usize())
    {
        // Stack extension, check rlimit
        let rlim = &curr.task_ext().process_data().rlim.read()[RLIMIT_STACK];
        let size = axconfig::plat::USER_STACK_TOP - vaddr.as_usize();
        if size as u64 > rlim.current {
            send_signal_process(
                &curr.task_ext().thread.process(),
                SignalInfo::new(Signo::SIGSEGV, SI_KERNEL as _),
            )
            .expect("Failed to send SIGSEGV");
        }
    }
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
