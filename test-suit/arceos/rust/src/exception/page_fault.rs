use std::{os::arceos::modules::ax_hal, println};

use ax_hal::{mem::VirtAddr, paging::MappingFlags, trap::page_fault_handler};

#[page_fault_handler]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags) -> bool {
    println!(
        "Page fault @ {:#x}, access_flags: {:?}",
        vaddr, access_flags
    );
    println!("Page fault test OK!");
    ax_hal::power::system_off();
}

pub fn run() -> crate::TestResult {
    println!("exception_page_fault: triggering expected page fault");
    let fault_addr = 0xdeadbeef as *mut u8;
    unsafe {
        *fault_addr = 233;
    }
    Err("page fault handler did not stop the system")
}
