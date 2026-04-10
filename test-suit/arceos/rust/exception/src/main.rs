#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

use std::{arch::asm, println};

fn raise_break_exception() {
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!("int3");
        #[cfg(target_arch = "aarch64")]
        asm!("brk #0");
        #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
        asm!("ebreak");
        #[cfg(target_arch = "loongarch64")]
        asm!("break 0");
    }
    println!("Breakpoint test OK!");
}

#[cfg(feature = "ax-std")]
fn raise_page_fault() {
    use std::os::arceos::modules::ax_hal;

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

    let fault_addr = 0xdeadbeef as *mut u8;
    unsafe {
        *(fault_addr) = 233;
    }
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Running exception tests...");
    raise_break_exception();
    #[cfg(feature = "ax-std")]
    raise_page_fault();
}
