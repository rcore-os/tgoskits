use alloc::sync::Arc;
#[cfg(axtest)]
use alloc::task::Wake;
#[cfg(axtest)]
use core::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use bitflags::bitflags;
use linux_raw_sys::general::{SI_TKILL, SI_USER};
use starry_signal::{SignalInfo, Signo};

use crate::{
    Errno, StarryError, StarryResult,
    file::{FD_TABLE, FileLike, PidFd, add_file_like, current_fd_table},
    mm::VmPtr,
    syscall::signal::check_kill_permission_identity,
    task::{
        PidIdentity, PidView, Tgid, TgidNumber, Tid, TidNumber, UserTaskRef,
        send_signal_to_process_data, send_signal_to_process_group_ref, send_signal_to_task,
    },
};

bitflags! {
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PidFdFlags: u32 {
        const NONBLOCK = 2048;
        const THREAD = 128;
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, Default)]
    struct PidFdSignalFlags: u32 {
        const THREAD = 1 << 0;
        const THREAD_GROUP = 1 << 1;
        const PROCESS_GROUP = 1 << 2;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PidFdSignalScope {
    Thread,
    ThreadGroup,
    ProcessGroup,
}

enum PidFdOpenTarget {
    Process(TgidNumber),
    Thread(TidNumber),
}

#[cfg(axtest)]
struct PidfdWakeCounter(AtomicUsize);

#[cfg(axtest)]
impl Wake for PidfdWakeCounter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
    }
}

