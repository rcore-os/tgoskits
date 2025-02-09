#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use core::sync::atomic::{AtomicUsize, Ordering};
use user_lib::{getpid, thread_spawn, waitpid};

static GLOBAL_VAR: AtomicUsize = AtomicUsize::new(0);

fn get_sp() -> usize {
    let sp: usize;
    unsafe {
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("mov {}, rsp", out(reg) sp, options(pure, readonly));
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("mov {}, sp", out(reg) sp, options(pure, readonly));
        #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
        core::arch::asm!("mv {}, sp", out(reg) sp, options(pure, readonly));
        #[cfg(target_arch = "loongarch64")]
        core::arch::asm!("move {}, $r3", out(reg) sp, options(pure, readonly));
    }
    sp
}

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    let test_user_thread = |arg| {
        for _ in 0..100 {
            let value = GLOBAL_VAR.fetch_add(100, Ordering::AcqRel);
            println!(
                "test user thread: pid = {:?}, arg = {:#x}, sp = {:#x?}, global_var = {}",
                getpid(),
                arg,
                get_sp(),
                value
            );
        }
        -getpid() as _
    };

    let t0 = thread_spawn(test_user_thread, 0xdead);
    let t1 = thread_spawn(test_user_thread, 0xbeef);
    let mut exit_code = 0;
    waitpid(t0, Some(&mut exit_code), 0);
    println!("thread {} exited with {}.", t0, exit_code);
    waitpid(t1, Some(&mut exit_code), 0);
    println!("thread {} exited with {}.", t1, exit_code);
    println!("main thread exited.");
    0
}
