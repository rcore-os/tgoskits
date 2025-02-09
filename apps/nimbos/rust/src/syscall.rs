use super::time::{ClockId, TimeSpec};
use crate::arch::syscall;

pub use crate::arch::sys_clone;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        pub const SYSCALL_READ: usize = 0;
        pub const SYSCALL_WRITE: usize = 1;
        pub const SYSCALL_YIELD: usize = 24;
        pub const SYSCALL_GETPID: usize = 39;
        pub const SYSCALL_CLONE: usize = 56;
        pub const SYSCALL_FORK: usize = 57;
        pub const SYSCALL_EXEC: usize = 59;
        pub const SYSCALL_EXIT: usize = 60;
        pub const SYSCALL_WAITPID: usize = 61;
        pub const SYSCALL_CLOCK_GETTIME: usize = 228;
        pub const SYSCALL_CLOCK_NANOSLEEP: usize = 230;
    }
    else {
        pub const SYSCALL_READ: usize = 63;
        pub const SYSCALL_WRITE: usize = 64;
        pub const SYSCALL_YIELD: usize = 124;
        pub const SYSCALL_GETPID: usize = 172;
        #[allow(dead_code)]
        pub const SYSCALL_CLONE: usize = 220;
        pub const SYSCALL_FORK: usize = 220;
        pub const SYSCALL_EXEC: usize = 221;
        pub const SYSCALL_EXIT: usize = 93;
        pub const SYSCALL_WAITPID: usize = 260;
        pub const SYSCALL_CLOCK_GETTIME: usize = 403;
        pub const SYSCALL_CLOCK_NANOSLEEP: usize = 407;
    }
}

pub fn sys_read(fd: usize, buffer: &mut [u8]) -> isize {
    syscall(SYSCALL_READ, [
        fd,
        buffer.as_mut_ptr() as usize,
        buffer.len(),
    ])
}

pub fn sys_write(fd: usize, buffer: &[u8]) -> isize {
    syscall(SYSCALL_WRITE, [fd, buffer.as_ptr() as usize, buffer.len()])
}

pub fn sys_exit(exit_code: i32) -> ! {
    syscall(SYSCALL_EXIT, [exit_code as usize, 0, 0]);
    panic!("sys_exit never returns!");
}

pub fn sys_yield() -> isize {
    syscall(SYSCALL_YIELD, [0, 0, 0])
}

pub fn sys_getpid() -> isize {
    syscall(SYSCALL_GETPID, [0, 0, 0])
}

pub fn sys_fork() -> isize {
    syscall(SYSCALL_FORK, [0, 0, 0])
}

pub fn sys_exec(path: &str) -> isize {
    syscall(SYSCALL_EXEC, [path.as_ptr() as usize, 0, 0])
}

pub fn sys_waitpid(pid: isize, exit_code: *mut i32, options: u32) -> isize {
    syscall(SYSCALL_WAITPID, [
        pid as usize,
        exit_code as _,
        options as _,
    ])
}

pub fn sys_clock_gettime(clk: ClockId, req: &mut TimeSpec) -> isize {
    syscall(SYSCALL_CLOCK_GETTIME, [clk as _, req as *mut _ as usize, 0])
}

pub fn sys_clock_nanosleep(clk: ClockId, flags: u32, req: &TimeSpec) -> isize {
    syscall(SYSCALL_CLOCK_NANOSLEEP, [
        clk as _,
        flags as _,
        req as *const _ as usize,
    ])
}
