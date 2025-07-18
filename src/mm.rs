use axhal::{
    mem::VirtAddr,
    paging::MappingFlags,
    trap::{PAGE_FAULT, register_trap_handler},
};
use axsignal::{SignalInfo, Signo};
use axtask::current;
use linux_raw_sys::general::{RLIMIT_STACK, SI_KERNEL};
use starry_api::signal::send_signal_to_process;
use starry_core::{
    mm::is_accessing_user_memory,
    task::{AsThread, ProcessData},
};

fn handle_user_page_fault(
    proc_data: &ProcessData,
    vaddr: VirtAddr,
    access_flags: MappingFlags,
) -> bool {
    if (starry_core::config::USER_STACK_TOP - starry_core::config::USER_STACK_SIZE
        ..starry_core::config::USER_STACK_TOP)
        .contains(&vaddr.as_usize())
    {
        // Stack extension, check rlimit
        let rlim = &proc_data.rlim.read()[RLIMIT_STACK];
        let size = starry_core::config::USER_STACK_TOP - vaddr.as_usize();
        if size as u64 > rlim.current {
            return false;
        }
    }
    proc_data
        .aspace
        .lock()
        .handle_page_fault(vaddr, access_flags)
}

#[register_trap_handler(PAGE_FAULT)]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags, is_user: bool) -> bool {
    debug!(
        "Page fault at {:#x}, access_flags: {:#x?}",
        vaddr, access_flags
    );
    if !is_user && !is_accessing_user_memory() {
        return false;
    }

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        return false;
    };

    if !handle_user_page_fault(&thr.proc_data, vaddr, access_flags) {
        warn!("{}: segmentation fault at {:#x}", curr.id_name(), vaddr);
        send_signal_to_process(
            thr.proc_data.proc.pid(),
            Some(SignalInfo::new(Signo::SIGSEGV, SI_KERNEL as _)),
        )
        .expect("Failed to send SIGSEGV");
    }

    true
}