impl PidFdOpenTarget {
    fn parse(pid: u32, flags: PidFdFlags) -> StarryResult<Self> {
        if (pid as i32) <= 0 {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(PidFdFlags::THREAD) {
            Ok(Self::Thread(TidNumber::try_from(pid)?))
        } else {
            Ok(Self::Process(TgidNumber::try_from(pid)?))
        }
    }
}

fn parse_signo(signo: u32) -> StarryResult<Signo> {
    Signo::from_repr(signo as u8).ok_or(StarryError::InvalidInput)
}

fn make_pidfd_siginfo(
    current: &crate::task::UserTaskRef,
    signo: Signo,
    scope: PidFdSignalScope,
) -> SignalInfo {
    let code = if scope == PidFdSignalScope::Thread {
        SI_TKILL
    } else {
        SI_USER as _
    };
    let curr = current;
    let thread = curr.as_thread();
    let sender = PidView::new(thread.active_pid_namespace())
        .visible_number(&thread.proc_data.identity())
        .expect("current process is visible in its active PID namespace")
        .get();
    SignalInfo::new_user(signo, code, sender, thread.cred().uid)
}

fn open_thread_pidfd_with<T>(
    identity: Arc<PidIdentity>,
    tid: TidNumber,
    new_live: impl FnOnce(Arc<PidIdentity>, UserTaskRef, TidNumber) -> T,
    new_detached: impl FnOnce(Arc<PidIdentity>, Arc<PidIdentity>, TidNumber) -> T,
) -> StarryResult<T> {
    if !identity.has_role::<Tid>() {
        return Err(StarryError::NoSuchProcess);
    }
    if let Some(task) = identity.live_task() {
        let identity = task.as_thread().pid_identity();
        Ok(new_live(identity, task, tid))
    } else {
        let process_identity = identity.thread_pidfd_process_identity()?;
        Ok(new_detached(identity, process_identity, tid))
    }
}

pub fn sys_pidfd_open(
    current: &crate::task::UserTaskRef,
    pid: u32,
    flags: u32,
) -> crate::StarryResult<isize> {
    debug!("sys_pidfd_open <= pid: {pid}, flags: {flags}");

    let flags = PidFdFlags::from_bits(flags).ok_or(StarryError::InvalidInput)?;
    let view = PidView::new(current.as_thread().active_pid_namespace());

    let fd = match PidFdOpenTarget::parse(pid, flags)? {
        PidFdOpenTarget::Thread(tid) => {
            let identity = view.resolve_identity(tid.pid_number())?;
            open_thread_pidfd_with(
                identity,
                tid,
                |identity, task, tid| PidFd::new_thread(identity, task.as_thread(), tid),
                PidFd::new_detached_thread,
            )?
        }
        PidFdOpenTarget::Process(tgid) => {
            let identity = view.resolve_identity(tgid.pid_number())?;
            // Linux first requires a PIDTYPE_PID task, then checks whether it
            // is also a thread-group leader. Other PID roles may keep the
            // numeric slot published after the task itself has been reaped.
            if !identity.has_role::<Tid>() {
                return Err(StarryError::NoSuchProcess);
            }
            // Without PIDFD_THREAD the target must be a thread-group leader.
            if !identity.has_role::<Tgid>() {
                return Err(Errno::ENOENT.into());
            }
            identity.public_process()?;
            PidFd::new_process(identity)
        }
    };
    if flags.contains(PidFdFlags::NONBLOCK) {
        fd.set_nonblocking(true)?;
    }

    fd.add_to_fd_table(true).map(|fd| fd as _)
}

pub fn sys_pidfd_getfd(
    current: &crate::task::UserTaskRef,
    pidfd: i32,
    target_fd: i32,
    flags: u32,
) -> crate::StarryResult<isize> {
    debug!("sys_pidfd_getfd <= pidfd: {pidfd}, target_fd: {target_fd}, flags: {flags}");

    if flags != 0 {
        return Err(StarryError::InvalidInput);
    }

    let pidfd = PidFd::from_fd(pidfd)?;
    let proc_data = pidfd.process_data()?;
    let curr_proc_data = current.as_thread().proc_data.clone();
    let is_current = Arc::ptr_eq(&proc_data, &curr_proc_data);
    if !is_current {
        // Linux __pidfd_fget() uses ptrace_may_access(PTRACE_MODE_ATTACH_REALCREDS).
        // Until Starry has that, require at least kill-style credentials on the target.
        check_kill_permission_identity(current, &proc_data.identity())?;
    }
    let fd_entry = if is_current {
        // Use the calling thread's live fd table, including any table installed
        // by unshare(CLONE_FILES) or close_range(CLOSE_RANGE_UNSHARE).
        current_fd_table().read().get(target_fd as usize).cloned()
    } else {
        let task = pidfd
            .process_identity()
            .live_task()
            .ok_or(StarryError::NoSuchProcess)?;
        task.as_thread()
            .clone_scope_item(&FD_TABLE)
            .read()
            .get(target_fd as usize)
            .cloned()
    };
    fd_entry
        .ok_or(StarryError::BadFileDescriptor)
        .and_then(|fd| {
            let fd = add_file_like(fd.inner.clone(), true)?;
            Ok(fd as isize)
        })
}

pub fn sys_pidfd_send_signal(
    current: &crate::task::UserTaskRef,
    pidfd: i32,
    signo: u32,
    sig: *mut SignalInfo,
    flags: u32,
) -> StarryResult<isize> {
    let flags = PidFdSignalFlags::from_bits(flags).ok_or(StarryError::InvalidInput)?;
    if flags.bits().count_ones() > 1 {
        return Err(StarryError::InvalidInput);
    }

    let pidfd_obj = PidFd::from_fd(pidfd)?;
    let target_process = pidfd_obj.process_identity();

    let scope = if flags.contains(PidFdSignalFlags::THREAD)
        || (flags.is_empty() && pidfd_obj.is_thread())
    {
        PidFdSignalScope::Thread
    } else if flags.contains(PidFdSignalFlags::PROCESS_GROUP) {
        PidFdSignalScope::ProcessGroup
    } else {
        PidFdSignalScope::ThreadGroup
    };

    let kinfo = if signo == 0 {
        None
    } else if sig.is_null() {
        let signo = parse_signo(signo)?;
        Some(make_pidfd_siginfo(current, signo, scope))
    } else {
        let signo_parsed = parse_signo(signo)?;
        let info = unsafe { sig.vm_read_uninit(current)?.assume_init() };
        if info.signo() != signo_parsed {
            return Err(StarryError::InvalidInput);
        }
        if !Arc::ptr_eq(&current.as_thread().proc_data.identity(), &target_process)
            && (info.code() >= 0 || info.code() == SI_TKILL)
        {
            return Err(StarryError::OperationNotPermitted);
        }
        Some(info)
    };

    match scope {
        PidFdSignalScope::Thread => {
            check_kill_permission_identity(current, &target_process)?;
            if pidfd_obj.is_zombie() {
                return Ok(0);
            }
            let task = pidfd_obj.signal_thread()?;
            send_signal_to_task(&task, Some(target_process), kinfo)?;
        }
        PidFdSignalScope::ThreadGroup => {
            check_kill_permission_identity(current, &target_process)?;
            if let Some(proc_data) = target_process.live_data() {
                send_signal_to_process_data(&proc_data, kinfo)?;
            } else if !target_process.is_zombie() {
                return Err(StarryError::NoSuchProcess);
            }
        }
        PidFdSignalScope::ProcessGroup => {
            let process = pidfd_obj.signal_process()?;
            check_kill_permission_identity(current, &target_process)?;
            send_signal_to_process_group_ref(&process.group(), kinfo)?;
        }
    }

    Ok(0)
}

#[cfg(any(test, axtest))]
pub(crate) fn pidfd_flags_and_signal_validation_rules_hold_for_test() -> bool {
    // Test PidFdFlags validation
    let valid_flags = 0u32;
    assert!(PidFdFlags::from_bits(valid_flags).is_some());

    let nonblock_only = 2048u32;
    assert!(PidFdFlags::from_bits(nonblock_only).is_some());

    let thread_only = 128u32;
    assert!(PidFdFlags::from_bits(thread_only).is_some());

    let all_valid = 2048u32 | 128u32;
    assert!(PidFdFlags::from_bits(all_valid).is_some());

    // Invalid flag should return None
    let invalid_flags = 0xFFFF;
    assert!(PidFdFlags::from_bits(invalid_flags).is_none());

    // Test parse_signo
    assert!(parse_signo(1).is_ok()); // SIGHUP
    assert!(parse_signo(9).is_ok()); // SIGKILL
    assert!(parse_signo(0).is_err()); // Invalid signo
    assert!(parse_signo(255).is_err()); // Out of range

    true
}

#[cfg(axtest)]
pub(crate) fn pidfd_thread_exit_window_matches_linux_for_test() -> bool {
    let namespace = crate::task::new_test_pid_namespace();
    let (process_identity, tgid_lease) = crate::task::new_test_process_identity(&namespace);
    let exit_flag = Arc::new(core::sync::atomic::AtomicBool::new(false));
    let (identity, tid_lease) =
        crate::task::new_test_thread_identity(&namespace, &process_identity, exit_flag.clone());
    let tid = TidNumber::from(identity.root_number());
    let exit_path = identity.mark_task_exited().retain_tid(tid_lease);
    let view = PidView::new(namespace);

    let fd = view
        .resolve_identity(tid.pid_number())
        .and_then(|identity| {
            open_thread_pidfd_with(
                identity,
                tid,
                |identity, task, tid| PidFd::new_thread(identity, task.as_thread(), tid),
                PidFd::new_detached_thread,
            )
        });
    let Ok(fd) = fd else {
        return false;
    };
    let unreadable_before_exit = !axpoll::Pollable::poll(&fd).contains(axpoll::IoEvents::IN);
    let wake_counter = Arc::new(PidfdWakeCounter(AtomicUsize::new(0)));
    let waker = core::task::Waker::from(wake_counter.clone());
    let mut registrar = axpoll::PollRegistrar::<axpoll::SharedObserver>::new(&waker);
    unsafe {
        axpoll::Pollable::register_shared(
            &fd,
            &mut registrar,
            axpoll::IoEvents::IN | axpoll::IoEvents::HUP,
        )
    };
    exit_flag.store(true, core::sync::atomic::Ordering::Release);
    identity.notify_thread_pidfd_exit();
    let readable_after_exit = axpoll::Pollable::poll(&fd).contains(axpoll::IoEvents::IN);
    let exit_woke_waiter = wake_counter.0.load(AtomicOrdering::Relaxed) > 0;

    let wakes_before_release = wake_counter.0.load(AtomicOrdering::Relaxed);
    registrar.reset(&waker);
    unsafe { axpoll::Pollable::register_shared(&fd, &mut registrar, axpoll::IoEvents::HUP) };
    exit_path.complete();
    let identity_released = view.resolve_identity(tid.pid_number()).is_err();
    let released_fd_hangs_up = axpoll::Pollable::poll(&fd).contains(axpoll::IoEvents::HUP);
    let release_woke_waiter = wake_counter.0.load(AtomicOrdering::Relaxed) > wakes_before_release;
    process_identity.mark_task_exited().complete();
    tgid_lease.release();

    unreadable_before_exit
        && readable_after_exit
        && exit_woke_waiter
        && identity_released
        && released_fd_hangs_up
        && release_woke_waiter
}
