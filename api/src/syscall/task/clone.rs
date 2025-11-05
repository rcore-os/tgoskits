use alloc::sync::Arc;

use axerrno::{AxError, AxResult};
use axfs_ng::FS_CONTEXT;
use axhal::uspace::UserContext;
use axtask::{TaskExtProxy, current, spawn_task};
use bitflags::bitflags;
use kspin::SpinNoIrq;
use linux_raw_sys::general::*;
use starry_core::{
    mm::copy_from_kernel,
    task::{AsThread, ProcessData, Thread, add_task_to_table},
};
use starry_process::Pid;
use starry_signal::Signo;

use crate::{
    file::{FD_TABLE, FileLike, PidFd},
    mm::UserPtr,
    task::new_user_task,
};

bitflags! {
    /// Options for use with [`sys_clone`].
    #[derive(Debug, Clone, Copy, Default)]
    struct CloneFlags: u32 {
        /// The calling process and the child process run in the same
        /// memory space.
        const VM = CLONE_VM;
        /// The caller and the child process share the same  filesystem
        /// information.
        const FS = CLONE_FS;
        /// The calling process and the child process share the same file
        /// descriptor table.
        const FILES = CLONE_FILES;
        /// The calling process and the child process share the same table
        /// of signal handlers.
        const SIGHAND = CLONE_SIGHAND;
        /// Sets pidfd to the child process's PID file descriptor.
        const PIDFD = CLONE_PIDFD;
        /// If the calling process is being traced, then trace the child
        /// also.
        const PTRACE = CLONE_PTRACE;
        /// The execution of the calling process is suspended until the
        /// child releases its virtual memory resources via a call to
        /// execve(2) or _exit(2) (as with vfork(2)).
        const VFORK = CLONE_VFORK;
        /// The parent of the new child  (as returned by getppid(2))
        /// will be the same as that of the calling process.
        const PARENT = CLONE_PARENT;
        /// The child is placed in the same thread group as the calling
        /// process.
        const THREAD = CLONE_THREAD;
        /// The cloned child is started in a new mount namespace.
        const NEWNS = CLONE_NEWNS;
        /// The child and the calling process share a single list of System
        /// V semaphore adjustment values
        const SYSVSEM = CLONE_SYSVSEM;
        /// The TLS (Thread Local Storage) descriptor is set to tls.
        const SETTLS = CLONE_SETTLS;
        /// Store the child thread ID in the parent's memory.
        const PARENT_SETTID = CLONE_PARENT_SETTID;
        /// Clear (zero) the child thread ID in child memory when the child
        /// exits, and do a wakeup on the futex at that address.
        const CHILD_CLEARTID = CLONE_CHILD_CLEARTID;
        /// A tracing process cannot force `CLONE_PTRACE` on this child
        /// process.
        const UNTRACED = CLONE_UNTRACED;
        /// Store the child thread ID in the child's memory.
        const CHILD_SETTID = CLONE_CHILD_SETTID;
        /// Create the process in a new cgroup namespace.
        const NEWCGROUP = CLONE_NEWCGROUP;
        /// Create the process in a new UTS namespace.
        const NEWUTS = CLONE_NEWUTS;
        /// Create the process in a new IPC namespace.
        const NEWIPC = CLONE_NEWIPC;
        /// Create the process in a new user namespace.
        const NEWUSER = CLONE_NEWUSER;
        /// Create the process in a new PID namespace.
        const NEWPID = CLONE_NEWPID;
        /// Create the process in a new network namespace.
        const NEWNET = CLONE_NEWNET;
        /// The new process shares an I/O context with the calling process.
        const IO = CLONE_IO;
    }
}

