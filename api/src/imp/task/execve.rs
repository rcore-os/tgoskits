use core::ffi::c_char;

use alloc::{string::ToString, vec::Vec};
use axerrno::{LinuxError, LinuxResult};
use axhal::arch::UspaceContext;
use axtask::{TaskExtRef, current};
use macro_rules_attribute::apply;
use starry_core::mm::load_user_app;

use crate::{ptr::UserConstPtr, syscall_instrument};

#[apply(syscall_instrument)]
pub fn sys_execve(
    path: UserConstPtr<c_char>,
    argv: UserConstPtr<usize>,
    envp: UserConstPtr<usize>,
) -> LinuxResult<isize> {
    let path = path.get_as_str()?.to_string();

    let args = argv
        .get_as_null_terminated()?
        .iter()
        .map(|arg| {
            UserConstPtr::<c_char>::from(*arg)
                .get_as_str()
                .map(|s| s.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let envs = envp
        .get_as_null_terminated()?
        .iter()
        .map(|env| {
            UserConstPtr::<c_char>::from(*env)
                .get_as_str()
                .map(|s| s.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    info!(
        "sys_execve: path: {:?}, args: {:?}, envs: {:?}",
        path, args, envs
    );

    let curr = current();
    let curr_ext = curr.task_ext();

    if curr_ext.thread.process().threads().len() > 1 {
        // TODO: handle multi-thread case
        error!("sys_execve: multi-thread not supported");
        return Err(LinuxError::EAGAIN);
    }

    let mut aspace = curr_ext.process_data().aspace.lock();
    aspace.unmap_user_areas()?;
    axhal::arch::flush_tlb(None);

    let (entry_point, user_stack_base) =
        load_user_app(&mut aspace, &args, &envs).map_err(|_| {
            error!("Failed to load app {}", path);
            LinuxError::ENOENT
        })?;
    drop(aspace);

    let name = path
        .rsplit_once('/')
        .map_or(path.as_str(), |(_, name)| name);
    curr.set_name(name);
    *curr_ext.process_data().exe_path.write() = path;

    // TODO: fd close-on-exec

    let uctx = UspaceContext::new(entry_point.as_usize(), user_stack_base, 0);
    unsafe { uctx.enter_uspace(curr.kernel_stack_top().expect("No kernel stack top")) }
}
