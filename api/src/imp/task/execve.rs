use core::ffi::c_char;

use alloc::{string::ToString, vec::Vec};
use axerrno::{LinuxError, LinuxResult};
use axhal::arch::TrapFrame;
use axtask::{TaskExtRef, current};
use starry_core::mm::{load_user_app, map_trampoline};

use crate::ptr::UserConstPtr;

pub fn sys_execve(
    tf: &mut TrapFrame,
    path: UserConstPtr<c_char>,
    argv: UserConstPtr<UserConstPtr<c_char>>,
    envp: UserConstPtr<UserConstPtr<c_char>>,
) -> LinuxResult<isize> {
    let path = path.get_as_str()?.to_string();

    let args = argv
        .get_as_null_terminated()?
        .iter()
        .map(|arg| arg.get_as_str().map(Into::into))
        .collect::<Result<Vec<_>, _>>()?;
    let envs = envp
        .get_as_null_terminated()?
        .iter()
        .map(|env| env.get_as_str().map(Into::into))
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
    map_trampoline(&mut aspace)?;
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

    tf.set_ip(entry_point.as_usize());
    tf.set_sp(user_stack_base.as_usize());
    Ok(0)
}
