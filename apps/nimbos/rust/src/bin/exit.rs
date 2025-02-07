#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;
use user_lib::{exit, fork, sched_yield, wait, waitpid};

const MAGIC: i32 = -0x10384;

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    println!("I am the parent. Forking the child...");
    let pid = fork();
    if pid == 0 {
        println!("I am the child.");
        for _ in 0..7 {
            sched_yield();
        }
        exit(MAGIC);
    } else {
        println!("I am the parent, fork a child pid {}", pid);
    }
    println!("I am the parent, waiting now..");
    let mut xstate = 0;
    assert!(waitpid(pid, Some(&mut xstate), 0) == pid);
    assert!(xstate == MAGIC);
    assert!(waitpid(pid, None, 0) < 0);
    assert!(wait(None) <= 0);
    println!("waitpid {} ok.", pid);
    println!("exit passed!");
    0
}
