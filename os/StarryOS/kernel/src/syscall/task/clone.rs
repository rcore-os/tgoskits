use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use ax_fs::FS_CONTEXT;
use ax_hal::uspace::UserContext;
use ax_kspin::SpinNoIrq;
use ax_task::{AxTaskExt, current, spawn_task};
use bitflags::bitflags;
use linux_raw_sys::general::*;
use starry_process::Pid;
use starry_signal::Signo;
use starry_vm::VmMutPtr;

use crate::{
    file::{FD_TABLE, FileLike, PidFd, close_file_like},
    mm::copy_from_kernel,
    task::{AsThread, ProcessData, Thread, add_task_to_table, new_user_task},
};

bitflags! {
    /// Options for use with [`sys_clone`] and [`sys_clone3`].
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CloneFlags: u64 {
        /// The calling process and the child process run in the same memory space.
        const VM = CLONE_VM as u64;
        /// The caller and the child process share the same filesystem information.
        const FS = CLONE_FS as u64;
        /// The calling process and the child process share the same file descriptor table.
        const FILES = CLONE_FILES as u64;
        /// The calling process and the child process share the same table of signal handlers.
        const SIGHAND = CLONE_SIGHAND as u64;
        /// Sets pidfd to the child process's PID file descriptor.
        const PIDFD = CLONE_PIDFD as u64;
        /// If the calling process is being traced, then trace the child also.
        const PTRACE = CLONE_PTRACE as u64;
        /// The execution of the calling process is suspended until the child releases
        /// its virtual memory resources via a call to execve(2) or _exit(2) (as with vfork(2)).
        const VFORK = CLONE_VFORK as u64;
        /// The parent of the new child (as returned by getppid(2)) will be the same
        /// as that of the calling process.
        const PARENT = CLONE_PARENT as u64;
        /// The child is placed in the same thread group as the calling process.
        const THREAD = CLONE_THREAD as u64;
        /// The cloned child is started in a new mount namespace.
        const NEWNS = CLONE_NEWNS as u64;
        /// The child and the calling process share a single list of System V
        /// semaphore adjustment values.
        const SYSVSEM = CLONE_SYSVSEM as u64;
        /// The TLS (Thread Local Storage) descriptor is set to tls.
        const SETTLS = CLONE_SETTLS as u64;
        /// Store the child thread ID in the parent's memory.
        const PARENT_SETTID = CLONE_PARENT_SETTID as u64;
        /// Clear (zero) the child thread ID in child memory when the child exits,
        /// and do a wakeup on the futex at that address.
        const CHILD_CLEARTID = CLONE_CHILD_CLEARTID as u64;
        /// A tracing process cannot force `CLONE_PTRACE` on this child process.
        const UNTRACED = CLONE_UNTRACED as u64;
        /// Store the child thread ID in the child's memory.
        const CHILD_SETTID = CLONE_CHILD_SETTID as u64;
        /// Create the process in a new cgroup namespace.
        const NEWCGROUP = CLONE_NEWCGROUP as u64;
        /// Create the process in a new UTS namespace.
        const NEWUTS = CLONE_NEWUTS as u64;
        /// Create the process in a new IPC namespace.
        const NEWIPC = CLONE_NEWIPC as u64;
        /// Create the process in a new user namespace.
        const NEWUSER = CLONE_NEWUSER as u64;
        /// Create the process in a new PID namespace.
        const NEWPID = CLONE_NEWPID as u64;
        /// Create the process in a new network namespace.
        const NEWNET = CLONE_NEWNET as u64;
        /// The new process shares an I/O context with the calling process.
        const IO = CLONE_IO as u64;
        /// Clear signal handlers on clone (since Linux 5.5).
        const CLEAR_SIGHAND = 0x100000000u64;
        /// Clone into specific cgroup (since Linux 5.7).
        const INTO_CGROUP = 0x200000000u64;
        /// (Deprecated) Causes the parent not to receive a signal when the child terminated.
        const DETACHED = CLONE_DETACHED as u64;
    }
}

/// Unified arguments for clone/clone3/fork/vfork.
#[derive(Debug, Clone, Copy, Default)]
pub struct CloneArgs {
    pub flags: CloneFlags,
    pub exit_signal: u64,
    pub stack: usize,
    pub tls: usize,
    pub parent_tid: usize,
    pub child_tid: usize,
    pub pidfd: usize,
}

