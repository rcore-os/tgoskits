use core::{future::poll_fn, sync::atomic::Ordering, task::Poll};

use ax_errno::{AxError, AxResult};
use ax_hal::uspace::UserContext;
use ax_task::{
    TaskInner, current,
    future::{block_on, interruptible},
};
use starry_process::Pid;
use starry_signal::{SignalInfo, SignalOSAction, SignalSet, Signo};

use super::{AsThread, Thread, do_exit, get_process_data, get_process_group, get_task};

/// Notify the parent (via `child_exit_event`) that a child of theirs changed
/// state (stopped or continued). Used by wait4 with WUNTRACED/WCONTINUED.
fn notify_parent_state_change(thr: &Thread) {
    if let Some(parent) = thr.proc_data.proc.parent()
        && let Ok(parent_data) = get_process_data(parent.pid())
    {
        parent_data.child_exit_event.wake();
    }
}

pub fn check_signals(
    thr: &Thread,
    uctx: &mut UserContext,
    restore_blocked: Option<SignalSet>,
) -> bool {
    let Some((sig, os_action)) = thr.signal.check_signals(uctx, restore_blocked) else {
        return false;
    };

    let signo = sig.signo();
    match os_action {
        SignalOSAction::Terminate => {
            do_exit(signo as i32, true);
        }
        SignalOSAction::CoreDump => {
            // TODO: implement core dump
            do_exit(128 + signo as i32, true);
        }
        SignalOSAction::Stop => {
            // Encode WIFSTOPPED status: (stop_signal << 8) | 0x7F.
            let status = ((signo as u32 as i32) << 8) | 0x7F;
            thr.proc_data.set_stop_status(status);
            notify_parent_state_change(thr);

            // Block until SIGCONT or a fatal signal (SIGKILL) becomes pending.
            // SIGCONT always resumes a stopped process, even if it is in the
            // thread's blocked mask — `send_signal_to_process` falls back to
            // waking any blocked thread so we still see it in the pending set.
            let mut wake_mask = SignalSet::default();
            wake_mask.add(Signo::SIGCONT);
            wake_mask.add(Signo::SIGKILL);
            loop {
                if !(thr.signal.pending() & wake_mask).is_empty() {
                    break;
                }
                // `interruptible` resolves with `Err(Interrupted)` whenever
                // `task.interrupt()` is called — signal delivery always does
                // this, so any incoming signal wakes us to re-check.
                let _ = block_on(interruptible(poll_fn::<(), _>(|_cx| Poll::Pending)));
            }
        }
        SignalOSAction::Continue => {
            // Encode WIFCONTINUED status: magic value 0xFFFF.
            thr.proc_data.set_stop_status(0xFFFF);
            notify_parent_state_change(thr);
        }
        SignalOSAction::Handler => {
            // do nothing
        }
    }
    true
}

pub fn block_next_signal() {
    current()
        .as_thread()
        .skip_next_signal_check
        .store(true, Ordering::SeqCst);
}

pub fn unblock_next_signal() -> bool {
    current()
        .as_thread()
        .skip_next_signal_check
        .swap(false, Ordering::SeqCst)
}

pub fn with_blocked_signals<R>(
    blocked: Option<SignalSet>,
    f: impl FnOnce() -> AxResult<R>,
) -> AxResult<R> {
    let curr = current();
    let sig = &curr.as_thread().signal;

    let old_blocked = blocked.map(|set| sig.set_blocked(set));
    f().inspect(|_| {
        if let Some(old) = old_blocked {
            sig.set_blocked(old);
        }
    })
}

pub(super) fn send_signal_thread_inner(task: &TaskInner, thr: &Thread, sig: SignalInfo) {
    if thr.signal.send_signal(sig) {
        task.interrupt();
    }
}

/// Sends a signal to a thread.
pub fn send_signal_to_thread(tgid: Option<Pid>, tid: Pid, sig: Option<SignalInfo>) -> AxResult<()> {
    let task = get_task(tid)?;
    let thread = task.try_as_thread().ok_or(AxError::OperationNotPermitted)?;
    if tgid.is_some_and(|tgid| thread.proc_data.proc.pid() != tgid) {
        return Err(AxError::NoSuchProcess);
    }

    if let Some(sig) = sig {
        info!("Send signal {:?} to thread {}", sig.signo(), tid);
        send_signal_thread_inner(&task, thread, sig);
    }

    Ok(())
}

/// Sends a signal to a process.
pub fn send_signal_to_process(pid: Pid, sig: Option<SignalInfo>) -> AxResult<()> {
    let proc_data = get_process_data(pid)?;

    if let Some(sig) = sig {
        let signo = sig.signo();
        info!("Send signal {signo:?} to process {pid}");

        // Pre-compute whether the signal is ignored so we can decide after
        // `send_signal` whether to fall back to waking blocked threads.
        let ignored = proc_data.signal.actions.lock()[signo].is_ignore(signo);

        if let Some(tid) = proc_data.signal.send_signal(sig)
            && let Ok(task) = get_task(tid)
        {
            task.interrupt();
        } else if !ignored {
            // The signal is queued but no thread is eligible for immediate
            // delivery (all threads block it). Interrupt every thread so any
            // task blocked in-kernel (e.g. a stopped task waiting on SIGCONT
            // while musl's `raise` has all app signals masked) can re-check
            // its pending set.
            for t in proc_data.proc.threads() {
                if let Ok(task) = get_task(t) {
                    task.interrupt();
                }
            }
        }
    }

    Ok(())
}

/// Sends a signal to a process group.
pub fn send_signal_to_process_group(pgid: Pid, sig: Option<SignalInfo>) -> AxResult<()> {
    let pg = get_process_group(pgid)?;

    if let Some(sig) = sig {
        info!("Send signal {:?} to process group {}", sig.signo(), pgid);
        for proc in pg.processes() {
            send_signal_to_process(proc.pid(), Some(sig.clone()))?;
        }
    }

    Ok(())
}

/// Sends a fatal signal to the current process.
pub fn raise_signal_fatal(sig: SignalInfo) -> AxResult<()> {
    let curr = current();
    let proc_data = &curr.as_thread().proc_data;

    let signo = sig.signo();
    info!("Send fatal signal {signo:?} to the current process");
    if let Some(tid) = proc_data.signal.send_signal(sig)
        && let Ok(task) = get_task(tid)
    {
        task.interrupt();
    } else {
        // No task wants to handle the signal, abort the task
        do_exit(signo as i32, true);
    }

    Ok(())
}
