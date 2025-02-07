#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{get_time_us, sleep};

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    println!("into sleep test!");
    let start = get_time_us();
    println!("current time_usec = {}", start);
    sleep(1);
    let end = get_time_us();
    println!(
        "time_msec = {} after sleeping 1 seconds, delta = {}us!",
        end,
        end - start - 1_000_000,
    );
    println!("simple_sleep passed!");
    0
}
