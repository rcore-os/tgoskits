#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{exit, fork, get_time_us, getpid, usleep, wait};

static NUM: usize = 30;

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    for _ in 0..NUM {
        let pid = fork();
        if pid == 0 {
            let current_time = get_time_us();
            let sleep_length =
                (current_time as i32 as isize) * (current_time as i32 as isize) % 1000000 + 1000000;
            println!("pid {} sleep for {} ms", getpid(), sleep_length / 1000);
            usleep(sleep_length as usize);
            println!(
                "pid {} OK! expect sleep {} ms, actual sleep {} ms",
                getpid(),
                sleep_length / 1000,
                (get_time_us() - current_time) / 1000
            );
            exit(0);
        }
    }

    for _ in 0..NUM {
        let mut exit_code = 0;
        assert!(wait(Some(&mut exit_code)) > 0);
        assert_eq!(exit_code, 0);
    }
    assert!(wait(None) < 0);
    println!("forktest2 test passed!");
    0
}
