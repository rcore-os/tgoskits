#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;
extern crate alloc;

mod entry;
mod mm;
mod syscall;

use alloc::vec::Vec;

#[unsafe(no_mangle)]
fn main() {
    // Create a init process
    axprocess::Process::new_init(axtask::current().id().as_u64() as _).build();

    let testcases = option_env!("AX_TESTCASES_LIST")
        .unwrap_or_else(|| "Please specify the testcases list by making user_apps")
        .split(',')
        .filter(|&x| !x.is_empty());

    for testcase in testcases {
        let args = testcase
            .split_ascii_whitespace()
            .map(Into::into)
            .collect::<Vec<_>>();

        let exit_code = entry::run_user_app(&args, &[]);
        info!("User task {} exited with code: {:?}", testcase, exit_code);
    }
}
