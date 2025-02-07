#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

static TESTS: &[&str] = &[
    "exit\0",
    "fantastic_text\0",
    "forktest\0",
    "forktest2\0",
    "forktest_simple\0",
    "hello_world\0",
    "matrix\0",
    "sleep\0",
    "sleep_simple\0",
    "stack_overflow\0",
    "yield\0",
    "thread_simple\0",
    "cyclictest\0",
];

use user_lib::{exec, fork, waitpid};

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    for test in TESTS {
        println!("Usertests: Running '{}':", test);
        let pid = fork();
        if pid == 0 {
            if exec(*test) == -1 {
                panic!("usertest '{}' not found!", test);
            } else {
                panic!("unreachable!");
            }
        } else {
            let mut exit_code = 0;
            let wait_pid = waitpid(pid, Some(&mut exit_code), 0);
            assert_eq!(pid, wait_pid);
            let color = if exit_code == 0 { 32 } else { 31 };
            println!(
                "\x1b[{}mUsertests: Test '{}' in Process {} exited with code {}.\x1b[0m",
                color, test, pid, exit_code
            );
        }
    }
    println!("usertests passed!");
    0
}
