#![no_std]
#![feature(linkage)]
#![feature(naked_functions)]

#[macro_use]
pub mod console;

mod arch;
mod lang_items;
mod syscall;
mod time;

pub use time::*;

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub extern "C" fn _start() -> ! {
    exit(main());
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
fn main() -> i32 {
    panic!("Cannot find main!");
}

use syscall::*;

pub fn read(fd: usize, buf: &mut [u8]) -> isize {
    sys_read(fd, buf)
}

pub fn write(fd: usize, buf: &[u8]) -> isize {
    sys_write(fd, buf)
}

pub fn exit(exit_code: i32) -> ! {
    sys_exit(exit_code)
}

pub fn sched_yield() -> isize {
    sys_yield()
}

pub fn getpid() -> isize {
    sys_getpid()
}

pub fn fork() -> isize {
    sys_fork()
}

pub fn exec(path: &str) -> isize {
    sys_exec(path)
}

pub fn waitpid(pid: isize, exit_code: Option<&mut i32>, options: u32) -> isize {
    let exit_code_ptr = exit_code.map(|e| e as _).unwrap_or(core::ptr::null_mut());
    sys_waitpid(pid, exit_code_ptr, options)
}

pub fn wait(exit_code: Option<&mut i32>) -> isize {
    waitpid(-1, exit_code, 0)
}

pub fn thread_spawn(entry: fn(usize) -> i32, arg: usize) -> isize {
    use core::sync::atomic::{AtomicUsize, Ordering};
    const MAX_THREADS: usize = 16;
    const THREAD_STACK_SIZE: usize = 4096 * 4; // 16K
    static mut THREAD_STACKS: [[u8; THREAD_STACK_SIZE]; MAX_THREADS] =
        [[0; THREAD_STACK_SIZE]; MAX_THREADS];
    static THREAD_COUNT: AtomicUsize = AtomicUsize::new(0);

    let thread_id = THREAD_COUNT.fetch_add(1, Ordering::AcqRel);
    let newsp = unsafe { THREAD_STACKS[thread_id].as_ptr_range().end as usize };
    sys_clone(entry, arg, newsp)
}
