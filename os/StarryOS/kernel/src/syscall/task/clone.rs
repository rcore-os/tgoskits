use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::FS_CONTEXT;
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::cpu::uspace::UserContext;
use bitflags::bitflags;
use bytemuck::{AnyBitPattern, NoUninit};
use linux_raw_sys::general::*;
use scope_local::Scope;
use starry_process::{Pid, Process};
use starry_signal::Signo;
use starry_vm::{VmMutPtr, VmPtr};

use super::schedule_abi::fork_schedule_policy;
#[cfg(target_arch = "riscv64")]
use crate::task::prepare_user_thread_with_fp_state_and_policy;
#[cfg(not(target_arch = "riscv64"))]
use crate::task::prepare_user_thread_with_policy;
use crate::{
    file::{FD_TABLE, FileLike, PidFd, add_file_like, close_file_like_if},
    mm::copy_from_kernel,
    task::{
        ProcessData, ProcessImage, Thread, allocate_user_tid, current_user_task, new_user_task,
        register_prepared_task,
    },
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

// The `sched:sched_process_fork` tracepoint is defined here, next to its sole
// emission site in `CloneArgs::do_clone` (which all of clone/clone3/fork/vfork
// funnel through), so the event schema and the fast-path call stay together.
// Registration into the global `.tracepoint` section is by link section, so
// the definition's module location is immaterial to discovery.
ktracepoint::define_event_trace!(
    sched_process_fork,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(sched),
    TP_PROTO(parent_tid: u64, child_tid: u64),
    TP_STRUCT__entry {
        parent_tid: u64,
        child_tid: u64,
    },
    TP_fast_assign {
        parent_tid: parent_tid,
        child_tid: child_tid,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "parent_tid={} child_tid={}",
            __entry.parent_tid,
            __entry.child_tid,
        )
    })
);

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

struct UnpublishedThread {
    process: Arc<Process>,
    tid: Pid,
    committed: bool,
}

