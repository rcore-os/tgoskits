#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(any(not(target_os = "none"), feature = "ax-std"))]
macro_rules! app {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(any(not(target_os = "none"), feature = "ax-std")))]
macro_rules! app {
    ($($item:item)*) => {};
}

app! {

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

use core::{arch::asm, hint::black_box, ptr};
#[cfg(feature = "ax-std")]
use std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity},
    modules::ax_hal::percpu::this_cpu_id,
};
use std::thread;

const STACK_SIZE: usize = 64 * 1024;
const WRITE_STRIDE: usize = 64;
const SEARCH_BYTES: usize = STACK_SIZE + 2 * 4096;

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    let cpu_num = thread::available_parallelism().unwrap().get();
    let creator_cpu = current_cpu_id();
    let target_cpu = if cpu_num > 1 {
        (creator_cpu + 1) % cpu_num
    } else {
        creator_cpu
    };
    println!(
        "Triggering task stack guard page: creator_cpu={creator_cpu}, \
         target_cpu={target_cpu}, cpu_num={cpu_num}"
    );

    let _ = thread::Builder::new()
        .name("stack-guard-page-overflow".into())
        .stack_size(STACK_SIZE)
        .spawn(move || hit_guard_page(target_cpu));

    loop {
        thread::yield_now();
    }
}

#[inline(never)]
fn hit_guard_page(target_cpu: usize) {
    pin_current_to_cpu(target_cpu);

    let sp = current_stack_pointer();
    println!(
        "stack guard probe cpu={} sp={:#x}",
        current_cpu_id(),
        sp
    );

    let mut offset = WRITE_STRIDE;
    while offset <= SEARCH_BYTES {
        let addr = sp.wrapping_sub(offset) as *mut u8;
        unsafe {
            ptr::write_volatile(addr, offset as u8);
        }
        black_box(addr);
        offset += WRITE_STRIDE;
    }

    panic!("stack guard page was not hit");
}

#[cfg(feature = "ax-std")]
fn pin_current_to_cpu(cpu_id: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
        "failed to pin stack guard probe to CPU {cpu_id}"
    );

    for _ in 0..256 {
        if this_cpu_id() == cpu_id {
            return;
        }
        thread::yield_now();
    }

    assert_eq!(
        this_cpu_id(),
        cpu_id,
        "stack guard probe did not migrate to CPU {cpu_id}"
    );
}

#[cfg(not(feature = "ax-std"))]
fn pin_current_to_cpu(_cpu_id: usize) {}

#[cfg(feature = "ax-std")]
fn current_cpu_id() -> usize {
    this_cpu_id()
}

#[cfg(not(feature = "ax-std"))]
fn current_cpu_id() -> usize {
    0
}

#[inline(always)]
fn current_stack_pointer() -> usize {
    let sp: usize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("mv {}, sp", out(reg) sp, options(nomem, nostack));

        #[cfg(target_arch = "x86_64")]
        asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack));

        #[cfg(target_arch = "aarch64")]
        asm!("mov {}, sp", out(reg) sp, options(nomem, nostack));

        #[cfg(target_arch = "loongarch64")]
        asm!("move {}, $sp", out(reg) sp, options(nomem, nostack));
    }
    sp
}

}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
