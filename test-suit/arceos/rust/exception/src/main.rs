#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]

#[cfg(feature = "axstd")]
extern crate axstd as std;

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

#[cfg(feature = "axstd")]
fn raise_page_fault() {
    use std::os::arceos::modules::axhal;

    use axhal::{mem::VirtAddr, paging::MappingFlags};

    #[linkme::distributed_slice(axhal::trap::PAGE_FAULT)]
    fn page_fault_handler(vaddr: VirtAddr, access_flags: MappingFlags) -> bool {
        println!(
            "Page fault @ {:#x}, access_flags: {:?}",
            vaddr, access_flags
        );
        println!("Page fault test OK!");
        axhal::power::system_off();
    }

    let fault_addr = 0xdeadbeef as *mut u8;
    unsafe {
        *(fault_addr) = 233;
    }
}

#[cfg_attr(feature = "axstd", unsafe(no_mangle))]
fn main() {
    println!("Running exception tests...");
    raise_break_exception();
    #[cfg(feature = "axstd")]
    raise_page_fault();
}
