use std::{
    io::{self, Write},
    os::arceos::modules::ax_hal,
    println,
};

use ax_hal::{
    mem::VirtAddr,
    trap::{PageFaultFlags, set_page_fault_handler},
};

fn handle_page_fault(vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
    println!(
        "Page fault @ {:#x}, access_flags: {:?}",
        vaddr, access_flags
    );
    println!("Page fault test OK!");
    io::stdout()
        .flush()
        .expect("failed to flush page fault test output");
    ax_hal::power::system_off();
}

pub fn run() -> crate::TestResult {
    set_page_fault_handler(handle_page_fault);
    println!("exception_page_fault: triggering expected page fault");
    let fault_addr = 0xdeadbeef as *mut u8;
    unsafe {
        *fault_addr = 233;
    }
    Err("page fault handler did not stop the system")
}
