#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{fork, getpid};

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    println!("parent start, pid = {}!", getpid());
    let pid = fork();
    if pid == 0 {
        // child process
        println!("hello child process, child pid = {}!", getpid());
        100
    } else {
        // parent process
        println!(
            "hello parent process, parent pid = {}, child pid = {}!",
            getpid(),
            pid
        );
        0
    }
}
