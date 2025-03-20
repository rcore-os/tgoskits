#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;
extern crate alloc;

mod ctypes;

mod mm;
mod ptr;
mod syscall_imp;
mod task;

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use axerrno::AxResult;
use axhal::arch::UspaceContext;
use axmm::{AddrSpace, kernel_aspace};
use axsync::Mutex;
use memory_addr::VirtAddr;

fn new_user_aspace_empty() -> AxResult<AddrSpace> {
    AddrSpace::new_empty(
        VirtAddr::from_usize(axconfig::plat::USER_SPACE_BASE),
        axconfig::plat::USER_SPACE_SIZE,
    )
}

/// If the target architecture requires it, the kernel portion of the address
/// space will be copied to the user address space.
fn copy_from_kernel(aspace: &mut AddrSpace) -> AxResult {
    if !cfg!(target_arch = "aarch64") && !cfg!(target_arch = "loongarch64") {
        // ARMv8 (aarch64) and LoongArch64 use separate page tables for user space
        // (aarch64: TTBR0_EL1, LoongArch64: PGDL), so there is no need to copy the
        // kernel portion to the user page table.
        aspace.copy_mappings_from(&kernel_aspace().lock())?;
    }
    Ok(())
}

fn run_user_app(args: &[String], envs: &[String]) -> Option<i32> {
    let mut uspace = new_user_aspace_empty()
        .and_then(|mut it| {
            copy_from_kernel(&mut it)?;
            Ok(it)
        })
        .expect("Failed to create user address space");

    let path = arceos_posix_api::FilePath::new(&args[0]).expect("Invalid file path");
    axfs::api::set_current_dir(path.parent().unwrap()).expect("Failed to set current dir");

    let (entry_vaddr, ustack_top) = mm::load_user_app(&mut uspace, args, envs)
        .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));
    let user_task = task::spawn_user_task(
        Arc::new(Mutex::new(uspace)),
        UspaceContext::new(entry_vaddr.into(), ustack_top, 2333),
        axconfig::plat::USER_HEAP_BASE as _,
    );
    user_task.join()
}

#[unsafe(no_mangle)]
fn main() {
    let testcases = option_env!("AX_TESTCASES_LIST")
        .unwrap_or_else(|| "Please specify the testcases list by making user_apps")
        .split(',')
        .filter(|&x| !x.is_empty());

    for testcase in testcases {
        let args = testcase
            .split_ascii_whitespace()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        let exit_code = run_user_app(&args, &[]);
        info!("User task {} exited with code: {:?}", testcase, exit_code);
    }
}
