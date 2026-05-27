use ax_errno::{AxError, AxResult};
use ax_task::current;

use crate::task::AsThread;
use linux_raw_sys::general::{
    CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER, CLONE_NEWUTS,
};

const SUPPORTED_NS_FLAGS: u32 =
    CLONE_NEWUTS | CLONE_NEWPID | CLONE_NEWNS | CLONE_NEWNET | CLONE_NEWIPC | CLONE_NEWUSER;

/// unshare(2) — disassociate parts of the process execution context.
pub fn sys_unshare(flags: u32) -> AxResult<isize> {
    if flags & !SUPPORTED_NS_FLAGS != 0 {
        warn!("sys_unshare: unsupported flags {:#x}", flags);
        return Err(AxError::InvalidInput);
    }

    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    let mut nsproxy = proc_data.nsproxy.lock();

    if flags & CLONE_NEWUTS != 0 {
        nsproxy.unshare_uts();
    }
    if flags & CLONE_NEWPID != 0 {
        nsproxy.prepare_child_pid_ns();
    }
    if flags & CLONE_NEWNS != 0 {
        nsproxy.unshare_mnt();
    }
    if flags & CLONE_NEWNET != 0 {
        nsproxy.unshare_net();
    }
    if flags & CLONE_NEWIPC != 0 {
        nsproxy.unshare_ipc();
    }
    if flags & CLONE_NEWUSER != 0 {
        nsproxy.unshare_user();
    }

    Ok(0)
}
