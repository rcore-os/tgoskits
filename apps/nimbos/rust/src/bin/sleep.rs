#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{exit, fork, get_time_us, sleep, waitpid};

fn sleepy() {
    let time: usize = 1;
    for i in 0..5 {
        sleep(1);
        println!("sleep {} x {} seconds.", i + 1, time);
    }
    exit(0);
}

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    let current_time = get_time_us();
    let pid = fork();
    let mut exit_code = 0;
    if pid == 0 {
        sleepy();
    }
    assert!(waitpid(pid, Some(&mut exit_code), 0) == pid && exit_code == 0);
    println!("use {} usecs.", get_time_us() - current_time);
    println!("sleep passed!");
    0
}
