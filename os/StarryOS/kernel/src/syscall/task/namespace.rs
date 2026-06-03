use ax_errno::{AxError, AxResult};
use ax_task::current;
use linux_raw_sys::general::{
    CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER, CLONE_NEWUTS,
};

use crate::{
    file::{NsFd, PidFd, get_file_like},
    task::AsThread,
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

/// setns(2) — reassociate the calling thread with an existing namespace.
///
/// `fd` is a file descriptor that references a namespace (obtained by
/// opening `/proc/<pid>/ns/<type>`).  `nstype` is the expected namespace
/// type (`CLONE_NEW*`); `0` means allow any type.
///
/// # Errors
///
/// * `EBADF` — `fd` is not a valid file descriptor or not a namespace fd
/// * `EINVAL` — `nstype` does not match the namespace type, or multi-threaded
///   process attempts to change PID namespace
/// * `EPERM` — insufficient privileges (e.g. user namespace restrictions)
pub fn sys_setns(fd: u32, nstype: u32) -> AxResult<isize> {
    if nstype != 0 && nstype & !SUPPORTED_NS_FLAGS != 0 {
        warn!("sys_setns: unsupported nstype {:#x}", nstype);
        return Err(AxError::InvalidInput);
    }

    let file_like = get_file_like(fd as i32)?;

    // ── 方式一: NsFd (from /proc/<pid>/ns/<type>) ────────────────────
    if let Some(nsfd) = file_like.downcast_ref::<NsFd>() {
        return setns_via_nsfd(nsfd, nstype);
    }

    // ── 方式二: PidFd (from pidfd_open) ─────────────────────────────
    if let Some(pidfd) = file_like.downcast_ref::<PidFd>() {
        return setns_via_pidfd(pidfd, nstype);
    }

    Err(AxError::BadFileDescriptor)
}

/// setns via an NsFd (from `/proc/<pid>/ns/<type>`).
///
/// An NsFd always references exactly one namespace type, so `nstype`
/// must either be `0` or match the fd's type.
fn setns_via_nsfd(nsfd: &NsFd, nstype: u32) -> AxResult<isize> {
    let fd_type = nsfd.ns_type();

    if nstype != 0 && nstype != fd_type {
        warn!(
            "sys_setns: nstype {:#x} does not match fd type {:#x}",
            nstype, fd_type
        );
        return Err(AxError::InvalidInput);
    }

    let curr = current();
    let thread = curr.as_thread();
    let proc_data = &thread.proc_data;

    // PID namespace: calling process stays in its current PID ns;
    // the target ns is staged to child_pid_ns and consumed by the
    // next fork/clone. Must be single-threaded (Linux check).
    if fd_type == CLONE_NEWPID {
        let thread_count = proc_data.proc.threads().len();
        if thread_count > 1 {
            warn!(
                "sys_setns: cannot change PID namespace in multi-threaded process ({} threads)",
                thread_count
            );
            return Err(AxError::InvalidInput);
        }
    }

    let mut nsproxy = proc_data.nsproxy.lock();

    match nsfd {
        NsFd::Uts(ns) => nsproxy.set_ns_uts(ns.clone()),
        NsFd::Ipc(ns) => nsproxy.set_ns_ipc(ns.clone()),
        NsFd::Mnt(ns) => nsproxy.set_ns_mnt(ns.clone()),
        NsFd::Pid(ns) => nsproxy.set_ns_pid(ns.clone()),
        NsFd::Net(ns) => nsproxy.set_ns_net(ns.clone()),
        NsFd::User(ns) => {
            // Multi-threaded process cannot change user namespace.
            let thread_count = proc_data.proc.threads().len();
            if thread_count > 1 {
                warn!(
                    "sys_setns: cannot change user namespace in multi-threaded process ({} \
                     threads)",
                    thread_count
                );
                return Err(AxError::OperationNotPermitted);
            }
            nsproxy.set_ns_user(ns.clone());
        }
    }

    debug!(
        "sys_setns: successfully joined namespace type {:#x}",
        fd_type
    );
    Ok(0)
}

/// setns via a PidFd (from `pidfd_open(2)`).
///
/// `nstype` is a bitmask of `CLONE_NEW*` flags specifying which
/// namespaces to join from the target process.  Unlike the NsFd path,
/// this can join multiple namespaces in a single call.
fn setns_via_pidfd(pidfd: &PidFd, nstype: u32) -> AxResult<isize> {
    if nstype == 0 {
        warn!("sys_setns: nstype must be non-zero for pidfd");
        return Err(AxError::InvalidInput);
    }
    if nstype & !SUPPORTED_NS_FLAGS != 0 {
        warn!("sys_setns: unsupported nstype flags {:#x}", nstype);
        return Err(AxError::InvalidInput);
    }

    let target_proc = pidfd.process_data()?;
    let target_nsproxy = target_proc.nsproxy.lock();

    let curr = current();
    let thread = curr.as_thread();
    let proc_data = &thread.proc_data;

    // Check multi-threaded restrictions before making any changes.
    let thread_count = proc_data.proc.threads().len();
    if nstype & CLONE_NEWPID != 0 && thread_count > 1 {
        warn!(
            "sys_setns: cannot change PID namespace in multi-threaded process ({} threads)",
            thread_count
        );
        return Err(AxError::InvalidInput);
    }
    if nstype & CLONE_NEWUSER != 0 && thread_count > 1 {
        warn!(
            "sys_setns: cannot change user namespace in multi-threaded process ({} threads)",
            thread_count
        );
        return Err(AxError::OperationNotPermitted);
    }

    let mut nsproxy = proc_data.nsproxy.lock();

    if nstype & CLONE_NEWUTS != 0 {
        nsproxy.set_ns_uts(target_nsproxy.uts_ns.clone());
    }
    if nstype & CLONE_NEWIPC != 0 {
        nsproxy.set_ns_ipc(target_nsproxy.ipc_ns.clone());
    }
    if nstype & CLONE_NEWNS != 0 {
        nsproxy.set_ns_mnt(target_nsproxy.mnt_ns.clone());
    }
    if nstype & CLONE_NEWPID != 0 {
        nsproxy.set_ns_pid(target_nsproxy.pid_ns.clone());
    }
    if nstype & CLONE_NEWNET != 0 {
        nsproxy.set_ns_net(target_nsproxy.net_ns.clone());
    }
    if nstype & CLONE_NEWUSER != 0 {
        nsproxy.set_ns_user(target_nsproxy.user_ns.clone());
    }

    debug!(
        "sys_setns: successfully joined namespaces {:#x} via pidfd",
        nstype
    );
    Ok(0)
}
