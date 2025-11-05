use alloc::{string::ToString, sync::Arc, vec::Vec};
use core::ffi::c_char;

use axerrno::{AxError, AxResult};
use axfs_ng::FS_CONTEXT;
use axhal::uspace::UserContext;
use axtask::current;
use starry_core::{mm::load_user_app, task::AsThread};
use starry_vm::vm_load_until_nul;

use crate::{file::FD_TABLE, mm::vm_load_string};

pub fn sys_execve(
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> AxResult<isize> {
    let path = vm_load_string(path)?;

    let args = vm_load_until_nul(argv)?
        .into_iter()
        .map(vm_load_string)
        .collect::<Result<Vec<_>, _>>()?;

    let envs = vm_load_until_nul(envp)?
        .into_iter()
        .map(vm_load_string)
        .collect::<Result<Vec<_>, _>>()?;

    debug!("sys_execve <= path: {path:?}, args: {args:?}, envs: {envs:?}");

    let curr = current();
    let proc_data = &curr.as_thread().proc_data;

    if proc_data.proc.threads().len() > 1 {
        // TODO: handle multi-thread case
        error!("sys_execve: multi-thread not supported");
        return Err(AxError::WouldBlock);
    }

    let mut aspace = proc_data.aspace.lock();
    let (entry_point, user_stack_base) =
        load_user_app(&mut aspace, Some(path.as_str()), &args, &envs)?;
    drop(aspace);

    let loc = FS_CONTEXT.lock().resolve(&path)?;
    curr.set_name(loc.name());

    *proc_data.exe_path.write() = loc.absolute_path()?.to_string();
    *proc_data.cmdline.write() = Arc::new(args);

    *proc_data.signal.actions.lock() = Default::default();

    // Close CLOEXEC file descriptors
    let mut fd_table = FD_TABLE.write();
    let cloexec_fds = fd_table
        .ids()
        .filter(|it| fd_table.get(*it).unwrap().cloexec)
        .collect::<Vec<_>>();
    for fd in cloexec_fds {
        fd_table.remove(fd);
    }
    drop(fd_table);

    uctx.set_ip(entry_point.as_usize());
    uctx.set_sp(user_stack_base.as_usize());
    Ok(0)
}
