#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;
extern crate alloc;
extern crate axruntime;

use alloc::format;

mod entry;
mod mm;
mod syscall;

#[unsafe(no_mangle)]
fn main() {
    // Create a init process
    axprocess::Process::new_init(axtask::current().id().as_u64() as _).build();
    starry_core::vfs::mount_all().expect("Failed to mount vfs");

    if option_env!("AX_TESTCASE") == Some("oscomp") {
        let envs = [format!("ARCH={}", option_env!("ARCH").unwrap_or("unknown"))];

        let init = include_str!("init.sh");
        axfs_ng::FS_CONTEXT
            .lock()
            .write("/init.sh", init.as_bytes())
            .expect("Failed to write init script to disk");

        let args = shlex::split("/musl/busybox sh /init.sh").unwrap();
        info!("Running user task: {:?}", args);
        let exit_code = entry::run_user_app(&args, &envs);
        info!("User task {:?} exited with code: {:?}", args, exit_code);
    } else {
        let testcases = option_env!("AX_TESTCASES_LIST")
            .unwrap_or_else(|| "Please specify the testcases list by making user_apps")
            .split(',');

        for testcase in testcases {
            let Some(args) = shlex::split(testcase) else {
                error!("Failed to parse testcase: {:?}", testcase);
                continue;
            };
            if args.is_empty() {
                continue;
            }
            info!("Running user task: {:?}", args);
            let exit_code = entry::run_user_app(&args, &[]);
            info!("User task {:?} exited with code: {:?}", args, exit_code);
        }
    }
}
