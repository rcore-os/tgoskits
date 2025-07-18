use alloc::{string::ToString, vec::Vec};
use core::ffi::c_char;

use axerrno::{LinuxError, LinuxResult};
use axhal::context::TrapFrame;
use axtask::current;
use starry_core::{
    mm::{load_user_app, map_trampoline},
    task::AsThread,
};

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
    let proc_data = &curr.as_thread().proc_data;

    if proc_data.proc.threads().len() > 1 {
        // TODO: handle multi-thread case
        error!("sys_execve: multi-thread not supported");
        return Err(LinuxError::EAGAIN);
    }

    let mut aspace = proc_data.aspace.lock();
    aspace.clear();
    map_trampoline(&mut aspace)?;

    let (entry_point, user_stack_base) = load_user_app(&mut aspace, Some(&path), &args, &envs)?;
    drop(aspace);

    let name = path
        .rsplit_once('/')
        .map_or(path.as_str(), |(_, name)| name);
    curr.set_name(name);
    *proc_data.exe_path.write() = path;

    // TODO: fd close-on-exec

    tf.set_ip(entry_point.as_usize());
    tf.set_sp(user_stack_base.as_usize());
    Ok(0)
}
