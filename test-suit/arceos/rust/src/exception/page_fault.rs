use ax_hal::{
    mem::VirtAddr,
    trap::{PageFaultFlags, set_page_fault_handler},
};
use ax_std::os::arceos::modules::{ax_hal, ax_runtime::emergency_console};

fn handle_page_fault(vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
    // This handler terminates the machine from exception context. std output
    // can still be queued to the serial worker when power is cut.
    emergency_console::write_fmt(format_args!(
        "Page fault @ {:#x}, access_flags: {:?}\nPage fault test OK!\n",
        vaddr, access_flags
    ));
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
