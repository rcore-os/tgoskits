#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;
extern crate alloc;
extern crate axstd;

mod ctypes;

mod mm;
mod syscall_imp;
mod task;
use alloc::{string::ToString, sync::Arc, vec};

use axhal::arch::UspaceContext;
use axstd::println;
use axsync::Mutex;
use memory_addr::VirtAddr;

#[unsafe(no_mangle)]
fn main() {
    let testcases = option_env!("AX_TESTCASES_LIST")
        .unwrap_or_else(|| "Please specify the testcases list by making user_apps")
        .split(',')
        .filter(|&x| !x.is_empty());
    println!("#### OS COMP TEST GROUP START basic-musl ####");
    for testcase in testcases {
        println!("Testing {}: ", testcase.split('/').next_back().unwrap());

        let args = vec![testcase.to_string()];
        let mut uspace = axmm::new_user_aspace(
            VirtAddr::from_usize(axconfig::plat::USER_SPACE_BASE),
            axconfig::plat::USER_SPACE_SIZE,
        )
        .expect("Failed to create user address space");
        let (entry_vaddr, ustack_top) = mm::load_user_app(&mut (args.into()), &mut uspace).unwrap();
        let user_task = task::spawn_user_task(
            Arc::new(Mutex::new(uspace)),
            UspaceContext::new(entry_vaddr.into(), ustack_top, 2333),
            0,
        );
        let exit_code = user_task.join();
        info!("User task {} exited with code: {:?}", testcase, exit_code);
    }
    println!("#### OS COMP TEST GROUP END basic-musl ####");
}