pub fn sys_clone(
    uctx: &UserContext,
    flags: u32,
    stack: usize,
    parent_tid: usize,
    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))] child_tid: usize,
    tls: usize,
    #[cfg(not(any(target_arch = "x86_64", target_arch = "loongarch64")))] child_tid: usize,
) -> AxResult<isize> {
    const FLAG_MASK: u32 = 0xff;
    let exit_signal = flags & FLAG_MASK;
    let mut flags = CloneFlags::from_bits_truncate(flags & !FLAG_MASK);
    if flags.contains(CloneFlags::VFORK) {
        debug!("sys_clone: CLONE_VFORK slow path");
        flags.remove(CloneFlags::VM);
    }

    debug!(
        "sys_clone <= flags: {flags:?}, exit_signal: {exit_signal}, stack: {stack:#x}, ptid: \
         {parent_tid:#x}, ctid: {child_tid:#x}, tls: {tls:#x}"
    );

    if exit_signal != 0 && flags.contains(CloneFlags::THREAD | CloneFlags::PARENT) {
        return Err(AxError::InvalidInput);
    }
    if flags.contains(CloneFlags::THREAD) && !flags.contains(CloneFlags::VM | CloneFlags::SIGHAND) {
        return Err(AxError::InvalidInput);
    }
    if flags.contains(CloneFlags::PIDFD | CloneFlags::PARENT_SETTID) {
        return Err(AxError::InvalidInput);
    }
    let exit_signal = Signo::from_repr(exit_signal as u8);

    let mut new_uctx = *uctx;
    if stack != 0 {
        new_uctx.set_sp(stack);
    }
    if flags.contains(CloneFlags::SETTLS) {
        new_uctx.set_tls(tls);
    }
    new_uctx.set_retval(0);

    let set_child_tid = if flags.contains(CloneFlags::CHILD_SETTID) {
        Some(UserPtr::<u32>::from(child_tid).get_as_mut()?)
    } else {
        None
    };

    let curr = current();
    let old_proc_data = &curr.as_thread().proc_data;

    let mut new_task = new_user_task(&curr.name(), new_uctx, set_child_tid);

    let tid = new_task.id().as_u64() as Pid;
    if flags.contains(CloneFlags::PARENT_SETTID) {
        *UserPtr::<Pid>::from(parent_tid).get_as_mut()? = tid;
    }

    let new_proc_data = if flags.contains(CloneFlags::THREAD) {
        new_task
            .ctx_mut()
            .set_page_table_root(old_proc_data.aspace.lock().page_table_root());
        old_proc_data.clone()
    } else {
        let proc = if flags.contains(CloneFlags::PARENT) {
            old_proc_data.proc.parent().ok_or(AxError::InvalidInput)?
        } else {
            old_proc_data.proc.clone()
        }
        .fork(tid);

        let aspace = if flags.contains(CloneFlags::VM) {
            old_proc_data.aspace.clone()
        } else {
            let mut aspace = old_proc_data.aspace.lock();
            let aspace = aspace.try_clone()?;
            copy_from_kernel(&mut aspace.lock())?;
            aspace
        };
        new_task
            .ctx_mut()
            .set_page_table_root(aspace.lock().page_table_root());

        let signal_actions = if flags.contains(CloneFlags::SIGHAND) {
            old_proc_data.signal.actions.clone()
        } else {
            Arc::new(SpinNoIrq::new(old_proc_data.signal.actions.lock().clone()))
        };
        let proc_data = ProcessData::new(
            proc,
            old_proc_data.exe_path.read().clone(),
            old_proc_data.cmdline.read().clone(),
            aspace,
            signal_actions,
            exit_signal,
        );
        proc_data.set_umask(old_proc_data.umask());

        {
            let mut scope = proc_data.scope.write();
            if flags.contains(CloneFlags::FILES) {
                FD_TABLE.scope_mut(&mut scope).clone_from(&FD_TABLE);
            } else {
                FD_TABLE
                    .scope_mut(&mut scope)
                    .write()
                    .clone_from(&FD_TABLE.read());
            }

            if flags.contains(CloneFlags::FS) {
                FS_CONTEXT.scope_mut(&mut scope).clone_from(&FS_CONTEXT);
            } else {
                FS_CONTEXT
                    .scope_mut(&mut scope)
                    .lock()
                    .clone_from(&FS_CONTEXT.lock());
            }
        }

        proc_data
    };

    new_proc_data.proc.add_thread(tid);

    if flags.contains(CloneFlags::PIDFD) {
        let pidfd = PidFd::new(&new_proc_data);
        *UserPtr::<i32>::from(parent_tid).get_as_mut()? = pidfd.add_to_fd_table(true)?;
    }

    let thr = Thread::new(tid, new_proc_data);
    if flags.contains(CloneFlags::CHILD_CLEARTID) {
        thr.set_clear_child_tid(child_tid);
    }
    *new_task.task_ext_mut() = Some(unsafe { TaskExtProxy::from_impl(thr) });

    let task = spawn_task(new_task);
    add_task_to_table(&task);

    Ok(tid as _)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_fork(uctx: &UserContext) -> AxResult<isize> {
    sys_clone(uctx, SIGCHLD, 0, 0, 0, 0)
}