impl UnpublishedThread {
    fn register(process: Arc<Process>, tid: Pid) -> Self {
        process.add_thread(tid);
        Self {
            process,
            tid,
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for UnpublishedThread {
    fn drop(&mut self) {
        if !self.committed {
            assert!(
                self.process.remove_unpublished_thread(self.tid),
                "prepared thread disappeared before clone rollback"
            );
        }
    }
}

struct PendingChildPidNamespace {
    owner: Arc<ProcessData>,
    namespace: Arc<SpinNoIrq<axnsproxy::PidNamespace>>,
    committed: bool,
}

impl PendingChildPidNamespace {
    fn take(owner: &Arc<ProcessData>) -> Option<Self> {
        let namespace = owner.nsproxy.lock().child_pid_ns.take()?;
        Some(Self {
            owner: owner.clone(),
            namespace,
            committed: false,
        })
    }

    fn namespace(&self) -> Arc<SpinNoIrq<axnsproxy::PidNamespace>> {
        self.namespace.clone()
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PendingChildPidNamespace {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = self
            .owner
            .nsproxy
            .lock()
            .restore_child_pid_ns_if_empty(self.namespace.clone());
    }
}

struct LocalPidReservation {
    namespace: Arc<SpinNoIrq<axnsproxy::PidNamespace>>,
    global_tid: u64,
    local_pid: u32,
    committed: bool,
}

impl LocalPidReservation {
    fn reserve(
        namespace: Arc<SpinNoIrq<axnsproxy::PidNamespace>>,
        global_tid: u64,
        namespace_init: bool,
    ) -> Option<Self> {
        let mut state = namespace.lock();
        if state.level == 0 {
            return None;
        }
        let local_pid = state.alloc_local_pid(global_tid);
        if namespace_init {
            assert_eq!(
                local_pid, 1,
                "new PID namespace must publish its init task as PID 1"
            );
            state.set_init_global_tid(global_tid);
        }
        drop(state);
        Some(Self {
            namespace,
            global_tid,
            local_pid,
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for LocalPidReservation {
    fn drop(&mut self) {
        if !self.committed {
            assert!(
                self.namespace
                    .lock()
                    .release_unpublished_local_pid(self.global_tid, self.local_pid),
                "PID namespace reservation changed before clone rollback"
            );
        }
    }
}

struct UserWriteRollback<T>
where
    T: AnyBitPattern + NoUninit + Copy + PartialEq,
{
    pointer: *mut T,
    previous: T,
    installed: T,
    committed: bool,
}

impl<T> UserWriteRollback<T>
where
    T: AnyBitPattern + NoUninit + Copy + PartialEq,
{
    fn install(pointer: *mut T, installed: T) -> AxResult<Self> {
        let previous = pointer.vm_read()?;
        pointer.vm_write(installed)?;
        Ok(Self {
            pointer,
            previous,
            installed,
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl<T> Drop for UserWriteRollback<T>
where
    T: AnyBitPattern + NoUninit + Copy + PartialEq,
{
    fn drop(&mut self) {
        if self.committed || self.pointer.vm_read() != Ok(self.installed) {
            return;
        }
        if self.pointer.vm_write(self.previous).is_err() {
            warn!("clone rollback could not restore a parent-visible word");
        }
    }
}

struct InstalledPidFd {
    fd: i32,
    file: Arc<dyn FileLike>,
    committed: bool,
}

impl InstalledPidFd {
    fn install(file: Arc<dyn FileLike>) -> AxResult<Self> {
        let fd = add_file_like(file.clone(), true)?;
        Ok(Self {
            fd,
            file,
            committed: false,
        })
    }

    fn fd(&self) -> i32 {
        self.fd
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for InstalledPidFd {
    fn drop(&mut self) {
        if !self.committed && !close_file_like_if(self.fd, &self.file) {
            warn!("clone rollback found a replaced PIDFD slot");
        }
    }
}

struct UnpublishedPtraceStop {
    process: Arc<ProcessData>,
    tid: Pid,
    committed: bool,
}

impl UnpublishedPtraceStop {
    fn register(process: Arc<ProcessData>, tid: Pid, context: &UserContext) -> Self {
        process.set_ptrace_stop(tid, Signo::SIGSTOP, context);
        Self {
            process,
            tid,
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for UnpublishedPtraceStop {
    fn drop(&mut self) {
        if !self.committed {
            assert!(
                self.process
                    .take_ptrace_stop_user_context_for(self.tid)
                    .is_some(),
                "prepared ptrace stop disappeared before clone rollback"
            );
        }
    }
}

impl CloneArgs {
    fn validate(&self) -> AxResult<()> {
        let Self {
            flags, exit_signal, ..
        } = self;

        if *exit_signal > 0 && flags.contains(CloneFlags::THREAD) {
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
        if flags.contains(CloneFlags::NEWNS | CloneFlags::FS) {
            return Err(AxError::InvalidInput);
        }
        if flags.contains(CloneFlags::NEWPID)
            && flags.intersects(CloneFlags::THREAD | CloneFlags::PARENT)
        {
            return Err(AxError::InvalidInput);
        }

        Ok(())
    }

    pub fn do_clone(self, uctx: &UserContext) -> AxResult<isize> {
        self.validate()?;

        let Self {
            flags,
            exit_signal,
            stack,
            tls,
            parent_tid: parent_tid_ptr,
            child_tid,
            pidfd,
        } = self;

        debug!(
            "do_clone <= flags: {:?}, exit_signal: {}, stack: {:#x}, tls: {:#x}",
            flags, exit_signal, stack, tls
        );

        let exit_signal = if exit_signal > 0 {
            Some(Signo::from_repr(exit_signal as u8).ok_or(AxError::InvalidInput)?)
        } else {
            None
        };

        // Linux blocks the parent for every CLONE_VFORK clone until the child
        // execs or exits, regardless of whether the caller passed a child stack.
        // BusyBox shell/timeout paths rely on that ordering when they combine
        // CLONE_VM, CLONE_VFORK, and a private child stack.
        let needs_vfork_block = flags.contains(CloneFlags::VFORK);

        let mut new_uctx = *uctx;
        new_uctx.prepare_clone_child_return_state();
        if stack != 0 {
            new_uctx.set_sp(stack);
        }
        if flags.contains(CloneFlags::SETTLS) {
            new_uctx.set_tls(tls);
        }
        new_uctx.set_retval(0);
        #[cfg(target_arch = "riscv64")]
        let child_fp_fs = match uctx.sstatus.fs() {
            riscv::register::sstatus::FS::Dirty => riscv::register::sstatus::FS::Clean,
            fs => fs,
        };
        #[cfg(target_arch = "riscv64")]
        new_uctx.sstatus.set_fs(child_fp_fs);

        let set_child_tid = if flags.contains(CloneFlags::CHILD_SETTID) {
            child_tid
        } else {
            0
        };

        let curr = current_user_task();
        let curr_thread = curr.as_thread();
        let old_proc_data = &curr_thread.proc_data;
        if flags.contains(CloneFlags::NEWCGROUP) && !curr_thread.cred().has_cap_sys_admin() {
            return Err(AxError::OperationNotPermitted);
        }
        let (child_policy, child_reset_on_fork) =
            fork_schedule_policy(curr.policy(), curr.reset_on_fork())?;
        let child_nice = match child_policy {
            ax_std::os::arceos::task::SchedulePolicy::Fair { nice, .. } => i32::from(nice.get()),
            _ => curr_thread.nice(),
        };

        let tid = allocate_user_tid()?;
        #[cfg(target_arch = "riscv64")]
        let child_fp_state = {
            let mut fp_state = ax_cpu::FpState::default();
            fp_state.save();
            fp_state.fs = child_fp_fs;
            fp_state
        };

        let mut scope = Scope::new();
        let current_fd_table = crate::file::current_fd_table();
        if flags.contains(CloneFlags::FILES) {
            // Synchronize with close_all_fds: holding a read lock ensures
            // close_all_fds either observes our strong-count increment or
            // blocks until the new thread has installed the shared Arc.
            let _guard = current_fd_table.read();
            FD_TABLE.scope_mut(&mut scope).clone_from(&current_fd_table);
        } else {
            FD_TABLE
                .scope_mut(&mut scope)
                .write()
                .clone_from(&current_fd_table.read());
        }

        let current_fs_context = ax_fs_ng::vfs::current_fs_context();
        if flags.contains(CloneFlags::FS) {
            FS_CONTEXT
                .scope_mut(&mut scope)
                .clone_from(&current_fs_context);
        } else {
            let mut fs_context = current_fs_context.lock().clone();
            if flags.contains(CloneFlags::NEWNS) {
                fs_context.unshare_mount_namespace()?;
            }
            *FS_CONTEXT.scope_mut(&mut scope).lock() = fs_context;
        }

        let mut pending_pid_namespace = None;
        let mut prepared_process = None;

        let (new_proc_data, page_table_root, namespace_init) = if flags.contains(CloneFlags::THREAD)
        {
            let page_table_root = old_proc_data.aspace().lock().page_table_root().as_usize();
            (old_proc_data.clone(), page_table_root, false)
        } else {
            let parent_process = if flags.contains(CloneFlags::PARENT) {
                old_proc_data.proc.parent().ok_or(AxError::InvalidInput)?
            } else {
                old_proc_data.proc.clone()
            };

            let aspace = if flags.contains(CloneFlags::VM) {
                old_proc_data.aspace()
            } else {
                let aspace_arc = old_proc_data.aspace();
                let aspace = aspace_arc.lock().try_clone()?;
                copy_from_kernel(&mut aspace.lock())?;
                aspace
            };
            let page_table_root = aspace.lock().page_table_root().as_usize();

            let signal_actions = if flags.contains(CloneFlags::SIGHAND) {
                old_proc_data.signal.actions()
            } else if flags.contains(CloneFlags::CLEAR_SIGHAND) {
                Arc::new(SpinNoIrq::new(Default::default()))
            } else {
                Arc::new(SpinNoIrq::new(
                    old_proc_data.signal.actions().lock().clone(),
                ))
            };

            let inherited_cgroup = old_proc_data.cgroup.read().clone();
            let mut new_nsproxy = old_proc_data.nsproxy.lock().clone_all();
            if flags.contains(CloneFlags::NEWUTS) {
                new_nsproxy.unshare_uts();
            }
            if flags.contains(CloneFlags::NEWIPC) {
                new_nsproxy.unshare_ipc();
            }
            if flags.contains(CloneFlags::NEWNS) {
                new_nsproxy.unshare_mnt();
            }
            let mut namespace_init = false;
            if flags.contains(CloneFlags::NEWPID) {
                new_nsproxy.unshare_pid();
                namespace_init = true;
            } else if let Some(reservation) = PendingChildPidNamespace::take(old_proc_data) {
                new_nsproxy.pid_ns = reservation.namespace();
                pending_pid_namespace = Some(reservation);
                namespace_init = true;
            }
            if flags.contains(CloneFlags::NEWNET) {
                new_nsproxy.unshare_net();
            }
            if flags.contains(CloneFlags::NEWUSER) {
                new_nsproxy.unshare_user();
            }
            if flags.contains(CloneFlags::NEWCGROUP) {
                new_nsproxy.unshare_cgroup(inherited_cgroup.clone());
            }

            let fork = parent_process.prepare_fork(tid);
            let proc = fork.process().clone();
            prepared_process = Some(fork);
            let proc_data = ProcessData::new(
                proc,
                ProcessImage::new(
                    old_proc_data.exe_path.read().clone(),
                    old_proc_data.cmdline.read().clone(),
                    old_proc_data.envp.read().clone(),
                    old_proc_data.auxv.read().clone(),
                    old_proc_data.root_path.read().clone(),
                    old_proc_data.cwd_path.read().clone(),
                ),
                aspace,
                signal_actions,
                exit_signal,
                curr_thread.tid(),
                flags.contains(CloneFlags::VM),
            );
            proc_data.set_umask(old_proc_data.umask());
            *proc_data.cgroup.write() = inherited_cgroup;
            proc_data.set_heap_top(old_proc_data.get_heap_top());
            proc_data.replace_personality(old_proc_data.personality());
            // Inherit parent dumpable (PR_SET_DUMPABLE state). Linux: child
            // fork/clone copies mm->dumpable from parent; without this, a
            // child of `prctl(PR_SET_DUMPABLE, 0) -> fork()` would reset to
            // SUID_DUMP_USER (1), breaking the safety semantics this PR is
            // supposed to enforce. Verified via Linux host: parent sets 0,
            // fork child PR_GET_DUMPABLE returns 0.
            proc_data.set_dumpable(old_proc_data.dumpable());
            proc_data.set_thp_disable(old_proc_data.thp_disable());

            *proc_data.nsproxy.lock() = new_nsproxy;

            (proc_data, page_table_root, namespace_init)
        };

        let unpublished_thread = UnpublishedThread::register(new_proc_data.proc.clone(), tid);
        let pid_namespace = new_proc_data.nsproxy.lock().pid_ns.clone();
        let local_pid_reservation =
            LocalPidReservation::reserve(pid_namespace, tid as u64, namespace_init);

        let parent_cred = Some(curr_thread.cred());
        let thr = Thread::new(
            tid,
            new_proc_data.clone(),
            parent_cred,
            curr_thread.signal.blocked(),
            scope,
        );
        thr.set_nice(child_nice);
        if curr_thread.no_new_privs() {
            thr.set_no_new_privs();
        }
        thr.set_seccomp_state(curr_thread.seccomp_state());
        if flags.contains(CloneFlags::CHILD_CLEARTID) {
            thr.set_clear_child_tid(child_tid);
        }
        let pidfd_file = if flags.contains(CloneFlags::PIDFD) && pidfd != 0 {
            // The pidfd and the later registry publication share the identity
            // embedded in ProcessData. A failed clone therefore cannot leave a
            // prematurely registered PID behind.
            let identity = new_proc_data.identity();
            let pidfd_obj = if flags.contains(CloneFlags::THREAD) {
                PidFd::new_thread(identity, &thr, tid)
            } else {
                PidFd::new_process(identity)
            };
            Some(Arc::new(pidfd_obj) as Arc<dyn FileLike>)
        } else {
            None
        };
        // perf: clone any `attr.inherit` event from the parent onto the child so
        // `perf record` follows it. Done before the child is scheduled (it is not
        // yet spawned) so the counter is present the first time the child runs.
        #[cfg(target_arch = "aarch64")]
        crate::perf::task::on_clone_inherit(curr_thread, &thr);
        // vfork(2) and clone(CLONE_VFORK) must sleep the parent until the child
        // execs or exits. Use PollSet so the parent's wait remains
        // interruptible by task.interrupt().
        if needs_vfork_block {
            let poll = Arc::new(axpoll::PollSet::new());
            new_proc_data.set_vfork_done(poll);
        }

        let parent_pid = curr.as_thread().proc_data.proc.pid();
        // The user-visible tid, not the scheduler id: they diverge for the init
        // process (pid/tid pinned to 1, scheduler id higher). Signal delivery
        // and ptrace below look this up in the tid-keyed task table.
        let parent_tid = curr.as_thread().tid() as Pid;
        let ptrace_event = if flags.contains(CloneFlags::THREAD) {
            super::ptrace::PTRACE_EVENT_CLONE
        } else if flags.contains(CloneFlags::VFORK) {
            super::ptrace::PTRACE_EVENT_VFORK
        } else {
            super::ptrace::PTRACE_EVENT_FORK
        };
        let ptrace_clone_event = super::ptrace::prepare_ptrace_clone_event(
            parent_pid,
            parent_tid,
            tid as Pid,
            ptrace_event,
        );
        let trace_clone = ptrace_clone_event.is_some();
        let unpublished_ptrace_stop = if let Some(tracer_pid) = ptrace_clone_event
            .as_ref()
            .and_then(|event| event.tracer_pid())
        {
            if !flags.contains(CloneFlags::THREAD) {
                new_proc_data.set_ptrace_tracer_pid(tracer_pid);
                new_proc_data.set_ptrace_attached();
            }
            Some(UnpublishedPtraceStop::register(
                new_proc_data.clone(),
                tid,
                &new_uctx,
            ))
        } else {
            None
        };

        #[cfg(target_arch = "riscv64")]
        let prepared_task = prepare_user_thread_with_fp_state_and_policy(
            new_user_task(new_uctx, set_child_tid),
            curr.name(),
            crate::config::KERNEL_STACK_SIZE,
            page_table_root,
            child_fp_state,
            thr,
            child_policy,
            child_reset_on_fork,
        )
        .map_err(map_task_creation_error)?;
        #[cfg(not(target_arch = "riscv64"))]
        let prepared_task = prepare_user_thread_with_policy(
            new_user_task(new_uctx, set_child_tid),
            curr.name(),
            crate::config::KERNEL_STACK_SIZE,
            page_table_root,
            thr,
            child_policy,
            child_reset_on_fork,
        )
        .map_err(map_task_creation_error)?;

        let published_process = prepared_process
            .map(|process| process.publish().ok_or(AxError::BadState))
            .transpose()?;
        let task_registration = prepared_task
            .with_task(|task| register_prepared_task(task, !flags.contains(CloneFlags::THREAD)))?;
        let installed_pidfd = pidfd_file.map(InstalledPidFd::install).transpose()?;
        let pidfd_write = installed_pidfd
            .as_ref()
            .map(|installed| UserWriteRollback::install(pidfd as *mut i32, installed.fd()))
            .transpose()?;
        let parent_tid_write = (flags.contains(CloneFlags::PARENT_SETTID) && parent_tid_ptr != 0)
            .then(|| UserWriteRollback::install(parent_tid_ptr as *mut Pid, tid))
            .transpose()?;

        let mut cgroup_guard = if flags.contains(CloneFlags::THREAD) {
            None
        } else {
            Some(
                crate::cgroup::begin_fork(new_proc_data.cgroup.read().clone(), tid)
                    .map_err(crate::cgroup::cgroup_error)?,
            )
        };
        if let Some(guard) = &mut cgroup_guard {
            guard.commit();
        }

        let _task = match prepared_task.publish() {
            Ok(task) => task,
            Err(error) => {
                if cgroup_guard.is_some()
                    && let Err(rollback_error) = crate::cgroup::exit_process(tid)
                {
                    warn!(
                        "clone rollback could not release cgroup membership for pid {tid}: \
                         {rollback_error}"
                    );
                }
                return Err(map_task_creation_error(error));
            }
        };

        if let Some(write) = parent_tid_write {
            write.commit();
        }
        if let Some(write) = pidfd_write {
            write.commit();
        }
        if let Some(installed) = installed_pidfd {
            installed.commit();
        }
        task_registration.commit();
        if let Some(stop) = unpublished_ptrace_stop {
            stop.commit();
        }
        if let Some(reservation) = local_pid_reservation {
            reservation.commit();
        }
        unpublished_thread.commit();
        if let Some(process) = published_process {
            process.commit();
        }
        if let Some(namespace) = pending_pid_namespace {
            namespace.commit();
        }
        if let Some(event) = ptrace_clone_event {
            event.publish();
        }

        if trace_clone && needs_vfork_block {
            let _ = crate::task::send_signal_to_thread(
                None,
                parent_tid,
                Some(starry_signal::SignalInfo::new_kernel(
                    starry_signal::Signo::SIGTRAP,
                )),
            );
        }

        // Fire before any potential vfork-wait so observers see the fork edge
        // even when the parent blocks below.
        trace_sched_process_fork(curr.id().as_u64(), tid as u64);

        // perf side-band: tell any `attr.task` event watching the parent that it
        // forked a child (PERF_RECORD_FORK), so `perf record` can account it.
        // Emitted before any vfork-wait below, in the parent's context.
        #[cfg(target_arch = "aarch64")]
        crate::perf::task::on_clone_sideband(
            curr.as_thread(),
            new_proc_data.proc.pid(),
            tid as u32,
        );

        // Block the parent until the child exec's or exits.
        if needs_vfork_block {
            new_proc_data.wait_vfork_done();
            let _ = super::ptrace::ptrace_notify_vfork_done(parent_pid, parent_tid, tid as Pid);
        }

        Ok(tid as _)
    }
}

fn map_task_creation_error(error: ax_std::os::arceos::task::TaskError) -> AxError {
    use ax_std::os::arceos::task::TaskError;

    match error {
        TaskError::TimerCapacity | TaskError::RuntimeFailure(_) => AxError::NoMemory,
        TaskError::DeadlineAdmission | TaskError::ThreadBusy => AxError::ResourceBusy,
        _ => AxError::BadState,
    }
}

ktracepoint::define_event_trace!(
    sys_clone,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(syscalls),
    TP_PROTO(flags:u32, stack:usize, parent_tid:usize),
    TP_STRUCT__entry {
        stack: usize,
        parent_tid: usize,
        flags: u32,
    },
    TP_fast_assign {
        flags: flags,
        stack: stack,
        parent_tid: parent_tid,
    },
    TP_ident(__entry),
    TP_printk({
        let flags = __entry.flags;
        let stack = __entry.stack;
        let parent_tid = __entry.parent_tid;
        alloc::format!("clone with flags: {flags}, stack: {stack:#x}, parent_tid: {parent_tid:#x}")
    })
);

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

    trace_sys_clone(clone_flags.bits() as _, stack, parent_tid);

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

#[cfg(target_arch = "x86_64")]
pub fn sys_vfork(uctx: &UserContext) -> AxResult<isize> {
    let flags = (CloneFlags::VFORK | CloneFlags::VM).bits() as u32 | SIGCHLD;
    sys_clone(uctx, flags, 0, 0, 0, 0)
}

#[cfg(axtest)]
pub(crate) fn clone_validation_rules_hold_for_test() -> bool {
    let parent_signal_allowed = CloneArgs {
        flags: CloneFlags::PARENT,
        exit_signal: SIGCHLD as u64,
        ..Default::default()
    }
    .validate()
    .is_ok();
    let thread_signal_rejected = CloneArgs {
        flags: CloneFlags::THREAD | CloneFlags::VM | CloneFlags::SIGHAND,
        exit_signal: SIGCHLD as u64,
        ..Default::default()
    }
    .validate()
    .is_err();
    let sighand_without_vm_rejected = CloneArgs {
        flags: CloneFlags::SIGHAND,
        ..Default::default()
    }
    .validate()
    .is_err();
    let newns_with_fs_rejected = CloneArgs {
        flags: CloneFlags::NEWNS | CloneFlags::FS,
        ..Default::default()
    }
    .validate()
    .is_err();
    // Cover the remaining validation arms to keep the full state machine under
    // axtest coverage (the host `#[cfg(test)]` mod below mirrors these but does
    // not execute during the kernel coverage run).
    let thread_without_vm_sighand_rejected = CloneArgs {
        flags: CloneFlags::THREAD,
        ..Default::default()
    }
    .validate()
    .is_err();
    let vfork_with_thread_rejected = CloneArgs {
        flags: CloneFlags::VFORK | CloneFlags::THREAD | CloneFlags::VM | CloneFlags::SIGHAND,
        ..Default::default()
    }
    .validate()
    .is_err();
    let pidfd_with_detached_rejected = CloneArgs {
        flags: CloneFlags::PIDFD | CloneFlags::DETACHED,
        ..Default::default()
    }
    .validate()
    .is_err();
    let newcgroup_allowed = CloneArgs {
        flags: CloneFlags::NEWCGROUP,
        ..Default::default()
    }
    .validate()
    .is_ok();
    // Empty flags + no exit signal is the minimal valid configuration.
    let minimal_valid = CloneArgs {
        flags: CloneFlags::empty(),
        exit_signal: 0,
        ..Default::default()
    }
    .validate()
    .is_ok();
    // A plain thread clone with VM|SIGHAND and no exit signal is the canonical
    // valid pthread spawn configuration.
    let thread_valid = CloneArgs {
        flags: CloneFlags::THREAD | CloneFlags::VM | CloneFlags::SIGHAND,
        exit_signal: 0,
        ..Default::default()
    }
    .validate()
    .is_ok();

    parent_signal_allowed
        && thread_signal_rejected
        && sighand_without_vm_rejected
        && newns_with_fs_rejected
        && thread_without_vm_sighand_rejected
        && vfork_with_thread_rejected
        && pidfd_with_detached_rejected
        && newcgroup_allowed
        && minimal_valid
        && thread_valid
}

#[cfg(test)]
mod tests {
    use linux_raw_sys::general::SIGCHLD;

    use super::{CloneArgs, CloneFlags};

    #[test]
    fn clone_parent_allows_nonzero_exit_signal() {
        let args = CloneArgs {
            flags: CloneFlags::PARENT,
            exit_signal: SIGCHLD as u64,
            ..Default::default()
        };

        assert!(args.validate().is_ok());
    }

    #[test]
    fn clone_thread_rejects_nonzero_exit_signal() {
        let args = CloneArgs {
            flags: CloneFlags::THREAD | CloneFlags::VM | CloneFlags::SIGHAND,
            exit_signal: SIGCHLD as u64,
            ..Default::default()
        };

        assert!(args.validate().is_err());
    }

    #[test]
    fn clone_thread_rejects_new_pid_namespace() {
        let args = CloneArgs {
            flags: CloneFlags::THREAD | CloneFlags::VM | CloneFlags::SIGHAND | CloneFlags::NEWPID,
            ..Default::default()
        };

        assert!(args.validate().is_err());
    }

    #[test]
    fn clone_new_cgroup_namespace_reaches_runtime_permission_checks() {
        let args = CloneArgs {
            flags: CloneFlags::NEWCGROUP,
            ..Default::default()
        };

        assert!(args.validate().is_ok());
    }
}
