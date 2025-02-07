#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;
use user_lib::{getpid, sched_yield};

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    println!("Hello, I am process {}.", getpid());
    for i in 0..5 {
        sched_yield();
        println!("Back in process {}, iteration {}.", getpid(), i);
    }
    println!("yield passed!");
    0
}
