use alloc::sync::Arc;
use core::mem::size_of;

use ax_fs_ng::vfs::FS_CONTEXT;
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_task::{AxTaskExt, current, spawn_task_with};
use bitflags::bitflags;
use linux_raw_sys::general::*;
use scope_local::Scope;
use starry_signal::Signo;
use starry_vm::VmMutPtr;

use crate::{
    StarryError, StarryResult,
    file::{FD_TABLE, PidFd, PreparedFileDescriptor, prepare_file_like},
    mm::{MmHandle, copy_from_kernel},
    sync::SpinLock,
    task::{
        AsThread, PidIdentity, PidReservation, PidReservationKind, Process, ProcessData,
        ProcessDataInit, ProcessImage, Tgid, Thread, Tid, TidNumber, add_task_to_table,
        new_user_task,
    },
};

/// Rolls back prepared topology and fd-table changes if clone fails before spawn.
///
/// The separate [`PidReservation`] still owns every namespace number until the
/// final, non-failing commit phase, so this transaction never has to undo an
/// externally visible PID publication.
struct CloneTransaction {
    identity: Arc<PidIdentity>,
    process: Option<Arc<Process>>,
    committed: bool,
}

impl CloneTransaction {
    fn new(identity: Arc<PidIdentity>) -> Self {
        Self {
            identity,
            process: None,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CloneTransaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(process) = self.process.take() {
            process.retire();
        }
        self.identity.abort_failed_task_publication();
    }
}

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
ax_tracepoint::define_event_trace!(
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

fn emit_sched_process_fork(parent_tid: TidNumber, child_tid: TidNumber) {
    trace_sched_process_fork(parent_tid.get() as u64, child_tid.get() as u64);
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
    fn validate(&self) -> StarryResult<()> {
        let Self {
            flags, exit_signal, ..
        } = self;

        if *exit_signal > 0 && flags.contains(CloneFlags::THREAD) {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(CloneFlags::THREAD)
            && !flags.contains(CloneFlags::VM | CloneFlags::SIGHAND)
        {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(CloneFlags::SIGHAND) && !flags.contains(CloneFlags::VM) {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(CloneFlags::VFORK | CloneFlags::THREAD) {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(CloneFlags::PIDFD | CloneFlags::DETACHED) {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(CloneFlags::NEWNS | CloneFlags::FS) {
            return Err(StarryError::InvalidInput);
        }
        // A thread must remain in the PID namespace of its thread group.
        // CLONE_PARENT only changes parentage, so Linux permits it with
        // CLONE_NEWPID. clone3 separately requires a zero exit signal when
        // CLONE_PARENT is present.
        if flags.contains(CloneFlags::NEWPID | CloneFlags::THREAD) {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(CloneFlags::INTO_CGROUP | CloneFlags::THREAD) {
            return Err(StarryError::InvalidInput);
        }

        Ok(())
    }

    fn validate_cgroup_target(&self, has_requested_cgroup: bool) -> StarryResult<()> {
        self.validate()?;
        if self.flags.contains(CloneFlags::INTO_CGROUP) != has_requested_cgroup {
            return Err(StarryError::InvalidInput);
        }
        Ok(())
    }

    pub fn do_clone(self, uctx: &UserContext) -> StarryResult<isize> {
        self.do_clone_in_cgroup(uctx, None)
    }

    pub(super) fn do_clone_in_cgroup(
        self,
        uctx: &UserContext,
        requested_cgroup: Option<Arc<ax_cgroup::CgroupNode>>,
    ) -> StarryResult<isize> {
        self.validate_cgroup_target(requested_cgroup.is_some())?;

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
            Some(Signo::from_repr(exit_signal as u8).ok_or(StarryError::InvalidInput)?)
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

        if flags.contains(CloneFlags::PARENT_SETTID) && parent_tid_ptr != 0 {
            crate::mm::prepare_user_write(parent_tid_ptr, size_of::<u32>())?;
        }
        if flags.contains(CloneFlags::PIDFD) && pidfd != 0 {
            crate::mm::prepare_user_write(pidfd, size_of::<i32>())?;
        }

        let curr = current();
        let curr_thread = curr.as_thread();
        let old_proc_data = &curr_thread.proc_data;
        if flags.contains(CloneFlags::NEWCGROUP) && !curr_thread.cred().has_cap_sys_admin() {
            return Err(StarryError::OperationNotPermitted);
        }

        let mut new_task = new_user_task(&curr.name(), new_uctx, set_child_tid)?;
        #[cfg(target_arch = "riscv64")]
        {
            let mut fp_state = ax_cpu::FpState::default();
            fp_state.save();
            fp_state.fs = child_fp_fs;
            new_task.ctx_mut().fp_state = fp_state;
        }

        let parent_pid_ns = curr_thread.active_pid_namespace();
        let target_pid_ns = if flags.contains(CloneFlags::THREAD) {
            parent_pid_ns.clone()
        } else if flags.contains(CloneFlags::NEWPID) {
            crate::namespace::PidNamespace::new_child(parent_pid_ns.clone())
        } else {
            old_proc_data.nsproxy.lock().pid_ns_for_children.clone()
        };
        let reservation_kind = if flags.contains(CloneFlags::THREAD) {
            PidReservationKind::Thread
        } else {
            PidReservationKind::ProcessLeader
        };
        let reservation = PidReservation::reserve(&target_pid_ns, reservation_kind)?;
        let root_tid = TidNumber::from(
            reservation
                .number_in(&crate::task::ROOT_PID_NS)
                .ok_or(StarryError::BadState)?,
        );
        let parent_visible_tid = TidNumber::from(
            reservation
                .number_in(&parent_pid_ns)
                .ok_or(StarryError::BadState)?,
        );
        let identity = reservation.identity();
        let tid_lease = identity.acquire_role::<Tid>()?;
        let mut tgid_lease = (!flags.contains(CloneFlags::THREAD))
            .then(|| identity.acquire_role::<Tgid>())
            .transpose()?;
        let mut clone_transaction = CloneTransaction::new(identity.clone());

        let child_kind = if flags.contains(CloneFlags::THREAD) {
            ax_cgroup::CgroupChildKind::Thread
        } else {
            ax_cgroup::CgroupChildKind::Process
        };
        let mut cgroup_guard = match (child_kind, requested_cgroup) {
            (ax_cgroup::CgroupChildKind::Process, Some(target)) => {
                crate::cgroup::begin_process_at(target, &identity)?
            }
            (kind, None) => crate::cgroup::begin_task(&old_proc_data.identity(), &identity, kind)?,
            (ax_cgroup::CgroupChildKind::Thread, Some(_)) => {
                unreachable!("CLONE_INTO_CGROUP with CLONE_THREAD passed validation")
            }
        };
        let child_cgroup = cgroup_guard.cgroup();
        let mut prepared_nsproxy = (!flags.contains(CloneFlags::THREAD)).then(|| {
            let mut nsproxy = old_proc_data.nsproxy.lock().clone_all();
            if flags.contains(CloneFlags::NEWUTS) {
                nsproxy.unshare_uts();
            }
            if flags.contains(CloneFlags::NEWIPC) {
                nsproxy.unshare_ipc();
            }
            if flags.contains(CloneFlags::NEWNS) {
                nsproxy.unshare_mnt();
            }
            if flags.contains(CloneFlags::NEWNET) {
                nsproxy.unshare_net();
            }
            if flags.contains(CloneFlags::NEWUSER) {
                nsproxy.unshare_user();
            }
            if flags.contains(CloneFlags::NEWCGROUP) {
                nsproxy.unshare_cgroup(child_cgroup.clone());
            }
            if flags.contains(CloneFlags::NEWPID) {
                nsproxy.pid_ns_for_children = target_pid_ns.clone();
            }
            nsproxy
        });

        let new_proc_data = if flags.contains(CloneFlags::THREAD) {
            old_proc_data.clone()
        } else {
            let proc = if flags.contains(CloneFlags::PARENT) {
                old_proc_data
                    .proc
                    .parent()
                    .ok_or(StarryError::InvalidInput)?
            } else {
                old_proc_data.proc.clone()
            }
            .fork(identity.clone());
            clone_transaction.process = Some(proc.clone());

            let aspace = if flags.contains(CloneFlags::VM) {
                old_proc_data
                    .clone_aspace_user_ref()
                    .map_err(|_| StarryError::InvalidInput)?
            } else {
                let parent_mm = old_proc_data.pin_aspace()?;
                let aspace = parent_mm.lock().try_clone()?;
                copy_from_kernel(&mut aspace.lock())?;
                MmHandle::from_arc(aspace).map_err(|_| StarryError::BadState)?
            };
            let signal_actions = if flags.contains(CloneFlags::SIGHAND) {
                old_proc_data.signal.actions()
            } else if flags.contains(CloneFlags::CLEAR_SIGHAND) {
                Arc::new(SpinLock::new(Default::default()))
            } else {
                Arc::new(SpinLock::new(
                    old_proc_data.signal.actions().lock_irqsave().clone(),
                ))
            };

            // RwLock read guards used as nested call arguments live until the
            // outer statement ends. Build the plain image first so all six
            // preemption guards are gone before `ProcessData::new` acquires
            // the sleepable address-space mutex.
            let process_image = ProcessImage::new(
                old_proc_data.exe_path.read().clone(),
                old_proc_data.cmdline.read().clone(),
                old_proc_data.envp.read().clone(),
                old_proc_data.auxv.read().clone(),
                old_proc_data.root_path.read().clone(),
                old_proc_data.cwd_path.read().clone(),
            );
            let proc_data = ProcessData::new(
                proc,
                identity.clone(),
                tgid_lease
                    .take()
                    .expect("process clone must own one TGID lease"),
                ProcessDataInit {
                    image: process_image,
                    aspace,
                    signal_actions,
                    exit_signal,
                    wait_parent_tid: curr_thread.tid_number(),
                },
            );
            proc_data.set_umask(old_proc_data.umask());
            proc_data.set_nice(old_proc_data.nice());
            *proc_data.cgroup.write() = child_cgroup.clone();
            proc_data.replace_personality(old_proc_data.personality());
            // Inherit parent dumpable (PR_SET_DUMPABLE state). Linux: child
            // fork/clone copies mm->dumpable from parent; without this, a
            // child of `prctl(PR_SET_DUMPABLE, 0) -> fork()` would reset to
            // SUID_DUMP_USER (1), breaking the safety semantics this PR is
            // supposed to enforce. Verified via Linux host: parent sets 0,
            // fork child PR_GET_DUMPABLE returns 0.
            proc_data.set_dumpable(old_proc_data.dumpable());
            proc_data.set_transparent_huge_page_mode(
                old_proc_data.transparent_huge_page_mode(),
            )?;

            *proc_data.nsproxy.lock() = prepared_nsproxy
                .take()
                .expect("process clone must prepare one namespace proxy");

            proc_data
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

        // Reserve pids before publishing the new TID in its thread group.
        new_proc_data.proc.add_thread(root_tid);

        let parent_cred = Some(curr_thread.cred());
        let thr = Thread::new(
            identity.clone(),
            tid_lease,
            new_proc_data.clone(),
            parent_cred,
            curr_thread.signal.blocked(),
            scope,
        );
        if curr_thread.no_new_privs() {
            thr.set_no_new_privs();
        }
        thr.set_seccomp_state(curr_thread.seccomp_state());
        if flags.contains(CloneFlags::CHILD_CLEARTID) {
            thr.set_clear_child_tid(child_tid);
        }
        let mut prepared_pidfd: Option<PreparedFileDescriptor> = None;
        let mut pidfd_copyout = None;
        if flags.contains(CloneFlags::PIDFD) && pidfd != 0 {
            // The pidfd and later namespace publication share the prepared
            // identity. Until the final commit, PID-number lookup cannot see it.
            let pidfd_obj = if flags.contains(CloneFlags::THREAD) {
                PidFd::new_thread(identity.clone(), &thr, root_tid)
            } else {
                PidFd::new_process(identity.clone())
            };
            let prepared = prepare_file_like(Arc::new(pidfd_obj), true)?;
            let fd = prepared.fd();
            prepared_pidfd = Some(prepared);
            pidfd_copyout = Some((pidfd as *mut i32, fd));
        }
        // perf: clone any `attr.inherit` event from the parent onto the child so
        // `perf record` follows it. Done before the child is scheduled (it is not
        // yet spawned) so the counter is present the first time the child runs.
        #[cfg(target_arch = "aarch64")]
        crate::perf::task::on_clone_inherit(curr_thread, &thr);
        *new_task.task_ext_mut() = Some(AxTaskExt::from_impl(thr));

        // vfork(2) and clone(CLONE_VFORK) must sleep the parent until the child
        // execs or exits. Use PollSet so the parent's wait remains
        // interruptible by task.interrupt().
        if needs_vfork_block {
            let poll = Arc::new(axpoll::PollSet::new());
            new_proc_data.set_vfork_done(poll);
        }

        if let Some((pidfd_ptr, fd)) = pidfd_copyout {
            pidfd_ptr.vm_write(fd)?;
        }
        // All fallible resource setup and aborting user-memory writes are
        // complete. Commit the namespace binding chain before installing the
        // reserved pidfd or exposing parent_tid; everything below is
        // deliberately infallible.
        let published_identity = reservation.publish()?;
        debug_assert!(Arc::ptr_eq(&published_identity, &identity));
        if let Some(pidfd) = prepared_pidfd.take() {
            pidfd.install();
        }
        if flags.contains(CloneFlags::PARENT_SETTID) && parent_tid_ptr != 0 {
            // Linux performs this copyout after the child is visible and does
            // not roll the child back if a concurrent unmap makes it fail.
            let _ = (parent_tid_ptr as *mut u32).vm_write(parent_visible_tid.get());
        }

        let parent_pid = curr.as_thread().proc_data.proc.pid_number();
        // The user-visible tid, not the scheduler id: they diverge for the init
        // process (pid/tid pinned to 1, scheduler id higher). Signal delivery
        // and ptrace below look this up in the tid-keyed task table.
        let parent_tid = curr.as_thread().tid_number();
        let ptrace_event = if flags.contains(CloneFlags::THREAD) {
            super::ptrace::PTRACE_EVENT_CLONE
        } else if flags.contains(CloneFlags::VFORK) {
            super::ptrace::PTRACE_EVENT_VFORK
        } else {
            super::ptrace::PTRACE_EVENT_FORK
        };
        let trace_clone =
            super::ptrace::ptrace_notify_clone(parent_pid, parent_tid, &identity, ptrace_event);
        if trace_clone && let Some(tracer) = curr.as_thread().proc_data.ptrace_tracer_identity() {
            if !flags.contains(CloneFlags::THREAD) {
                new_proc_data.set_ptrace_tracer(&tracer);
                let attach_mode = if curr.as_thread().proc_data.is_ptrace_seized() {
                    crate::task::PtraceAttachMode::Seize
                } else {
                    crate::task::PtraceAttachMode::Attach
                };
                new_proc_data.set_ptrace_attach_mode(attach_mode);
            }
            new_proc_data.set_ptrace_stop(root_tid, starry_signal::Signo::SIGSTOP, &new_uctx);
        }

        cgroup_guard.commit();
        spawn_task_with(new_task, add_task_to_table);
        clone_transaction.commit();

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
        emit_sched_process_fork(curr_thread.tid(), root_tid);

        // perf side-band: tell any `attr.task` event watching the parent that it
        // forked a child (PERF_RECORD_FORK), so `perf record` can account it.
        // Emitted before any vfork-wait below, in the parent's context.
        #[cfg(target_arch = "aarch64")]
        crate::perf::task::on_clone_sideband(
            curr.as_thread(),
            &new_proc_data.identity(),
            &identity,
        );

        // Block the parent until the child exec's or exits.
        if needs_vfork_block {
            new_proc_data.wait_vfork_done();
            let _ = super::ptrace::ptrace_notify_vfork_done(parent_pid, parent_tid, &identity);
        }

        Ok(parent_visible_tid.get() as _)
    }
}

ax_tracepoint::define_event_trace!(
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
) -> StarryResult<isize> {
    const FLAG_MASK: u32 = 0xff;
    let clone_flags = CloneFlags::from_bits_truncate((flags & !FLAG_MASK) as u64);
    let exit_signal = (flags & FLAG_MASK) as u64;

    trace_sys_clone(clone_flags.bits() as _, stack, parent_tid);

    if clone_flags.contains(CloneFlags::PIDFD | CloneFlags::PARENT_SETTID) {
        return Err(StarryError::InvalidInput);
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
pub fn sys_fork(uctx: &UserContext) -> StarryResult<isize> {
    sys_clone(uctx, SIGCHLD, 0, 0, 0, 0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_vfork(uctx: &UserContext) -> StarryResult<isize> {
    let flags = (CloneFlags::VFORK | CloneFlags::VM).bits() as u32 | SIGCHLD;
    sys_clone(uctx, flags, 0, 0, 0, 0)
}

#[cfg(all(test, not(axtest)))]
fn clone_validation_rules_hold_for_test() -> bool {
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
    let thread_with_newpid_rejected = CloneArgs {
        flags: CloneFlags::THREAD | CloneFlags::VM | CloneFlags::SIGHAND | CloneFlags::NEWPID,
        ..Default::default()
    }
    .validate()
    .is_err();
    let thread_with_into_cgroup_rejected = CloneArgs {
        flags: CloneFlags::THREAD | CloneFlags::VM | CloneFlags::SIGHAND | CloneFlags::INTO_CGROUP,
        ..Default::default()
    }
    .validate_cgroup_target(true)
    .is_err();
    let into_cgroup_without_target_rejected = CloneArgs {
        flags: CloneFlags::INTO_CGROUP,
        ..Default::default()
    }
    .validate_cgroup_target(false)
    .is_err();
    let unexpected_target_rejected = CloneArgs::default().validate_cgroup_target(true).is_err();
    let into_cgroup_with_target_allowed = CloneArgs {
        flags: CloneFlags::INTO_CGROUP,
        ..Default::default()
    }
    .validate_cgroup_target(true)
    .is_ok();
    let legacy_parent_newpid_allowed = CloneArgs {
        flags: CloneFlags::PARENT | CloneFlags::NEWPID,
        exit_signal: SIGCHLD as u64,
        ..Default::default()
    }
    .validate()
    .is_ok();
    // Cover the remaining validation arms in the host unit suite.
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
        && thread_with_newpid_rejected
        && thread_with_into_cgroup_rejected
        && into_cgroup_without_target_rejected
        && unexpected_target_rejected
        && into_cgroup_with_target_allowed
        && legacy_parent_newpid_allowed
        && thread_without_vm_sighand_rejected
        && vfork_with_thread_rejected
        && pidfd_with_detached_rejected
        && newcgroup_allowed
        && minimal_valid
        && thread_valid
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use linux_raw_sys::general::SIGCHLD;

    use super::{CloneArgs, CloneFlags, clone_validation_rules_hold_for_test};

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
    fn legacy_clone_parent_allows_new_pid_namespace() {
        let args = CloneArgs {
            flags: CloneFlags::PARENT | CloneFlags::NEWPID,
            exit_signal: SIGCHLD as u64,
            ..Default::default()
        };

        assert!(args.validate().is_ok());
    }

    #[test]
    fn clone_thread_rejects_into_cgroup() {
        let args = CloneArgs {
            flags: CloneFlags::THREAD
                | CloneFlags::VM
                | CloneFlags::SIGHAND
                | CloneFlags::INTO_CGROUP,
            ..Default::default()
        };

        assert!(args.validate_cgroup_target(true).is_err());
    }

    #[test]
    fn clone_into_cgroup_requires_exactly_one_resolved_target() {
        let args = CloneArgs {
            flags: CloneFlags::INTO_CGROUP,
            ..Default::default()
        };
        assert!(args.validate_cgroup_target(false).is_err());
        assert!(args.validate_cgroup_target(true).is_ok());

        assert!(CloneArgs::default().validate_cgroup_target(true).is_err());
    }

    #[test]
    fn clone_validation_rules_hold() {
        assert!(clone_validation_rules_hold_for_test());
    }
}