impl CloneArgs {
    fn validate(&self) -> AxResult<()> {
        let Self {
            flags, exit_signal, ..
        } = self;

        if *exit_signal > 0 && flags.intersects(CloneFlags::THREAD | CloneFlags::PARENT) {
            return Err(AxError::InvalidInput);
        }
        if flags.contains(CloneFlags::THREAD)
            && !flags.contains(CloneFlags::VM | CloneFlags::SIGHAND)
        {
            return Err(AxError::InvalidInput);
        }
        if flags.contains(CloneFlags::SIGHAND) && !flags.contains(CloneFlags::VM) {
            return Err(AxError::InvalidInput);
        }
        if flags.contains(CloneFlags::VFORK | CloneFlags::THREAD) {
            return Err(AxError::InvalidInput);
        }
        if flags.contains(CloneFlags::PIDFD | CloneFlags::DETACHED) {
            return Err(AxError::InvalidInput);
        }

        let namespace_flags = CloneFlags::NEWNS
            | CloneFlags::NEWIPC
            | CloneFlags::NEWNET
            | CloneFlags::NEWPID
            | CloneFlags::NEWUSER
            | CloneFlags::NEWUTS
            | CloneFlags::NEWCGROUP;

        if flags.intersects(namespace_flags) {
            warn!("sys_clone/sys_clone3: namespace flags detected, stub support only");
        }

        Ok(())
    }

    pub fn do_clone(self, uctx: &UserContext) -> AxResult<isize> {
        self.validate()?;

        let Self {
            mut flags,
            exit_signal,
            stack,
            tls,
            parent_tid,
            child_tid,
            pidfd,
        } = self;

        if flags.contains(CloneFlags::VFORK) {
            debug!("do_clone: CLONE_VFORK slow path");
            flags.remove(CloneFlags::VM);
        }

        debug!(
            "do_clone <= flags: {:?}, exit_signal: {}, stack: {:#x}, tls: {:#x}",
            flags, exit_signal, stack, tls
        );

        let exit_signal = if exit_signal > 0 {
            Some(Signo::from_repr(exit_signal as u8).ok_or(AxError::InvalidInput)?)
        } else {
            None
        };

        let mut new_uctx = *uctx;
        if stack != 0 {
            new_uctx.set_sp(stack);
        }
        if flags.contains(CloneFlags::SETTLS) {
            new_uctx.set_tls(tls);
        }
        new_uctx.set_retval(0);

        let set_child_tid = if flags.contains(CloneFlags::CHILD_SETTID) {
            child_tid
        } else {
            0
        };

        let curr = current();
        let old_proc_data = &curr.as_thread().proc_data;

        let mut new_task = new_user_task(&curr.name(), new_uctx, set_child_tid);

        let tid = new_task.id().as_u64() as Pid;
        if flags.contains(CloneFlags::PARENT_SETTID) && parent_tid != 0 {
            (parent_tid as *mut Pid).vm_write(tid).ok();
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
            } else if flags.contains(CloneFlags::CLEAR_SIGHAND) {
                Arc::new(SpinNoIrq::new(Default::default()))
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
            proc_data.set_heap_top(old_proc_data.get_heap_top());

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

        let parent_cred = Some(curr.as_thread().cred());
        let thr = Thread::new(tid, new_proc_data.clone(), parent_cred);
        if flags.contains(CloneFlags::CHILD_CLEARTID) {
            thr.set_clear_child_tid(child_tid);
        }
        if flags.contains(CloneFlags::PIDFD) && pidfd != 0 {
            let pidfd_obj = if flags.contains(CloneFlags::THREAD) {
                PidFd::new_thread(&thr)
            } else {
                PidFd::new_process(&new_proc_data)
            };
            let fd = pidfd_obj.add_to_fd_table(true)?;
            if let Err(err) = (pidfd as *mut i32).vm_write(fd) {
                let _ = close_file_like(fd);
                return Err(err.into());
            }
        }
        *new_task.task_ext_mut() = Some(AxTaskExt::from_impl(thr));

        let task = spawn_task(new_task);
        add_task_to_table(&task);

        Ok(tid as _)
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
    let clone_flags = CloneFlags::from_bits_truncate((flags & !FLAG_MASK) as u64);
    let exit_signal = (flags & FLAG_MASK) as u64;

    if clone_flags.contains(CloneFlags::PIDFD | CloneFlags::PARENT_SETTID) {
        return Err(AxError::InvalidInput);
    }

    let args = CloneArgs {
        flags: clone_flags,
        exit_signal,
        stack,
        tls,
        parent_tid,
        child_tid,
        // In sys_clone, parent_tid is reused for pidfd when CLONE_PIDFD is set
        pidfd: if clone_flags.contains(CloneFlags::PIDFD) {
            parent_tid
        } else {
            0
        },
    };

    args.do_clone(uctx)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_fork(uctx: &UserContext) -> AxResult<isize> {
    sys_clone(uctx, SIGCHLD, 0, 0, 0, 0)
}
